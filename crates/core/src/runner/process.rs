//! Spawning the Claude Code CLI, and the life of one run (ADR-0004, ADR-0012).
//!
//! Stages either side of this are pure: [`events`](super::events) knows what a
//! line means and [`outcome`](super::outcome) knows what an ending means, and
//! neither of them ever touched a process. This module is where a real child
//! exists — the one place in `rimaia-core` that has to be right about pipes,
//! signals and process groups, because everything it gets wrong shows up at 2am
//! as a hung queue or an orphaned `node` still holding a port.
//!
//! # The invocation is a pure function, on purpose
//!
//! [`Invocation::args`] takes no context and touches nothing: it is (task,
//! repository, settings, trigger) in, argument vector out. That is what makes
//! ADR-0012's permission posture and ADR-0004's isolation flags assertable as
//! exact vectors, the same class of contract as prompt composition — the flags
//! this product is most dangerous to get wrong are the ones a test can pin
//! byte for byte without spawning anything.
//!
//! **Argument vectors, never `sh -c`.** A worktree path routinely contains a
//! space, and the composed system prompt contains newlines and quotes.
//!
//! # Two environment rules that are not the same rule
//!
//! 1. [`RunEnvironment`] is the operator's *choice* — `inherit` (default) or
//!    `strict_local`, read through task 006's accessor. Inheriting adds a fixed
//!    ~$0.08 of setup per run — ~13,300 cache-creation tokens, charged once per
//!    session and not per turn (`spike/FINDINGS.md` §2) — and buys the
//!    operator's own MCP servers, which ADR-0004's amendment decides is worth
//!    it by default. See `db::settings::ENVIRONMENT_SETUP_COST_USD` for why the
//!    spike's "3.6x" is the misleading way to state that.
//! 2. **Stripping `CLAUDE_*` is not a choice** and is not configurable. Those
//!    variables are process identity, not user config: a child told
//!    `CLAUDE_CODE_SESSION_ID` believes it is a nested session of whatever
//!    spawned Rimaia. Rimaia is developed and tested from inside a Claude Code
//!    session, so this is live, not theoretical. See [`is_process_identity`].
//!
//! # Cancellation is a signal to a process *group*
//!
//! `spike/FINDINGS.md` §7 measured it: `process_group(0)` at spawn plus
//! `kill -TERM -<pgid>` takes the whole tree down with zero orphans, where
//! signalling the child alone leaves its `bash`, its `npm` and whatever those
//! started still running. SIGTERM first, then SIGKILL when the grace period
//! ends, because a killed run still emits its `result` on the way out and that
//! event is worth more than the second we wait for it.
//!
//! And a killed run exits **143**, not by signal — so nothing here treats the
//! stream stopping as evidence of anything. The loop keeps reading after it has
//! asked the child to stop.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use crate::context::ServiceContext;
use crate::db::settings::{self, RunEnvironment};
use crate::db::{new_id, ExitClass, Run, RunState, RunStatus, Task};
use crate::error::{Error, Result};
use crate::mcp::{RunHandles, MCP_SERVER_NAME};
use crate::paths::AppPaths;
use crate::repo;
use crate::runner::events::{EventStream, InitEvent, RunEvent, TokenUsage};
use crate::runner::outcome::{
    finish_run, start_run, NewRun, PullRequestWatch, RunOutcome, SpawnedAs, Termination,
};
use crate::runner::prompt::{compose_prompt, compose_resume_prompt, compose_system_append};
use crate::runner::strategy;
use crate::scheduler::attempts::{self, Ending};
use crate::scheduler::{pause, retry, InFlight};
use crate::tasks::{self, set_run_state};
use crate::worktree::{self, Worktree};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The prerequisite, resolved through `PATH`. **Never bundled** (ADR-0004): it
/// is the same binary the operator already trusts interactively, carrying the
/// subscription auth Rimaia deliberately never handles.
pub const CLAUDE_CLI: &str = "claude";

/// What `strict_local` restricts settings discovery to (ADR-0004's amendment).
///
/// Deliberately **not** `--bare`: that would also switch off `CLAUDE.md`
/// discovery, and a repository's own `CLAUDE.md` is wanted in both modes — it is
/// the project's instructions, not the operator's configuration.
const SETTING_SOURCES: &str = "project,local";

/// How long a cancelled run is given to emit its `result` and exit before it is
/// killed outright.
///
/// A killed run announces itself before dying (`spike/FINDINGS.md` §5), and that
/// announcement is what tells a reviewer the difference between "we stopped it"
/// and "it died". Ten seconds buys that; it is not a timeout on the run itself,
/// which ADR-0010's run window owns.
pub const DEFAULT_GRACE_PERIOD: Duration = Duration::from_secs(10);

/// The signal-sending utility. POSIX-mandated, and reached as an argument vector
/// like every other subprocess here.
///
/// A separate process rather than a direct `libc::kill` because a negative pid —
/// "signal this process group" — has no expression in `std` or `tokio`, and
/// `rimaia-core` does not depend on `libc`. The cost is one `execve` per
/// cancellation, on a path that runs at most twice per run.
#[cfg(unix)]
const KILL: &str = "kill";

/// The `settings` key holding the tool blocklist (ADR-0012 point 3: "the list is
/// a setting so it can grow with experience").
///
/// Read through [`settings::get`] rather than through SQL of this module's own,
/// which is seam-contract D3's rule. The key constant sits here rather than in
/// [`settings`] because the vocabulary is the runner's — these are Claude Code
/// permission-rule patterns, and nothing outside this module has any business
/// knowing their shape.
pub const DISALLOWED_TOOLS: &str = "disallowed_tools";

/// What a run refuses to let the agent do when the setting is unset.
///
/// ADR-0012 point 3's three operations — force-pushing, hard resets against
/// remotes, remote branch deletion — each spelled both flag-first
/// (`git push --force origin main`) and remote-first (`git push origin
/// --force main`), because `Bash(x:*)` is a **command-line prefix** match and
/// covering only one ordering is not a blocklist for the flag, only for one
/// way of typing it. `Bash(git push origin :*)` additionally covers the
/// `git push origin :branch` delete shorthand, which carries no `--delete` or
/// `-d` token at all.
///
/// This still does not cover every phrasing — `git push origin +main:main`
/// forces via refspec with no `--force`/`-f` token to match, and a remote
/// named anything other than `origin` is untouched by the reset pattern —
/// which is exactly the incompleteness the next paragraph names.
///
/// **This is not a sandbox and does not pretend to be one.** ADR-0012's own
/// Consequences say so: "the denied-tools list is a blocklist, and blocklists
/// are incomplete by construction. It reduces the common accidents". The
/// isolation that actually bounds a run is the worktree (ADR-0005); this stops
/// the specific mistakes that reach past it into a shared remote.
pub const DEFAULT_DISALLOWED_TOOLS: [&str; 11] = [
    "Bash(git push --force:*)",
    "Bash(git push -f:*)",
    "Bash(git push --force-with-lease:*)",
    "Bash(git push --delete:*)",
    "Bash(git push -d:*)",
    "Bash(git push origin --force:*)",
    "Bash(git push origin -f:*)",
    "Bash(git push origin --delete:*)",
    "Bash(git push origin -d:*)",
    "Bash(git push origin :*)",
    "Bash(git reset --hard origin/:*)",
];

/// The prefix that marks an environment variable as Claude Code's own process
/// identity. See [`is_process_identity`].
const IDENTITY_PREFIX: &str = "CLAUDE";

/// The `settings` key holding the per-attempt turn budget (ADR-0011:
/// "`--max-turns` per attempt bounds runaway loops").
///
/// Here rather than in [`settings`], for the same seam-contract D3 reason
/// [`DISALLOWED_TOOLS`] gives: the vocabulary is the runner's, and nothing
/// outside this module has any business knowing that a "turn" is a Claude Code
/// concept.
pub const MAX_TURNS: &str = "max_turns";

