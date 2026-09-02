//! The preflight doctor (task 018; ADR-0004, ADR-0012, seam-contract D11, D16,
//! D22).
//!
//! Eight checks, each corresponding to one way an overnight queue can waste a
//! night. That is the criterion for adding a ninth, and it is the task file's
//! own: a check that does not map to a wasted night is noise on a panel the
//! user is meant to read.
//!
//! # It is a preflight, not a watchdog
//!
//! This module runs at three moments and no others: when the window opens, when
//! the user presses Re-check, and immediately before the queue is told to start
//! — [`QueueHandle::start`](crate::scheduler::QueueHandle::start), which is also
//! what task 013's scheduled start will call before flipping the switch.
//!
//! It deliberately does **not** run inside
//! [`scheduler::queue`](crate::scheduler::queue)'s step loop. That loop wakes on
//! every change event, so a doctor there would be eight subprocess spawns per
//! card drag. The one check that genuinely must happen per step is already there
//! and stays there: `probe_cli` before the claim, for task 008's stated reason.
//! An environment that breaks *after* the queue started is caught by that probe
//! (for `claude`) and by the run itself (for everything else). Seam-contract D22
//! records this so it is not later "improved" into a watchdog.
//!
//! # Every check is a function over injected inputs
//!
//! No check discovers its own environment. Program paths arrive in
//! [`Programs`], the data directory in [`AppPaths`], the repository list from
//! [`repo::list`], and the MCP endpoint is **read** off the live
//! [`RunHandles`](crate::mcp::RunHandles) rather than re-bound — re-binding to
//! see whether a port is free would race the server that is already holding it.
//! That is what makes the checks testable against a `TempDir` and a stand-in
//! binary instead of against whatever the developer happens to have installed.

/// Public so each check can be tested as the function over injected inputs it
/// is — a `TempDir` and a renamed binary — rather than only through [`run`],
/// which would make every test depend on whatever the machine running it
/// happens to have installed.
pub mod checks;

use std::path::PathBuf;

use serde::Serialize;

use crate::context::ServiceContext;
use crate::error::Result;
use crate::mcp::{self, RunHandles};
use crate::paths::AppPaths;
use crate::repo;
use crate::runner::process::CLAUDE_CLI;
use crate::runner::RunnerConfig;

pub use checks::{
    MINIMUM_CLAUDE_VERSION, MINIMUM_GIT_VERSION, ROOMY_DISK_BYTES, USABLE_DISK_BYTES,
};

/// Which check a [`CheckResult`] came from.
///
/// An identity, not a label: the frontend groups rows by it and the welcome
/// flow shows each step only the rows belonging to it, neither of which can be
/// done by matching on prose. [`Check::label`] is the prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    ClaudeCli,
    ClaudeAuthenticated,
    Git,
    GitHubCli,
    DataDirectory,
    DiskSpace,
    RepositoryPath,
    McpPort,
}

impl Check {
    /// Every check, in the order a report lists them — prerequisites first,
    /// then storage, then what is registered in this installation. The order is
    /// fixed here rather than left to whatever order [`run`] happens to await
    /// in, so the panel does not reshuffle between two runs of the doctor.
    pub const ALL: [Check; 8] = [
        Check::ClaudeCli,
        Check::ClaudeAuthenticated,
        Check::Git,
        Check::GitHubCli,
        Check::DataDirectory,
        Check::DiskSpace,
        Check::RepositoryPath,
        Check::McpPort,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Check::ClaudeCli => "claude_cli",
            Check::ClaudeAuthenticated => "claude_authenticated",
            Check::Git => "git",
            Check::GitHubCli => "github_cli",
            Check::DataDirectory => "data_directory",
            Check::DiskSpace => "disk_space",
            Check::RepositoryPath => "repository_path",
            Check::McpPort => "mcp_port",
        }
    }

    /// What the row is called on screen.
    pub const fn label(self) -> &'static str {
        match self {
            Check::ClaudeCli => "Claude Code CLI",
            Check::ClaudeAuthenticated => "Claude Code sign-in",
            Check::Git => "git",
            Check::GitHubCli => "GitHub CLI",
            Check::DataDirectory => "App data directory",
            Check::DiskSpace => "Free disk space",
            Check::RepositoryPath => "Registered repository",
            Check::McpPort => "MCP server",
        }
    }
}