/// How many turns one attempt may take when nobody has set a budget.
///
/// Chosen from two constraints pulling in opposite directions. A turn limit is
/// [`ExitClass::Fatal`] (`runner::outcome`'s rule 4, and ADR-0011's fatal row
/// names it): a budget set too low does not cost a retry, it **abandons the
/// task**, half-done, with a card that says "failed" for a reason the operator
/// did not choose. And a budget set too high does not bound the runaway
/// ADR-0011 wants bounded. The spike's recorded runs took four to forty turns
/// for one-file work, so a substantial overnight plan plausibly wants a few
/// hundred; three hundred is comfortably above honest work and far below a loop
/// that has stopped making progress.
///
/// **This changes every implementation run's argv**, which is why
/// `tests/runner_process.rs` asserts the vector with `--max-turns 300` in it
/// rather than without: before task 014 the flag was never passed at all, and
/// the CLI's own default applied.
pub const DEFAULT_MAX_TURNS: u32 = 300;

// ---------------------------------------------------------------------------
// What a run is allowed to do
// ---------------------------------------------------------------------------

/// ADR-0012's two postures, and there is no third.
///
/// The ADR is emphatic that this is "the decision with the largest blast radius
/// in the product", so it is an enum rather than a string threaded through a
/// call chain: a mode is chosen once, from a [`RunTrigger`], and the `init`
/// event is checked against it rather than trusted (see
/// [`verify_permission_mode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionMode {
    /// Unattended: no prompts, behind a per-repository opt-in.
    BypassPermissions,
    /// A run the operator started with the app in front of them. ADR-0012's
    /// "conservative default for interactive runs".
    AcceptEdits,
}

impl PermissionMode {
    /// The CLI's own spelling, which is also what `init` echoes back — one
    /// string, so the request and the verification cannot drift apart.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BypassPermissions => "bypassPermissions",
            Self::AcceptEdits => "acceptEdits",
        }
    }
}

/// Who asked for this run.
///
/// Task 008 only ever produces [`Manual`](RunTrigger::Manual) — the queue is
/// task 009 — but both arms exist now so that 009 adds a *caller*, not a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunTrigger {
    /// Started by the scheduler, unattended.
    Queued,
    /// Started by hand from the board, with the app in the foreground.
    Manual,
}

impl RunTrigger {
    pub const fn permission_mode(self) -> PermissionMode {
        match self {
            Self::Queued => PermissionMode::BypassPermissions,
            Self::Manual => PermissionMode::AcceptEdits,
        }
    }
}

// ---------------------------------------------------------------------------
// The argument vector
// ---------------------------------------------------------------------------

/// Everything the CLI invocation is a pure function of.
///
/// Assembled once by [`run_task`] and then only read, so the exact vector a run
/// was spawned with is a value a test can hold — see this module's header for
/// why that matters more here than almost anywhere else in the codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Minted by Rimaia **before** the process exists, so `--resume` works even
    /// if the child dies before emitting its `init` (ADR-0004). ADR-0011 has
    /// every attempt of a task's eventual retry loop share this id; driving
    /// that loop — deciding *when* a fresh call reuses one — is task 014's job
    /// (task 008's own task file lists "Retry and resume behaviour (014)" as
    /// out of scope), so [`run_task`] mints a fresh one on every call it makes.
    pub session_id: String,
    /// Continue an existing session rather than opening this one.
    ///
    /// `spike/FINDINGS.md` §6 is explicit about the pairing: "`--session-id` on
    /// the first run and `--resume` on the retry is the right shape". They are
    /// alternatives, not companions — the first opens an id, the second reuses
    /// one. Task 014 is what sets this; task 008 never does.
    pub resume: bool,
    pub permission_mode: PermissionMode,
    pub run_environment: RunEnvironment,
    /// `--append-system-prompt`, from [`compose_system_append`]. ADR-0012 point
    /// 4 reserves this channel for orchestrator facts the agent may not weigh
    /// against the task.
    pub system_append: String,
    /// `None` lets the CLI pick, which is what a task with no explicit strategy
    /// means (ADR-0016: the column is nullable precisely so "not set" is
    /// expressible). An empty `--model` would be a worse answer than no flag.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `--allowedTools`: tools pre-approved for this session, so the run never
    /// stops to ask.
    ///
    /// Empty for an implementation run, which gets its blanket approval from
    /// `bypassPermissions` (ADR-0012) and needs no list. Non-empty for a
    /// strategy run, and that is not a nicety — it is the only reason the
    /// planner can answer at all.
    ///
    /// **`acceptEdits` does not cover MCP tools.** It auto-approves file edits;
    /// an `mcp__*` call still raises a permission request, and an unattended run
    /// has nobody to grant it, so the call is refused and the planner produces
    /// nothing. Naming the one tool here is what keeps ADR-0012's argument for
    /// the *narrow* posture intact instead of reaching for
    /// `bypassPermissions`: the planner is permitted exactly its own write-back
    /// and nothing else, while `disallowed_tools` still denies it every way of
    /// touching the worktree.
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    /// `--mcp-config`, as an inline JSON string from
    /// [`RunHandles::mcp_config_json`](crate::mcp::RunHandles::mcp_config_json)
    /// (seam-contract D17.4).
    ///
    /// `None` for an implementation run, which reaches Rimaia through nothing
    /// and has no reason to. `Some` for a strategy run, whose entire output is
    /// one call back through the scoped handle this names — and also `None` for
    /// a strategy run when no endpoint is bound, the busy-port case D16.7 makes
    /// non-fatal, which [`strategy::resolve`](super::strategy::resolve) refuses
    /// to plan into rather than spawning a planner that cannot answer.
    pub mcp_config: Option<String>,
    /// ADR-0011 bounds a runaway loop with this. Left `None` by task 008: there
    /// is no column and no setting holding a turn budget, and *what* the budget
    /// should be is retry policy, which is task 014's. The field is here so that
    /// task adds a value rather than a flag.
    ///
    /// Task 020 is the first caller to set it: a planner's budget comes from
    /// the catalogue, so a strategy run is bounded even though the retry loop
    /// that will eventually bound implementation runs is still task 014's.
    pub max_turns: Option<u32>,
}

impl Invocation {
    /// The argv, in task 008's documented order.
    ///
    /// Order is part of the contract rather than an accident: `--disallowedTools`
    /// is variadic, so it has to be followed by a flag (or by nothing) for its
    /// list to terminate where this function intends.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];

        args.push(
            if self.resume {
                "--resume"
            } else {
                "--session-id"
            }
            .to_string(),
        );
        args.push(self.session_id.clone());

        args.push("--permission-mode".to_string());
        args.push(self.permission_mode.as_str().to_string());

        if self.run_environment == RunEnvironment::StrictLocal {
            args.push("--strict-mcp-config".to_string());
            args.push("--setting-sources".to_string());
            args.push(SETTING_SOURCES.to_string());
        }

        args.push("--append-system-prompt".to_string());
        args.push(self.system_append.clone());

        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = &self.effort {
            args.push("--effort".to_string());
            args.push(effort.clone());
        }

        // Before `--disallowedTools` so the two variadic lists read in the order
        // a human reasons about them — what this run may do, then what it may
        // not. Each `--` token ends the previous list, so the pairing is safe in
        // either order; this one is just the legible one.
        if !self.allowed_tools.is_empty() {
            args.push("--allowedTools".to_string());
            args.extend(self.allowed_tools.iter().cloned());
        }

        if !self.disallowed_tools.is_empty() {
            args.push("--disallowedTools".to_string());
            args.extend(self.disallowed_tools.iter().cloned());
        }

        // Immediately after the other variadic flag and before `--max-turns`,
        // which is the ordering contract `--disallowedTools` above already
        // documents: `--mcp-config` also takes "JSON files or strings
        // (space-separated)", so what ends its list is the next `--` token.
        // Anything non-flag appended after it would be read as a second config.
        if let Some(mcp_config) = &self.mcp_config {
            args.push("--mcp-config".to_string());
            args.push(mcp_config.clone());
        }

        if let Some(max_turns) = self.max_turns {
            args.push("--max-turns".to_string());
            args.push(max_turns.to_string());
        }

        args
    }
}

// ---------------------------------------------------------------------------
// The environment the child inherits
// ---------------------------------------------------------------------------

/// Whether a parent environment variable is Claude Code's own process identity
/// and must therefore not reach the child.
///
/// **A prefix rule, not a list.** `spike/FINDINGS.md` §2b counted thirteen
/// `CLAUDE_*` / `CLAUDECODE` variables exported into children by Claude Code
/// 2.1.234; a list of thirteen names is a list that goes stale on the next
/// release, and the failure it produces — a child quietly believing it is a
/// nested session of its parent — is invisible until someone reads a transcript
/// and finds the wrong session id. The prefix is `CLAUDE` rather than `CLAUDE_`
/// because `CLAUDECODE` carries no underscore.
///
/// Case-insensitive so the rule means the same thing on a platform whose
/// environment is not case-sensitive.
pub fn is_process_identity(name: &str) -> bool {
    name.get(..IDENTITY_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(IDENTITY_PREFIX))
}

/// Removes Rimaia's own inherited process identity from a child's environment.
///
/// Rule 2 of this module's header: unconditional, in both `run_environment`
/// modes, and not a setting. Removals rather than a rebuilt environment, because
/// everything else — `PATH`, `HOME`, the operator's shell configuration — is
/// exactly what a run is supposed to have.
///
/// `vars_os()`, not `vars()`: the latter panics on any non-Unicode key or
/// value, and this reads the *whole* parent environment before a single one of
/// its names has been checked for identity. One legacy latin-1 variable
/// anywhere in the operator's shell would otherwise take the panic through
/// every caller of this function, including [`probe_cli`] and [`spawn`].
/// `to_string_lossy` is safe here because [`is_process_identity`] only ever
/// compares an ASCII prefix — a name that is not valid UTF-8 is never one of
/// the `CLAUDE*` variables being stripped anyway.
/// `pub(crate)` so task 018's doctor strips the same variables when it runs
/// `claude auth status`. "Always strip" is easier to keep true than "strip on
/// the paths that matter", and a second copy of the rule in another module is
/// exactly how the two would come to disagree.
pub(crate) fn strip_process_identity(command: &mut Command) {
    let names = std::env::vars_os().map(|(name, _)| name.to_string_lossy().into_owned());
    for name in inherited_identity_vars(names) {
        command.env_remove(name);
    }
}

/// The names in `parent` that a spawned run must not inherit.
///
/// Takes the environment rather than reading it, so the rule is testable as the
/// pure thing it is; [`strip_process_identity`] passes `std::env::vars_os()`,
/// lossily converted.
pub fn inherited_identity_vars<I, S>(parent: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    parent
        .into_iter()
        .map(Into::into)
        .filter(|name| is_process_identity(name))
        .collect()
}

// ---------------------------------------------------------------------------
// Settings this module reads
// ---------------------------------------------------------------------------

/// Rimaia's own MCP tools, denied to an implementation run whatever the
/// operator's configuration says.
///
/// # Why this is not in [`DEFAULT_DISALLOWED_TOOLS`]
///
/// That list is configuration, and an explicitly empty setting means an empty
/// list — the operator is allowed to turn it off. This is not configuration. It
/// closes a hole that would otherwise make [`RunScope`](crate::mcp::RunScope)
/// decorative.
///
/// # The hole
///
/// `run_environment` defaults to `inherit` (ADR-0004's amendment), and ADR-0006
/// tells the operator to register Rimaia with `claude mcp add`. So an
/// implementation run's session loads the **operator's unscoped `/mcp`**, and
/// ADR-0012 gives that run `bypassPermissions`, which — unlike `acceptEdits` —
/// auto-approves MCP calls. The run would hold `move_task`, `create_task`,
/// `set_task_dependencies`, and every ADR-0021 configuration tool: exactly the
/// rows `Tool::run_access` marks `Refused`. A prompt-injected run could mark its
/// own card `done`, or change the model every future run uses, with no bash
/// involved at all.
///
/// # Derived, not listed
///
/// Built from [`Tool::ALL`](crate::mcp::Tool::ALL) rather than spelled out, so a
/// tool added later is denied by existing.
///
/// # A trap for task 021
///
/// This denies by *tool name*, and a scoped handle registers under the same
/// server name — so it will block a legitimate scoped handle just as
/// effectively as the inherited operator one. That is correct today, because an
/// implementation run gets `mcp_config: None` and has no scoped handle to
/// block. It stops being correct the moment task 021 gives one to a run so it
/// can write findings back to its own card, which is exactly what ADR-0017
/// plans.
///
/// Whoever does that has three options and should pick deliberately: register
/// the run-scoped handle under a distinct server name, apply this denial only
/// when no grant was minted for the run, or establish that `--allowedTools`
/// overrides `--disallowedTools` in the CLI — which is **not verified here**
/// and should not be assumed.
fn rimaia_tools_denied_to_a_run() -> Vec<String> {
    // The bare server name denies the whole server where Claude Code supports
    // it; the per-tool entries are what make the denial exact if it does not.
    std::iter::once(format!("mcp__{MCP_SERVER_NAME}"))
        .chain(
            crate::mcp::Tool::ALL
                .iter()
                .map(|tool| format!("mcp__{MCP_SERVER_NAME}__{}", tool.as_str())),
        )
        .collect()
}