/// Pass, warn, or fail — and only [`Fail`](CheckStatus::Fail) blocks the queue.
///
/// The assignment per check is argued in seam-contract D22 rather than chosen
/// per call site, because "does this block the user out of their own queue" is
/// exactly the kind of decision two agents would answer differently. The two
/// that most look like mistakes, so they are stated here too: a `claude` older
/// than the pinned minimum is a **Warn**, because it may well still work and
/// locking someone out of their queue over a version string is worse than the
/// risk; a `git` too old for worktrees is a **Fail**, because worktree creation
/// is not optional and every run would die at the same place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    pub const fn is_blocking(self) -> bool {
        matches!(self, CheckStatus::Fail)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
        }
    }
}

/// One row of the report.
///
/// `remediation` is `Option` because a passing row has nothing to remedy, and
/// it is never a generic sentence: the whole value of this module is that
/// "install Claude Code, then press Re-check" is actionable at 6pm where "check
/// your setup" is not. A `Warn` or `Fail` without a remediation is a bug, which
/// [`checks::every_failing_result_carries_a_specific_remediation`] enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub check: Check,
    /// [`Check::label`], carried on the wire rather than left for each client
    /// to re-spell.
    ///
    /// Derived, and stored anyway, on purpose: the alternative is a map from
    /// check id to heading in `src/types.ts` *and* another in the MCP view,
    /// which is two copies of prose that would drift the first time a check is
    /// renamed. Every constructor below fills it, so there is still exactly one
    /// place the words live.
    pub label: &'static str,
    /// Which repository this row is about, for the two per-repository checks.
    /// `None` for the six that describe the installation as a whole.
    ///
    /// The name is carried rather than only interpolated into `detail` so the
    /// panel can group rows by repository without parsing prose — but `detail`
    /// names it too, because the acceptance criterion is that an unauthenticated
    /// `gh` "produces a warning naming the affected repository" and a sentence
    /// that only makes sense next to its own heading is not that.
    pub repository: Option<String>,
    pub status: CheckStatus,
    pub detail: String,
    pub remediation: Option<String>,
}

impl CheckResult {
    pub fn pass(check: Check, detail: impl Into<String>) -> Self {
        Self {
            check,
            label: check.label(),
            repository: None,
            status: CheckStatus::Pass,
            detail: detail.into(),
            remediation: None,
        }
    }

    pub fn warn(check: Check, detail: impl Into<String>, remediation: impl Into<String>) -> Self {
        Self {
            check,
            label: check.label(),
            repository: None,
            status: CheckStatus::Warn,
            detail: detail.into(),
            remediation: Some(remediation.into()),
        }
    }

    pub fn fail(check: Check, detail: impl Into<String>, remediation: impl Into<String>) -> Self {
        Self {
            check,
            label: check.label(),
            repository: None,
            status: CheckStatus::Fail,
            detail: detail.into(),
            remediation: Some(remediation.into()),
        }
    }

    /// Attaches the repository a per-repository row is about.
    pub fn about(mut self, repository: impl Into<String>) -> Self {
        self.repository = Some(repository.into());
        self
    }
}

/// What the doctor found, in [`Check::ALL`] order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub results: Vec<CheckResult>,
}

impl DoctorReport {
    pub fn new(mut results: Vec<CheckResult>) -> Self {
        // Stable across runs so the panel does not reshuffle, and stable within
        // a check so two repositories keep the order `repo::list` returned them
        // in (alphabetical). `sort_by_key` is stable, which is what makes the
        // second half true without a second key.
        results.sort_by_key(|result| {
            Check::ALL
                .iter()
                .position(|check| *check == result.check)
                .unwrap_or(usize::MAX)
        });
        Self { results }
    }

    /// Whether anything here refuses to let the queue start.
    pub fn is_blocking(&self) -> bool {
        self.results
            .iter()
            .any(|result| result.status.is_blocking())
    }

    pub fn blocking(&self) -> impl Iterator<Item = &CheckResult> {
        self.results
            .iter()
            .filter(|result| result.status.is_blocking())
    }

    /// The refusal message the user reads when the queue will not start.
    ///
    /// Every blocking row, detail and remediation both, in one string — not a
    /// count and not the first one. A user told "1 check failed" has to go
    /// looking; a user told "the Claude Code CLI could not be run … install
    /// Claude Code and check that `claude` runs in a terminal" is already doing
    /// the thing. It is one string because it crosses the error boundary as
    /// `Error::invalid`'s message, and seam-contract D8 keeps `ErrorCode` coarse
    /// on purpose: specificity that is required lives in the message.
    pub fn blocking_summary(&self) -> String {
        let blocking: Vec<&CheckResult> = self.blocking().collect();
        if blocking.is_empty() {
            // Not reachable through `QueueHandle::start`, which asks
            // `is_blocking` first — but a summary that claimed a failure it
            // could not name would be worse than a sentence saying so.
            return "no preflight check is failing".to_string();
        }

        let lines: Vec<String> = blocking
            .iter()
            .map(|result| {
                let subject = match &result.repository {
                    Some(repository) => format!("{} ({repository})", result.check.label()),
                    None => result.check.label().to_string(),
                };
                match &result.remediation {
                    Some(remediation) => format!("{subject}: {} {remediation}", result.detail),
                    None => format!("{subject}: {}", result.detail),
                }
            })
            .collect();

        format!(
            "the run queue was not started because {} preflight {} failing. {}",
            lines.len(),
            if lines.len() == 1 {
                "check is"
            } else {
                "checks are"
            },
            lines.join(" "),
        )
    }
}