/// The tool blocklist, or [`DEFAULT_DISALLOWED_TOOLS`] when nobody has set one.
///
/// One pattern per line rather than comma-separated: a pattern contains spaces
/// and parentheses already, and a line is the one separator that cannot appear
/// inside one. Blank lines are ignored, so the stored value stays readable in
/// the `sqlite3` CLI (ADR-0003).
///
/// An explicitly empty setting means an empty list — the operator turning the
/// blocklist off is a thing they are allowed to do, and silently restoring the
/// default would be the same defect `settings::base_instructions` documents.
pub async fn disallowed_tools(pool: &sqlx::SqlitePool) -> Result<Vec<String>> {
    let Some(stored) = settings::get(pool, DISALLOWED_TOOLS).await? else {
        return Ok(DEFAULT_DISALLOWED_TOOLS
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect());
    };

    Ok(stored
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// The per-attempt turn budget, or [`DEFAULT_MAX_TURNS`] when nobody has set
/// one.
///
/// Tolerant on read, like every other key in this codebase and for ADR-0003's
/// reason — but note what "tolerant" costs here and does not: an unusable value
/// falls back to a budget that is generous, never to *no* budget, because "no
/// budget" is the runaway ADR-0011 asked for a bound against.
pub async fn max_turns(pool: &sqlx::SqlitePool) -> Result<u32> {
    let Some(stored) = settings::get(pool, MAX_TURNS).await? else {
        return Ok(DEFAULT_MAX_TURNS);
    };

    match stored.trim().parse::<u32>() {
        Ok(0) | Err(_) => {
            tracing::warn!(
                value = stored,
                default = DEFAULT_MAX_TURNS,
                "unusable max_turns; falling back to the default"
            );
            Ok(DEFAULT_MAX_TURNS)
        }
        Ok(value) => Ok(value),
    }
}

// ---------------------------------------------------------------------------
// The prerequisite
// ---------------------------------------------------------------------------

/// Confirms the CLI is installed and reports the version it printed.
///
/// Called **before any run state is written** — before the task is claimed and
/// before a `runs` row exists — because task 008's acceptance criterion is that
/// a missing binary is a clear error and not a task stuck `running` with a
/// transcript that was never opened.
pub async fn probe_cli(program: &Path) -> Result<String> {
    let mut command = Command::new(program);
    command.arg("--version");
    // Even here. "Always strip" is easier to keep true than "strip on the paths
    // that matter", and this is a child of Rimaia's like any other.
    strip_process_identity(&mut command);

    let output = command
        .output()
        .await
        .map_err(|error| missing_cli(program, error.to_string()))?;

    if !output.status.success() {
        return Err(missing_cli(
            program,
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The sentence a user reads when the prerequisite is not there.
///
/// It names what was looked for, where, and what to do — and says that Rimaia
/// runs their own installation, because "install Claude Code" is otherwise a
/// confusing thing to be told by an app that is visibly running Claude Code.
/// No install command is quoted: there are several, they change, and a wrong one
/// is worse than none.
fn missing_cli(program: &Path, detail: impl AsRef<str>) -> Error {
    let detail = detail.as_ref();
    let mut message = format!(
        "could not run the Claude Code CLI ({}). Rimaia drives your own installation and never \
         bundles one — install Claude Code, check that `{CLAUDE_CLI}` runs in a terminal, then \
         start the run again",
        program.display(),
    );
    if !detail.is_empty() {
        message.push_str(&format!(" ({detail})"));
    }
    Error::invalid(message)
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

/// A cancel button, as a value.
///
/// Cloneable and cheap: the caller keeps one clone and hands the other to
/// [`run_task`], so "cancel this task's run" needs no registry inside core and
/// no run id — which matters, because the run id does not exist until after the
/// process has been committed to.
///
/// Built on `watch` rather than `Notify` because [`cancelled`](Self::cancelled)
/// is polled from inside a `select!` loop and re-created on every iteration: a
/// watch channel retains its value, so a cancellation that lands between two
/// polls is still there to be found.
#[derive(Debug, Clone)]
pub struct CancelSignal {
    requested: Arc<watch::Sender<bool>>,
}

impl Default for CancelSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelSignal {
    pub fn new() -> Self {
        let (requested, _) = watch::channel(false);
        Self {
            requested: Arc::new(requested),
        }
    }

    /// Asks the run to stop. Idempotent, and does nothing if the run already
    /// finished — a cancel that arrives too late is not an error.
    pub fn cancel(&self) {
        // `send_replace` rather than `send`: the latter reports an error when no
        // receiver exists, which is the normal state between iterations of the
        // run loop.
        self.requested.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.requested.borrow()
    }

    /// Resolves once [`cancel`](Self::cancel) has been called, now or earlier.
    pub async fn cancelled(&self) {
        let mut receiver = self.requested.subscribe();
        loop {
            if *receiver.borrow_and_update() {
                return;
            }
            if receiver.changed().await.is_err() {
                // Unreachable: this value owns the sender, so it outlives every
                // receiver it mints. Waiting forever is the safe reading anyway
                // — reporting a cancellation nobody asked for would kill a run.
                std::future::pending::<()>().await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The knobs that belong to the runner rather than to a task.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Resolved through `PATH` by default. A path is accepted so a test can
    /// point at a stand-in, and so an operator with a non-standard install has
    /// somewhere for that to go later.
    pub program: PathBuf,
    /// How long a cancelled child is given to emit its `result` before SIGKILL.
    pub grace_period: Duration,
    /// See [`Invocation::max_turns`].
    pub max_turns: Option<u32>,
    /// Where a strategy run mints its scoped MCP token (seam-contract D17.4).
    ///
    /// On the *runner's* config rather than passed per call because it is a
    /// property of this installation in exactly the way `program` is: the
    /// shell builds one table in `setup()` and hands the same one to
    /// [`mcp::build`](crate::mcp::build), so the endpoint a run is handed is
    /// always the address the server most recently bound — including after a
    /// runtime rebind.
    ///
    /// [`Default`] gives an empty table with no endpoint, which is the right
    /// answer for every test that never plans anything: no endpoint means no
    /// `--mcp-config` and no planner, not a failure.
    pub run_handles: RunHandles,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            program: PathBuf::from(CLAUDE_CLI),
            grace_period: DEFAULT_GRACE_PERIOD,
            max_turns: None,
            run_handles: RunHandles::default(),
        }
    }
}

/// The session a retry continues (ADR-0011: "retries resume, they do not
/// restart").
///
/// A one-field struct rather than a bare `Option<String>` on the request,
/// because "some string" is exactly what a caller could get wrong here and the
/// consequence is not a compile error: a run id, a task id or a stale session
/// would all spawn happily and start a *new* conversation under an old name,
/// throwing away the context the resume exists to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeSession {
    pub session_id: String,
}

/// What [`run_task`] is asked to do.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub task_id: String,
    pub trigger: RunTrigger,
    /// Continue an earlier attempt's session rather than open a new one.
    ///
    /// `Some` turns three things on together, and they belong together: the
    /// `--resume` argv branch, the one-line continuation prompt instead of the
    /// composed one, and **not running the planner again**. Splitting them into
    /// three flags would let a caller ask for two of the three, and the
    /// combination that would go unnoticed is the expensive one — a planner
    /// process spawned per retry, quietly rewriting the model mid-chain.
    pub resume: Option<ResumeSession>,
    /// The caller's half of the cancel button.
    pub cancel: CancelSignal,
    /// The registry whose per-repository
    /// [`preparation_lock`](InFlight::preparation_lock) this run takes while
    /// its worktree is created.
    ///
    /// `None` skips the lock, which is right for every caller that cannot have
    /// a second run in the same repository to collide with: a unit test driving
    /// one run, and any single-run path. It is an `Option` rather than a
    /// required field because the alternative is making every such caller mint
    /// a registry it will never read, and a registry nobody else holds
    /// serializes against nothing anyway — an argument that would be false the
    /// moment it were written down as a requirement.
    pub in_flight: Option<InFlight>,
}

impl RunRequest {
    /// A manual "Run now" on a task, with a fresh cancel signal the caller keeps
    /// no handle on. For a run nothing will ever cancel.
    pub fn manual(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            trigger: RunTrigger::Manual,
            resume: None,
            cancel: CancelSignal::new(),
            in_flight: None,
        }
    }

    /// The same, continuing `session_id` — a "Retry now" pressed on a task that
    /// is waiting out a wall.
    pub fn resuming(task_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            resume: Some(ResumeSession {
                session_id: session_id.into(),
            }),
            ..Self::manual(task_id)
        }
    }
}

/// One spawned attempt, as [`execute`] takes it.
///
/// Borrowed rather than owned because every field already lives somewhere the
/// caller is holding — the `runs` row, the worktree, the composed prompt.
#[derive(Debug, Clone, Copy)]
pub struct Attempt<'a> {
    pub task_id: &'a str,
    pub run_id: &'a str,
    /// The child's working directory (ADR-0005). Never the user's own checkout.
    pub worktree: &'a Path,
    /// Delivered on stdin, which is then closed — see [`execute`].
    pub prompt: &'a str,
    pub invocation: &'a Invocation,
    pub cancel: &'a CancelSignal,
}

// ---------------------------------------------------------------------------
// Running one task
// ---------------------------------------------------------------------------

/// [`worktree::prepare`], serialized per repository when the caller supplied a
/// registry to serialize on.
///
/// The lock is held across this call and nothing else. Everything after it —
/// the strategy run, the prompt composition, the child process — happens inside
/// a worktree of its own and has no shared `.git` to contend for, so extending
/// the lock past this point would turn a per-repository cap of two into a
/// sequential queue wearing a parallel label.
async fn prepare_worktree(
    ctx: &ServiceContext,
    in_flight: Option<&InFlight>,
    repository_id: &str,
    task_id: &str,
) -> Result<Worktree> {
    match in_flight {
        Some(registry) => {
            let lock = registry.preparation_lock(repository_id);
            let _held = lock.lock().await;
            worktree::prepare(ctx, task_id).await
        }
        None => worktree::prepare(ctx, task_id).await,
    }
}

/// Runs one task end to end: validate, prepare, spawn, stream, classify, record.
///
/// The order of the first four steps is the part worth reading, because each one
/// is a precondition for the next and one of them is an acceptance criterion:
///
/// 1. **The repository's opt-in** (ADR-0012). Checked through
///    [`repo::ensure_unattended_runs_allowed`] rather than re-derived, so the
///    board's disabled "Run now" tooltip and this refusal are one sentence.
/// 2. **The CLI exists.** Before anything is written, per task 008's acceptance
///    criterion — a missing prerequisite must not leave a half-open run.
/// 3. **The worktree**, through task 007's idempotent [`worktree::prepare`],
///    holding the repository's [`preparation_lock`](InFlight::preparation_lock)
///    across it when the caller supplied a registry — see
///    [`prepare_worktree`]. This is also what writes `tasks.branch`, which is
///    why the task detail is re-read afterwards: the composed prompt names the
///    branch, and composing it from the row as it was a moment earlier would
///    name nothing.
/// 4. **The claim.** The task goes to `run_state = running` before the row is
///    opened, mirroring ADR-0010's selection-then-run order.
pub async fn run_task(
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    request: RunRequest,
) -> Result<Run> {
    let task_id = request.task_id.clone();
    let detail = tasks::get_task(ctx, &task_id).await?;
    let repository = repo::get(ctx, &detail.task.repository_id).await?;
    repo::ensure_unattended_runs_allowed(&repository)?;

    let version = probe_cli(&config.program).await?;
    tracing::debug!(%task_id, cli = %version, "the Claude Code prerequisite is installed");

    let worktree =
        prepare_worktree(ctx, request.in_flight.as_ref(), &repository.id, &task_id).await?;

    let detail = tasks::get_task(ctx, &task_id).await?;

    // The claim moved ahead of the strategy run in task 020, and the order is
    // load-bearing: it is what makes this task exclusively ours *before* a
    // second process is spawned on its behalf. Without it the queue and a manual
    // "Run now" could each start a planner for the same card and each pay for
    // it. `claim` is already a no-op for a task this process just moved to
    // `running`, and `release` below already covers a claim that never became a
    // run — the only new thing is that it now also covers the planner.
    claim(ctx, &detail.task).await?;

    // **A resume does not run the planner again**, and this is the easiest
    // thing in the retry loop to get wrong by omission. `strategy::resolve`
    // spawns a whole second Claude Code process to decide how the work should
    // be done; running it per retry would pay for that decision once per wall
    // the task hits, and — worse — a second planner reading a half-finished
    // worktree could answer differently from the first, changing the model or
    // the effort *mid-session*. The attempt continues what the first one
    // started, so it continues with what the first one was given: the effective
    // values already on the row (ADR-0016's precedence chain, resolved by
    // `tasks::get_task`), and no fresh guidance, because the guidance the
    // planner produced is already in the session being resumed.
    let (model, effort, guidance) = if request.resume.is_some() {
        (
            detail.effective_model.clone(),
            detail.effective_effort.clone(),
            None,
        )
    } else {
        let resolved = match strategy::resolve(
            ctx,
            paths,
            config,
            &detail,
            &repository,
            Path::new(&worktree.path),
            &request.cancel,
        )
        .await
        {
            Ok(resolved) => resolved,
            Err(error) => {
                release(ctx, &task_id).await;
                return Err(error);
            }
        };

        match resolved {
            strategy::Resolution::Ready {
                model,
                effort,
                guidance,
            } => (model, effort, guidance),
            // Stopped while planning. Spawning the implementation run now would
            // run the very thing the user just cancelled, so the claim goes
            // back and nothing else happens.
            strategy::Resolution::Cancelled => {
                release(ctx, &task_id).await;
                return Err(Error::invalid(format!(
                    "\"{}\" was cancelled while its strategy was being planned",
                    detail.task.title,
                )));
            }
        }
    };

    // Everything between the claim and `start_run` runs inside this block so a
    // failure gives the claim back. Before task 020 these reads all happened
    // *before* the claim and could not strand anything; moving the claim up to
    // cover the strategy run put four fallible calls behind it, and a bare `?`
    // on any of them would leave a card reading "running" with no `runs` row to
    // close it out — repaired only by the next launch's reconciliation.
    let prepared = async {
        // Re-read once more: a planner that wrote a proposal changed this row,
        // and the prompt has to carry what the card now says rather than what
        // it said before the planner ran.
        let detail = tasks::get_task(ctx, &task_id).await?;
        let base = settings::base_instructions(&ctx.pool).await?;
        let run_environment = settings::run_environment(&ctx.pool).await?;
        let disallowed = disallowed_tools(&ctx.pool).await?;
        let turns = max_turns(&ctx.pool).await?;
        Ok::<_, Error>((detail, base, run_environment, disallowed, turns))
    }
    .await;

    let (detail, base, run_environment, disallowed, turns) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            release(ctx, &task_id).await;
            return Err(error);
        }
    };

    // ADR-0011: "retries resume, they do not restart... every retry is
    // `claude -p --resume <session-id>` with a short continuation prompt". The
    // composed prompt is already in the session; sending it again would
    // re-spend the tokens that produced the context this attempt exists to
    // reuse, and would read to the agent as a fresh instruction to start over.
    let prompt = match &request.resume {
        Some(_) => compose_resume_prompt(&detail),
        None => compose_prompt(&base, &detail, &repository, guidance.as_ref()),
    };

    let invocation = Invocation {
        session_id: match &request.resume {
            Some(resume) => resume.session_id.clone(),
            None => new_id(),
        },
        resume: request.resume.is_some(),
        permission_mode: request.trigger.permission_mode(),
        run_environment,
        system_append: compose_system_append(&detail, &repository),
        model,
        effort,
        // Empty: ADR-0012 gives an unattended implementation run
        // `bypassPermissions`, which approves everything the blocklist has not
        // already taken away. A list here would narrow that, which is task
        // 012's or 014's decision to make, not this line's.
        allowed_tools: Vec::new(),
        // The operator's blocklist, plus the denial that is not theirs to turn
        // off — see `rimaia_tools_denied_to_a_run`.
        disallowed_tools: disallowed
            .into_iter()
            .chain(rimaia_tools_denied_to_a_run())
            .collect(),
        // An implementation run reaches Rimaia through nothing: the scoped
        // handle exists so a *planner* can answer, and ADR-0016 gives the
        // implementation run no reason to write to its own card.
        mcp_config: None,
        // The setting, unless this installation's wiring overrides it — which
        // in production it never does (`RunnerConfig::default` leaves it
        // `None`), and which a test or a future strategy caller may.
        max_turns: config.max_turns.or(Some(turns)),
    };

    let run = match start_run(
        ctx,
        paths,
        NewRun {
            task_id: task_id.clone(),
            session_id: invocation.session_id.clone(),
            prompt: prompt.clone(),
        },
    )
    .await
    {
        Ok(run) => run,
        // The task is claimed and there is no row to close it out. Releasing it
        // here is what stops a failed start from leaving a card that says
        // "running" until the next launch reconciles it.
        Err(error) => {
            release(ctx, &task_id).await;
            return Err(error);
        }
    };

    let attempt = Attempt {
        task_id: &task_id,
        run_id: &run.id,
        worktree: Path::new(&worktree.path),
        prompt: &prompt,
        invocation: &invocation,
        cancel: &request.cancel,
    };

    match execute(ctx, paths, config, attempt).await {
        Ok(mut outcome) => {
            // ADR-0011's policy, applied at the one call site every starter
            // passes through — the queue, "Run now", "Retry now", and whatever
            // starts a run next. Deciding it here rather than in `finish_run`
            // is what keeps `outcome` the only writer of the `runs` row while
            // still leaving one place that knows the retry rules; deciding it
            // in each *caller* would be three copies of ADR-0011's table.
            apply_retry_policy(ctx, &task_id, &run.id, &mut outcome).await;

            tracing::info!(
                %task_id,
                run_id = %run.id,
                exit_class = ?outcome.exit_class,
                turns = outcome.num_turns,
                cost_usd = outcome.cost_usd,
                resume_after = outcome.resume_after.map(|at| at.to_rfc3339()),
                "run finished",
            );
            finish_run(ctx, &run.id, &outcome).await
        }
        // Spawning or supervision itself failed. The row exists, so it is closed
        // as fatal rather than left open — an unfinished `runs` row and a task
        // stuck `running` are the same defect from two tables.
        Err(error) => {
            let outcome = runner_fatal(error.to_string());
            if let Err(nested) = finish_run(ctx, &run.id, &outcome).await {
                tracing::error!(run_id = %run.id, %nested, "could not record a failed run");
            }
            Err(error)
        }
    }
}

/// Decides when — or whether — this task is tried again, and records the two
/// consequences of that decision (ADR-0011).
///
/// Fills [`RunOutcome::resume_after`], which `finish_run` writes to the row and
/// `apply_to_task` routes on, and raises ADR-0011's **global pause** when the
/// class is `usage_limit`. The pause uses the *same instant* the policy put on
/// `resume_after` rather than the raw reported reset, so the queue does not
/// wake a minute of jitter before the task it is waiting for is due.
///
/// Infallible by construction: a failure to read the history or to write the
/// pause is logged and the attempt is left un-retried. That direction is
/// deliberate — an outcome recorded with no retry is a card a human sees in the
/// morning, where a propagated error would abandon the `runs` row and leave the
/// task `running` with no process, which is the one state nothing recovers
/// from.
async fn apply_retry_policy(
    ctx: &ServiceContext,
    task_id: &str,
    run_id: &str,
    outcome: &mut RunOutcome,
) {
    let ending = Ending {
        exit_class: outcome.exit_class,
        usage_limit_resets_at: outcome.usage_limit_resets_at,
    };

    let history = match attempts::history(ctx, task_id, ending).await {
        Ok(history) => history,
        Err(error) => {
            tracing::error!(
                %task_id, %run_id, %error,
                "could not read this task's attempt history; it will not be retried",
            );
            return;
        }
    };
    let Some(history) = history else {
        // No rows at all, for a run that just wrote one. Unreachable in
        // practice and not worth a panic: nothing to resume is the safe
        // reading.
        return;
    };

    // The run id as the jitter seed — see `retry::jitter` on why the spread is
    // derived rather than drawn, and why it has to differ per run.
    let decision = retry::decide(&history, ctx.clock.now(), run_id);
    outcome.resume_after = decision.resume_after();

    if outcome.exit_class == ExitClass::UsageLimit {
        if let Some(until) = outcome.resume_after {
            if let Err(error) = pause::note_usage_limit(ctx, until).await {
                tracing::error!(
                    %task_id, %run_id, %error,
                    "could not record the usage-limit pause; the queue may start into a closed window",
                );
            }
        }
    }
}

/// Puts the task into `run_state = running`, from wherever it legally can be.
///
/// Every transition goes through [`set_run_state`], which is the only writer of
/// that column (ADR-0006). What this function adds is the *route*: the state
/// machine has no `idle -> running` edge, deliberately, so a start walks
/// `idle -> queued -> running` exactly as a scheduler's selection would.
///
/// `running` already is not an error. That is task 009's arm: ADR-0010 requires
/// selection and the transition to happen in one transaction, so when the
/// scheduler exists it claims the task itself and hands this a task already
/// claimed. Until then this is the only claimer there is.
///
/// `failed` and `cancelled` re-enter through `queued`, because that is what
/// pressing "Run now" on a task that failed last night means, and ADR-0007's own
/// note on those edges says trying again "re-enters at Queued like every other
/// start". `blocked` is refused: an unsatisfied dependency is task 011's to
/// clear, not this module's to override.
async fn claim(ctx: &ServiceContext, task: &Task) -> Result<()> {
    let route: &[RunState] = match task.run_state {
        RunState::Running => &[],
        RunState::Idle | RunState::Failed | RunState::Cancelled => {
            &[RunState::Queued, RunState::Running]
        }
        RunState::Queued | RunState::WaitingRetry => &[RunState::Running],
        RunState::Blocked => {
            return Err(Error::invalid(format!(
                "\"{}\" is blocked by an unsatisfied dependency and cannot be started",
                task.title,
            )))
        }
    };

    for state in route {
        set_run_state(ctx, &task.id, *state).await?;
    }
    Ok(())
}

/// Undoes a claim that never became a run.
///
/// Best effort and deliberately not fatal: the caller is already returning an
/// error, and replacing it with "and also the release failed" would hide the
/// thing that actually went wrong. Startup reconciliation is the backstop
/// (ADR-0011).
async fn release(ctx: &ServiceContext, task_id: &str) {
    if let Err(error) = set_run_state(ctx, task_id, RunState::Failed).await {
        tracing::error!(%task_id, %error, "could not release a task whose run never started");
    }
}

/// The outcome for an ending the CLI never described, because Rimaia is the one
/// that ended it.
///
/// [`RunOutcome::of`] classifies what the *agent* reported; a misconfiguration
/// or an unwritable transcript is a judgement this module makes about a run that
/// may not have reported anything at all. `fatal` because neither condition gets
/// better by being retried (ADR-0011's fatal row).
fn runner_fatal(message: String) -> RunOutcome {
    RunOutcome {
        exit_class: ExitClass::Fatal,
        status: RunStatus::Failed,
        error_message: Some(message),
        num_turns: None,
        cost_usd: None,
        duration_ms: None,
        pr_url: None,
        usage_limit_resets_at: None,
        // `fatal` is ADR-0011's no-retry row, so there is nothing to schedule
        // and the column stays NULL — which is also what puts the task in
        // `failed` rather than leaving it waiting on a deadline that will
        // never arrive.
        resume_after: None,
        // Supervision itself failed, so there is no evidence the process ever
        // ran as anything. Seam-contract D18 makes that NULL rather than a
        // record of what we intended to spawn.
        spawned_as: SpawnedAs::default(),
        usage: TokenUsage::default(),
    }
}

/// Replaces a classified outcome's verdict while keeping the numbers the
/// `result` event did carry.
fn override_as_fatal(outcome: &mut RunOutcome, message: String) {
    outcome.exit_class = ExitClass::Fatal;
    outcome.status = RunStatus::Failed;
    outcome.error_message = Some(message);
}

// ---------------------------------------------------------------------------
// The process
// ---------------------------------------------------------------------------

/// Spawns the CLI, streams its output into `paths`, and classifies how it ended.
///
/// Everything about the child lives inside this function. It returns a
/// [`RunOutcome`] rather than writing one, so the `runs` row keeps a single
/// writer (`outcome::finish_run`) and task 009 can wrap this in its own
/// bookkeeping without duplicating any of the supervision.
///
/// # Stdin is written from another task, and closed
///
/// **A prompt left unclosed hangs the run** — `spike/FINDINGS.md` §7 measured
/// exactly that. It is also written concurrently rather than before the read
/// loop starts: a several-thousand-token prompt is larger than a pipe buffer,
/// so writing it inline would block until the child drained it, and the child
/// cannot be read from while we are blocked writing to it.
pub async fn execute(
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    attempt: Attempt<'_>,
) -> Result<RunOutcome> {
    let mut stream = EventStream::create(ctx, paths, attempt.task_id, attempt.run_id)?;
    let mut process = spawn(config, &attempt)?;
    let group = process.group;

    let mut stdin = process
        .child
        .stdin
        .take()
        .ok_or_else(|| Error::internal("the child's stdin was piped but is not there"))?;
    let prompt = attempt.prompt.to_string();
    let writer = tokio::spawn(async move {
        stdin.write_all(prompt.as_bytes()).await?;
        stdin.shutdown().await
        // And then dropped, which is what actually closes the pipe.
    });

    let mut stdout = BufReader::new(
        process
            .child
            .stdout
            .take()
            .ok_or_else(|| Error::internal("the child's stdout was piped but is not there"))?,
    )
    .lines();
    let mut stderr = BufReader::new(
        process
            .child
            .stderr
            .take()
            .ok_or_else(|| Error::internal("the child's stderr was piped but is not there"))?,
    )
    .lines();

    let mut pull_request = PullRequestWatch::default();
    let mut cancelled = false;
    let mut terminating = false;
    let mut killed = false;
    let mut fatal: Option<String> = None;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut status = None;
    // ADR-0022. Stays `None` for a run that dies before announcing itself, and
    // seam-contract D18 makes that NULL rather than a guess.
    let mut observed_model: Option<String> = None;

    // Armed only when a termination is ordered; the guards below keep it from
    // being polled before that, which is why it can start already elapsed.
    let grace = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(grace);

    loop {
        if status.is_some() && !stdout_open && !stderr_open {
            break;
        }

        tokio::select! {
            // Draining the stream outranks noticing the exit. A killed run emits
            // its `result` and *then* exits (spike section 5), and the two can
            // become ready in the same poll — reading first is what stops the
            // most informative event of a cancelled run being classified as an
            // absent one.
            biased;

            line = stdout.next_line(), if stdout_open => match line {
                Ok(Some(line)) => match stream.observe(&line) {
                    Ok(Some(event)) => {
                        pull_request.observe(&event);
                        if let RunEvent::Init(init) = &event {
                            // ADR-0022's `runs.model`. Taken from `init` rather
                            // than from the flag because the flag may have been
                            // absent (the CLI's own default) or an alias, and
                            // this is the resolved name a later chart groups by.
                            observed_model.clone_from(&init.model);
                            report_applied_environment(init, attempt.invocation);
                            if let Err(error) =
                                verify_permission_mode(init, attempt.invocation.permission_mode)
                            {
                                fatal.get_or_insert_with(|| error.to_string());
                                begin_termination(
                                    &mut terminating, group, &mut grace, config.grace_period,
                                );
                            }
                        }
                    }
                    Ok(None) => {}
                    // ADR-0013 makes the transcript the record of the run. One
                    // that can no longer record itself has nothing to review in
                    // the morning, and the conditions that produce this (a full
                    // disk, a failing volume) do not clear on their own — so the
                    // run is stopped rather than left burning tokens unrecorded.
                    Err(error) => {
                        tracing::error!(
                            run_id = %attempt.run_id, %error,
                            "the transcript is no longer writable; stopping the run",
                        );
                        fatal.get_or_insert_with(|| format!(
                            "the run was stopped because its transcript could not be written: {error}"
                        ));
                        begin_termination(
                            &mut terminating, group, &mut grace, config.grace_period,
                        );
                    }
                },
                Ok(None) => stdout_open = false,
                Err(error) => {
                    tracing::warn!(run_id = %attempt.run_id, %error, "stdout stopped being readable");
                    stdout_open = false;
                }
            },

            line = stderr.next_line(), if stderr_open => match line {
                Ok(Some(line)) => {
                    // A diagnostic, never the record: failing to keep it is
                    // worth a log line and nothing more.
                    if let Err(error) = stream.observe_stderr(&line) {
                        tracing::warn!(run_id = %attempt.run_id, %error, "could not capture stderr");
                        stderr_open = false;
                    }
                }
                Ok(None) => stderr_open = false,
                Err(error) => {
                    tracing::warn!(run_id = %attempt.run_id, %error, "stderr stopped being readable");
                    stderr_open = false;
                }
            },

            exited = process.child.wait(), if status.is_none() => {
                // Waited on rather than inferred from the pipes closing, because
                // those two are not the same event: a background process the
                // agent started inherits stdout and holds it open long after the
                // CLI itself is gone.
                status = Some(exited?);
                // Which is also why the group is reaped here. Anything still in
                // it once the CLI has exited is something the agent left behind,
                // and task 008 is explicit that no orphaned children survive —
                // so this is both the cleanup and the thing that releases a
                // leaked pipe. Without it one stray `npm run dev` would hold a
                // finished run open, and its task at `running`, until the app
                // was restarted.
                //
                // Nothing already written is lost: bytes in a pipe outlive the
                // process that wrote them, and the biased branch above drains
                // them before this one is ever polled.
                reap_group(group);
            },

            _ = attempt.cancel.cancelled(), if !terminating => {
                cancelled = true;
                begin_termination(&mut terminating, group, &mut grace, config.grace_period);
            },

            _ = &mut grace, if terminating && !killed => {
                killed = true;
                tracing::warn!(
                    run_id = %attempt.run_id,
                    "the grace period elapsed; killing the process group",
                );
                signal_group(group, Signal::Kill).await;
            },
        }
    }

    let status = status.expect("the loop only ends once the child has been reaped");
    process.reaped = true;

    match writer.await {
        Ok(Ok(())) => {}
        // A broken pipe here is ordinary for a run that ended before it read its
        // prompt; anything else still leaves the classification to speak for the
        // run, which is why this is a warning and not a verdict.
        Ok(Err(error)) => tracing::warn!(
            run_id = %attempt.run_id, %error, "the prompt was not fully delivered on stdin",
        ),
        Err(error) => tracing::warn!(
            run_id = %attempt.run_id, %error, "the stdin writer did not finish cleanly",
        ),
    }

    stream.finish()?;

    let mut termination = Termination::from_stream(&stream).exited_with(status.code());
    if cancelled {
        termination = termination.cancelled();
    }
    let mut outcome = RunOutcome::of(&termination, pull_request.into_url());
    if let Some(message) = fatal {
        override_as_fatal(&mut outcome, message);
    }

    // This is the only place the invocation and the run's own account of itself
    // are both in scope, which is why ADR-0022's three "spawned as" columns are
    // filled here rather than at `finish_run`. Falling back to the flag when
    // `init` never arrived keeps a killed run's model recorded; falling all the
    // way to `None` when neither exists is D18's "not recorded".
    outcome.spawned_as = SpawnedAs {
        model: observed_model.or_else(|| attempt.invocation.model.clone()),
        effort: attempt.invocation.effort.clone(),
        run_environment: Some(attempt.invocation.run_environment.as_str().to_string()),
    };

    if stream.malformed_lines() > 0 {
        tracing::warn!(
            run_id = %attempt.run_id,
            lines = stream.malformed_lines(),
            "some stream lines could not be parsed; they are in the transcript verbatim",
        );
    }

    // Logged whatever the class, because a refused run can end any way at all:
    // the interesting fact is that the agent spent the attempt being told no,
    // and the permission mode beside it is the lever that changes that
    // (ADR-0012).
    if stream.denied_tool_calls() > 0 {
        tracing::warn!(
            run_id = %attempt.run_id,
            denied = stream.denied_tool_calls(),
            permission_mode = attempt.invocation.permission_mode.as_str(),
            "tool calls were refused for want of approval",
        );
    }

    Ok(outcome)
}

/// Kills whatever is left of the process group, without waiting for it.
///
/// Spawned rather than awaited because the caller has to go straight back to
/// reading: the pipe this is releasing is the one it is reading from.
fn reap_group(group: Option<u32>) {
    tokio::spawn(async move { signal_group(group, Signal::Kill).await });
}

/// Asks the process group to stop and starts the clock on it, unless a
/// termination is already under way.
///
/// Three different conditions order one — the user cancelling, a permission mode
/// nobody asked for, a transcript that can no longer be written — and only the
/// first of them should arm the grace period; a second `reset` would hand a
/// child that is already ignoring SIGTERM a fresh reprieve.
fn begin_termination(
    terminating: &mut bool,
    group: Option<u32>,
    grace: &mut std::pin::Pin<&mut tokio::time::Sleep>,
    grace_period: Duration,
) {
    if *terminating {
        return;
    }
    *terminating = true;
    grace
        .as_mut()
        .reset(tokio::time::Instant::now() + grace_period);
    // Fire and forget: the loop that called this has to go straight back to
    // reading, because the `result` event we are terminating for is still coming.
    tokio::spawn(async move { signal_group(group, Signal::Term).await });
}

/// A spawned CLI process and the group it leads.
struct ChildProcess {
    child: Child,
    /// The process group id, which is the child's own pid because it was spawned
    /// with `process_group(0)`. `None` on a platform without process groups.
    group: Option<u32>,
    /// Set once [`Child::wait`] has returned, so [`Drop`] knows there is nothing
    /// left to kill.
    reaped: bool,
}

impl Drop for ChildProcess {
    /// The backstop for "all child processes reaped on app exit" (task 008).
    ///
    /// Reached when the future supervising this run is dropped rather than
    /// awaited — the app quitting, or a future cancelled from above. Blocking in
    /// `Drop` is not free, but `kill` returns immediately and the alternative is
    /// an agent's `npm run dev` still holding a port after Rimaia is gone.
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if let Some(group) = self.group {
            let _ = blocking_signal_group(group, Signal::Kill);
        }
        // `kill_on_drop(true)` at spawn covers the direct child even where the
        // group signal could not be delivered.
    }
}

/// Builds the command and starts it.
fn spawn(config: &RunnerConfig, attempt: &Attempt<'_>) -> Result<ChildProcess> {
    let mut command = Command::new(&config.program);
    command
        .args(attempt.invocation.args())
        .current_dir(attempt.worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    strip_process_identity(&mut command);
    set_process_group(&mut command);

    let child = command.spawn().map_err(|error| {
        missing_cli(
            &config.program,
            format!(
                "could not start it in {}: {error}",
                attempt.worktree.display()
            ),
        )
    })?;
    let group = child.id();

    tracing::debug!(
        run_id = %attempt.run_id,
        pid = group,
        worktree = %attempt.worktree.display(),
        environment = attempt.invocation.run_environment.as_str(),
        permission_mode = attempt.invocation.permission_mode.as_str(),
        "spawned the Claude Code CLI",
    );

    Ok(ChildProcess {
        child,
        group,
        reaped: false,
    })
}

/// Which signal to deliver. Spelled as the CLI's own names, since that is what
/// crosses to [`KILL`].
#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

impl Signal {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Kill => "KILL",
        }
    }
}

#[cfg(unix)]
fn set_process_group(command: &mut Command) {
    // Zero means "a new group whose id is the child's pid". Everything the agent
    // starts inherits it, which is what makes one signal reach the whole tree.
    command.process_group(0);
}