/// The external binaries the doctor probes, injectable.
///
/// `claude` comes from the runner's own [`RunnerConfig::program`] rather than
/// from this struct's default whenever an [`Environment`] is built by
/// [`Environment::for_runner`]: a doctor that reported on a *different* `claude`
/// than the one the queue spawns would be reassuring about the wrong binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Programs {
    pub claude: PathBuf,
    pub git: PathBuf,
    pub gh: PathBuf,
}

impl Default for Programs {
    fn default() -> Self {
        Self {
            claude: PathBuf::from(CLAUDE_CLI),
            git: PathBuf::from(repo::GIT_CLI),
            gh: PathBuf::from(repo::GH_CLI),
        }
    }
}

/// Everything the doctor needs that it cannot discover for itself.
///
/// `run_handles` is the live table, not a URL copied at startup, for exactly
/// the reason seam-contract D17.4 gives for the runner holding the same value:
/// `set_mcp_port` rebinds the server at runtime and a copy goes stale. Reading
/// [`RunHandles::endpoint`] is also *why* the MCP check does not attempt a bind
/// of its own — a doctor that tried to prove the port was free would have to
/// take it from the server currently holding it, and would then report a
/// healthy installation as broken (or, worse, briefly break it).
#[derive(Debug, Clone)]
pub struct Environment {
    pub programs: Programs,
    pub paths: AppPaths,
    pub run_handles: RunHandles,
}

impl Environment {
    /// Everything defaulted except where state lives.
    pub fn new(paths: AppPaths) -> Self {
        Self {
            programs: Programs::default(),
            paths,
            run_handles: RunHandles::default(),
        }
    }

    /// The shell's construction: the same `claude` binary and the same live
    /// handle table the queue and the MCP server already share.
    pub fn for_runner(paths: AppPaths, runner: &RunnerConfig) -> Self {
        Self {
            programs: Programs {
                claude: runner.program.clone(),
                ..Programs::default()
            },
            paths,
            run_handles: runner.run_handles.clone(),
        }
    }

    pub fn with_programs(mut self, programs: Programs) -> Self {
        self.programs = programs;
        self
    }
}

/// Runs every check and collects the report.
///
/// **This is the function task 013 calls before a scheduled start.** It is
/// public and named for that: a broken environment reported at 22:00, when the
/// schedule fires, is a five-second warning; the same environment discovered at
/// 02:00 is a wasted night, which is the whole argument of task 018. Task 013
/// owns the timer and nothing about the timer belongs here.
///
/// Never `Err` for a failing check — a failure is a [`CheckResult`], which is
/// the point. The `Result` is for the two things that are not check outcomes at
/// all: the repository list not being readable, and the port setting not being
/// readable. Both mean the database is unavailable, which no remediation string
/// on a panel can help with.
pub async fn run(ctx: &ServiceContext, environment: &Environment) -> Result<DoctorReport> {
    let repositories = repo::list(ctx).await?;
    let configured_port = mcp::configured_port(&ctx.pool).await?;

    let mut results = vec![
        checks::claude_cli(&environment.programs.claude).await,
        checks::claude_authenticated(&environment.programs.claude).await,
        checks::git(&environment.programs.git).await,
        checks::data_directory(&environment.paths),
        checks::disk_space(&environment.paths),
        checks::mcp_port(
            configured_port,
            environment.run_handles.endpoint().as_deref(),
        ),
    ];

    // Sequentially rather than joined: this spawns up to two subprocesses per
    // repository, and a machine with a dozen registered repositories should not
    // answer a Re-check click with two dozen simultaneous `git` processes. The
    // doctor has seconds to spend and no deadline to meet.
    for repository in &repositories {
        results.push(checks::repository_path(repository).await?);
        results.push(checks::github_cli(repository, &environment.programs.gh).await?);
    }

    Ok(DoctorReport::new(results))
}