/// Process groups are a POSIX concept. ADR-0004 notes Windows needs its own
/// answer here (a job object) and calls it the process module's business; there
/// is no Windows target yet, so this is honestly a gap rather than a stub
/// pretending to be a port.
#[cfg(not(unix))]
fn set_process_group(_command: &mut Command) {}

#[cfg(unix)]
async fn signal_group(group: Option<u32>, signal: Signal) {
    let Some(group) = group else {
        tracing::error!("the child reported no pid; it cannot be signalled");
        return;
    };

    let target = format!("-{group}");
    let result = Command::new(KILL)
        .args(["-s", signal.as_str(), "--", &target])
        .output()
        .await;

    match result {
        // A group that is already gone reports a non-zero status, which is the
        // ordinary outcome of escalating to SIGKILL after SIGTERM worked.
        Ok(output) if output.status.success() => {
            tracing::debug!(
                group,
                signal = signal.as_str(),
                "signalled the process group"
            );
        }
        Ok(output) => tracing::debug!(
            group,
            signal = signal.as_str(),
            detail = %String::from_utf8_lossy(&output.stderr).trim(),
            "the process group did not accept the signal; it has most likely already exited",
        ),
        Err(error) => tracing::error!(
            group, signal = signal.as_str(), %error,
            "could not run `{KILL}`; the process tree may survive",
        ),
    }
}

#[cfg(not(unix))]
async fn signal_group(_group: Option<u32>, _signal: Signal) {
    tracing::error!("cancelling a run is not implemented on this platform");
}

/// The synchronous form, for [`ChildProcess::drop`].
#[cfg(unix)]
fn blocking_signal_group(group: u32, signal: Signal) -> std::io::Result<()> {
    std::process::Command::new(KILL)
        .args(["-s", signal.as_str(), "--", &format!("-{group}")])
        .output()
        .map(|_| ())
}

#[cfg(not(unix))]
fn blocking_signal_group(_group: u32, _signal: Signal) -> std::io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Verifying what the CLI actually applied
// ---------------------------------------------------------------------------

/// Checks the permission mode `init` says it applied against the one that was
/// asked for (ADR-0004's amendment).
///
/// **A mismatch is fatal.** ADR-0012 calls the permission posture "the decision
/// with the largest blast radius in the product"; a CLI that quietly ran under a
/// different one is a run nobody authorised, and continuing it would make the
/// per-repository opt-in a statement about what Rimaia *requested* rather than
/// about what happened.
///
/// **An absent field is not.** ADR-0004's tolerance rule is explicit that a
/// Claude Code update must not break a queue, and a renamed field would
/// otherwise fail every run at once — the loudest possible version of exactly
/// the failure that rule exists to prevent. The warning is the record.
pub fn verify_permission_mode(init: &InitEvent, requested: PermissionMode) -> Result<()> {
    match init.permission_mode.as_deref() {
        Some(applied) if applied != requested.as_str() => Err(Error::internal(format!(
            "Claude Code applied permission mode \"{applied}\" when Rimaia asked for \"{}\". \
             The run was stopped rather than continued under a posture nobody chose (ADR-0012).",
            requested.as_str(),
        ))),
        Some(_) => Ok(()),
        None => {
            tracing::warn!(
                requested = requested.as_str(),
                "the init event named no permission mode; it could not be verified",
            );
            Ok(())
        }
    }
}

/// Logs what the run actually inherited, and warns where it is not what was
/// asked for.
///
/// Neither of these stops a run. An MCP server surviving `strict_local` costs
/// tokens and hygiene, not authority — and ADR-0004's amendment asks for this to
/// be *visible* ("task 018's doctor should report the hooks and MCP servers a
/// run will inherit, so it is a visible choice rather than a surprise at 2am"),
/// which is a log line and a doctor, not a killed run.
fn report_applied_environment(init: &InitEvent, invocation: &Invocation) {
    let servers: Vec<&str> = init
        .mcp_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();

    tracing::debug!(
        tools = init.tools.len(),
        mcp_servers = servers.len(),
        model = init.model.as_deref().unwrap_or("-"),
        version = init.claude_code_version.as_deref().unwrap_or("-"),
        "the run's applied configuration",
    );

    if invocation.run_environment == RunEnvironment::StrictLocal && !servers.is_empty() {
        tracing::warn!(
            servers = servers.join(", "),
            "strict_local was requested but MCP servers are connected",
        );
    }

    // `apiKeySource: "none"` is what confirms the run is on the operator's
    // subscription rather than a metered key, which is ADR-0004's premise.
    match init.api_key_source.as_deref() {
        Some("none") | None => {}
        Some(source) => tracing::warn!(
            source,
            "this run is authenticating with an API key rather than the Claude Code subscription",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// The thirteen names `spike/FINDINGS.md` §2b counted, spelled out here so
    /// the prefix rule is tested against the vocabulary that actually leaks
    /// rather than against a synthetic `CLAUDE_X`.
    const OBSERVED_LEAKS: [&str; 4] = [
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_ENTRYPOINT",
    ];

    #[test]
    fn every_claude_variable_is_process_identity_including_the_one_with_no_underscore() {
        for name in OBSERVED_LEAKS {
            assert!(is_process_identity(name), "{name} must not reach a child");
        }
        // The reason the prefix is `CLAUDE` and not `CLAUDE_`.
        assert!(is_process_identity("CLAUDECODE"));
    }

    #[test]
    fn a_variable_that_merely_mentions_claude_elsewhere_is_kept() {
        for name in [
            "PATH",
            "HOME",
            "MY_CLAUDE_NOTES",
            "ANTHROPIC_API_KEY",
            "CLAUD",
        ] {
            assert!(
                !is_process_identity(name),
                "{name} was stripped unnecessarily"
            );
        }
    }

    #[test]
    fn a_variable_name_that_is_not_ascii_does_not_panic_on_a_byte_boundary() {
        // `get(..6)` returns None rather than slicing through a multi-byte
        // character, which is the whole reason it is used instead of indexing.
        assert!(!is_process_identity("CLAÜDE_CODE"));
        assert!(!is_process_identity("🚀"));
    }

    #[test]
    fn the_identity_rule_selects_only_the_leaking_names_from_a_whole_environment() {
        let parent = [
            "PATH",
            "CLAUDECODE",
            "HOME",
            "CLAUDE_CODE_SESSION_ID",
            "SHELL",
        ];

        assert_eq!(
            inherited_identity_vars(parent),
            vec![
                "CLAUDECODE".to_string(),
                "CLAUDE_CODE_SESSION_ID".to_string()
            ]
        );
    }

    #[test]
    fn a_trigger_decides_the_permission_mode_and_nothing_else_does() {
        // ADR-0012 point 6: bypass is for the unattended path, and a run started
        // by hand with the app in front of the operator defaults to acceptEdits.
        assert_eq!(
            RunTrigger::Queued.permission_mode(),
            PermissionMode::BypassPermissions
        );
        assert_eq!(
            RunTrigger::Manual.permission_mode(),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            PermissionMode::BypassPermissions.as_str(),
            "bypassPermissions"
        );
        assert_eq!(PermissionMode::AcceptEdits.as_str(), "acceptEdits");
    }

    #[tokio::test]
    async fn a_cancellation_that_arrived_before_anyone_waited_is_still_found() {
        // The reason this is a watch channel and not a `Notify`: the run loop
        // re-creates this future on every iteration, so a signal that lands
        // between two polls has to be retained rather than missed.
        let cancel = CancelSignal::new();
        cancel.cancel();

        assert!(cancel.is_cancelled());
        cancel.cancelled().await;
    }

    #[tokio::test]
    async fn a_clone_of_the_cancel_signal_cancels_the_same_run() {
        let cancel = CancelSignal::new();
        let held_by_the_caller = cancel.clone();

        held_by_the_caller.cancel();

        assert!(cancel.is_cancelled());
    }
}
