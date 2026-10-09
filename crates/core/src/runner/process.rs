//! Spawning the agent CLI, and the life of one run (ADR-0004, ADR-0012,
//! ADR-0026).
//!
//! Stages either side of this are pure: [`events`](super::events) knows what a
//! line means and [`outcome`](super::outcome) knows what an ending means, and
//! neither of them ever touched a process. This module is where a real child
//! exists — the one place in `rimaia-core` that has to be right about pipes,
//! signals and process groups, because everything it gets wrong shows up at 2am
//! as a hung queue or an orphaned `node` still holding a port.
//!
//! # Which CLI, and what this module still owns
//!
//! ADR-0026 moved the flag vocabulary behind
//! [`AgentProvider`](super::provider::AgentProvider): a
//! [`RunIntent`](super::provider::RunIntent) in, a
//! [`SpawnPlan`](super::provider::SpawnPlan) out. Nothing below this line knows
//! a flag. What stays here is everything that touches a pipe, a signal or a
//! process group — written once, for every provider, because a provider that
//! spawned its own child could leak a process tree or write a transcript Rimaia
//! cannot read.
//!
//! The intent is still a pure value, which is what makes ADR-0012's permission
//! posture and ADR-0004's isolation flags assertable as exact vectors: the flags
//! this product is most dangerous to get wrong are the ones a test can pin byte
//! for byte without spawning anything.
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
//!    it by default. See `provider::claude::INHERIT_COST_USD` for why the
//!    spike's "3.6x" is the misleading way to state that.
//! 2. **Stripping an agent's identity variables is not a choice** and is not
//!    configurable. Those variables are process identity, not user config: a
//!    child told `CLAUDE_CODE_SESSION_ID` believes it is a nested session of
//!    whatever spawned Rimaia. Rimaia is developed and tested from inside a
//!    Claude Code session, so this is live, not theoretical. The rule takes the
//!    **union** over every registered provider rather than the active one's
//!    (seam-contract D27.5) — see [`is_process_identity`].
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
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use crate::board::{
    BoardPort, Claim, FinishRun, LeaseRef, NextStep, RunContext, StartRun, TeamLimits,
    TranscriptEnd,
};
use crate::context::ServiceContext;
use crate::credentials::inject::ChildEnvironment;
use crate::credentials::CredentialAccess;
use crate::db::settings::{self, RunEnvironment};
use crate::db::Repository;
use crate::db::{new_id, ExitClass, Run, RunKind, RunStatus};
use crate::error::{Error, Result};
use crate::mcp::RunHandles;
use crate::paths::AppPaths;
use crate::repo;
use crate::runner::events::{
    transcript_path, EventStream, InitEvent, RunEvent, RunTail, TokenUsage,
};
use crate::runner::outcome::{
    kind_not_wired_yet, PullRequestWatch, RunOutcome, SpawnedAs, Termination,
};
use crate::runner::prompt::{compose_prompt, compose_resume_prompt, compose_system_append};
use crate::runner::provider::{
    self, claude, AgentProvider, ForbiddenOperation, PromptStyle, RunIntent, RunPlan,
    SessionIntent, SpawnPlan,
};
use crate::runner::strategy;
use crate::scheduler::{pause, InFlight, ResumePoint};
use crate::worktree::{self, Worktree};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The prerequisite's default program name, and the blocklist an unset setting
/// means, re-exported at the paths they have always had.
///
/// Both moved behind the provider seam (ADR-0026), and both are still Claude
/// Code's: the doctor imports the first and `tests/runner_process.rs` imports
/// both, and neither import line changes. If somebody later drops these
/// re-exports it is a compile error, never a silent break.
pub use claude::{is_process_identity, CLAUDE_CLI, DEFAULT_DISALLOWED_TOOLS};

/// ADR-0012's two postures, at the path every caller already used. The type
/// itself lives beside the intent it rides on, because it is a Rimaia concept
/// and not one provider's flag.
pub use provider::PermissionMode;

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
/// [`settings`] because the vocabulary is the runner's — the stored value is
/// written in the active provider's own rule language, and nothing outside this
/// module has any business knowing its shape.
pub const DISALLOWED_TOOLS: &str = "disallowed_tools";

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

/// Who asked for this run.
///
/// Task 008 only ever produces [`Manual`](RunTrigger::Manual) — the queue is
/// task 009 — but both arms exist now so that 009 adds a *caller*, not a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
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
// The environment the child inherits
// ---------------------------------------------------------------------------

/// Removes Rimaia's own inherited process identity from a child's environment.
///
/// Rule 2 of this module's header: unconditional, in both `run_environment`
/// modes, not a setting, and taking the **union** over every provider rather
/// than the active one's (seam-contract D27.5). Removals rather than a rebuilt environment, because
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
    let prefixes = provider::identity_prefixes();
    parent
        .into_iter()
        .map(Into::into)
        .filter(|name| is_identity_of_any_provider(&prefixes, name))
        .collect()
}

/// Whether `name` starts with any registered provider's identity prefix.
///
/// Case-insensitive, so the rule means the same thing on a platform whose
/// environment is not case-sensitive, and on a *slice* of prefixes because one
/// provider exports two of them — a fix that assumes a single string is exactly
/// what seam-contract D27.5 exists to fail.
fn is_identity_of_any_provider(prefixes: &[&str], name: &str) -> bool {
    prefixes.iter().any(|prefix| {
        name.get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
    })
}

// ---------------------------------------------------------------------------
// Settings this module reads
// ---------------------------------------------------------------------------

/// Rimaia's operator tool surface, denied to every run the runner spawns
/// whatever the operator's configuration says.
///
/// # Why this is not part of the blocklist setting
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
/// # Named as an intent, spelled by the provider
///
/// Which tool names that is, and whether a whole server can be denied at once,
/// is one provider's business (ADR-0026 point 4). What Rimaia states is the
/// operation.
///
/// # Every run, and never a run's own handle
///
/// The denial works by tool name, so a run's own scoped handle is served under
/// a different server name, `rimaia-run`, and this is spelled at the operator's
/// `rimaia` only. That is what lets it be unconditional: [`forbidden_operations`]
/// appends it for every intent, implementation, planner, review or fix, and no
/// caller can drop it. Seam-contract D30 points 1 and 2 decide it, and
/// `run-scoped-server-name.jsonl` records the CLI matching the server segment
/// exactly rather than as a prefix.
const RIMAIA_TOOL_SURFACE: ForbiddenOperation = ForbiddenOperation::RimaiaToolSurface;

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

/// The blocklist as the **operations** an intent carries (ADR-0026 point 4).
///
/// The same setting [`disallowed_tools`] reads, one level up and as the board
/// handed it over in [`TeamLimits`]: an unset setting means ADR-0012 point 3's
/// three operations, and a set one means the operator's own rules, each tagged
/// with the provider whose vocabulary it was written in. The tag is what stops
/// those strings being handed to a provider that never spoke them — and what
/// stops them being silently dropped either.
///
/// `extra` is whatever the caller adds on top, and only what is particular to
/// it: nothing for an implementation run, the planner's own denials for a
/// strategy run. The operator tool surface is appended here, after `extra`, for
/// every intent (seam-contract D30 point 2), because a denial a caller has to
/// remember is one a caller can forget.
///
/// Pure since task 036: the read is the board's, done once when it builds a
/// [`RunContext`](crate::board::RunContext), so a blocklist cannot be read
/// twice and refused on one value and spawned on another.
pub(crate) fn forbidden_operations(
    limits: &TeamLimits,
    provider: &dyn AgentProvider,
    extra: impl IntoIterator<Item = ForbiddenOperation>,
) -> Vec<ForbiddenOperation> {
    let mut forbidden: Vec<ForbiddenOperation> = match &limits.disallowed_tools {
        None => claude::DEFAULT_FORBIDDEN.to_vec(),
        Some(rules) => rules
            .iter()
            .map(|rule| ForbiddenOperation::ProviderRule {
                provider: provider.id(),
                rule: rule.clone(),
            })
            .collect(),
    };
    forbidden.extend(extra);
    forbidden.push(RIMAIA_TOOL_SURFACE);

    forbidden
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
///
/// Routed through [`AgentProvider::version_probe`] (task 032) rather than a
/// hardcoded `--version`: what to run is the provider's own vocabulary, and
/// this module stays the one place that owns the spawning.
pub async fn probe_cli(provider: &dyn AgentProvider, program: &Path) -> Result<String> {
    let plan = provider.version_probe(program);
    let mut command = Command::new(program);
    command.args(&plan.args);
    for (key, value) in &plan.env_set {
        command.env(key, value);
    }
    // Even here. "Always strip" is easier to keep true than "strip on the paths
    // that matter", and this is a child of Rimaia's like any other.
    strip_process_identity(&mut command);
    for key in &plan.env_remove {
        command.env_remove(key);
    }

    let output = command
        .output()
        .await
        .map_err(|error| missing_cli(provider, program, error.to_string()))?;

    if !output.status.success() {
        return Err(missing_cli(
            provider,
            program,
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The sentence a user reads when the prerequisite is not there.
///
/// It names what was looked for, where, and what to do — and says that Rimaia
/// runs their own installation, because "install <the CLI>" is otherwise a
/// confusing thing to be told by an app that is visibly running it.
/// No install command is quoted: there are several, they change, and a wrong one
/// is worse than none.
fn missing_cli(provider: &dyn AgentProvider, program: &Path, detail: impl AsRef<str>) -> Error {
    let detail = detail.as_ref();
    let name = provider.display_name();
    let cli = provider.default_program();
    let mut message = format!(
        "could not run the {name} CLI ({}). Rimaia drives your own installation and never \
         bundles one — install {name}, check that `{cli}` runs in a terminal, then \
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
    /// Which agent CLI this installation drives (ADR-0026, seam-contract D27.2).
    ///
    /// `Arc<dyn …>` rather than a type parameter because this struct is held in
    /// `AppState`, in the queue's shared state, in the strategy resolver's call
    /// chain and in the doctor's environment, and a parameter would propagate
    /// into all of them. `Arc` over `Box` because it keeps the `Clone` those
    /// holders depend on; every provider is zero-sized, so the `Debug` above can
    /// never print a token.
    pub provider: Arc<dyn AgentProvider>,
    /// Resolved through `PATH` by default. A path is accepted so a test can
    /// point at a stand-in, and so an operator with a non-standard install has
    /// somewhere for that to go later.
    pub program: PathBuf,
    /// How long a cancelled child is given to emit its `result` before SIGKILL.
    pub grace_period: Duration,
    /// See [`RunIntent::max_turns`](provider::RunIntent::max_turns).
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
    /// Where a repository's own forge token is read from at spawn (task 022,
    /// ADR-0020).
    ///
    /// On the config for `run_handles`' reason: it is a property of this
    /// installation, and the shell builds one and hands the same one to every
    /// starter. [`Default`] is the real OS keychain; the suite substitutes
    /// `testing::credentials::MemoryStore`, because CI has no unlocked keychain
    /// and no D-Bus and a test that needed one could not run.
    pub credentials: CredentialAccess,
}

impl Default for RunnerConfig {
    /// **Claude Code, and nothing else.** The test-only provider exists to
    /// falsify the seam, never to be reached by a default — see
    /// `the_default_provider_is_still_claude_code`.
    fn default() -> Self {
        Self {
            provider: Arc::new(claude::ClaudeProvider),
            program: PathBuf::from(claude::ClaudeProvider.default_program()),
            grace_period: DEFAULT_GRACE_PERIOD,
            max_turns: None,
            run_handles: RunHandles::default(),
            credentials: CredentialAccess::default(),
        }
    }
}

/// The session a retry continues (ADR-0011: "retries resume, they do not
/// restart").
///
/// A one-field struct rather than a bare `Option<String>`, because "some
/// string" is exactly what a caller could get wrong here and the consequence is
/// not a compile error: a run id, a task id or a stale session would all spawn
/// happily and start a *new* conversation under an old name, throwing away the
/// context the resume exists to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeSession {
    pub session_id: String,
}

/// What [`run_task`] is handed beside its claim: the two things that belong to
/// the process supervising the run rather than to the board.
///
/// Which task, who started it and whether it resumes all moved to the
/// [`Claim`] (seam-contract D31 point 5), because the board decides them.
#[derive(Debug, Clone, Default)]
pub struct RunRequest {
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

/// One spawned attempt, as [`execute`] takes it.
///
/// Borrowed rather than owned because every field already lives somewhere the
/// caller is holding — the `runs` row, the worktree, the composed prompt.
#[derive(Debug, Clone, Copy)]
pub struct Attempt<'a> {
    pub task_id: &'a str,
    pub run_id: &'a str,
    /// What Rimaia wants, in a vocabulary no provider owns. Carries the prompt
    /// and the workspace, which used to sit beside it here.
    pub intent: &'a RunIntent<'a>,
    /// What [`negotiate`](provider::negotiate) decided about this intent against
    /// this provider. Carried rather than re-derived, so the run is supervised
    /// under the same answer it was admitted under.
    pub plan: &'a RunPlan,
    pub cancel: &'a CancelSignal,
    /// What this repository's credential adds to the child, and what has to be
    /// scrubbed from everything the run writes down (task 022).
    ///
    /// A **third** environment rule, and not the same shape as the two in this
    /// module's header: rule 1 is the operator's choice and rule 2 is
    /// unconditional, where this one is conditional on the repository.
    /// [`ChildEnvironment::ambient`] is a repository with no credential, and it
    /// changes nothing at all — which is what makes adopting this feature safe
    /// one repository at a time.
    pub credentials: &'a ChildEnvironment,
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

/// The intent an implementation run is admitted under, before anything about
/// the worktree or the prompt is known.
///
/// One function for the two places that negotiate it: the manual starter,
/// before it claims, and [`run_task`], against the claim's own context. The
/// fields [`negotiate`](provider::negotiate) reads are all here; the prompt,
/// the workspace and the strategy are filled in afterwards, and it reads none
/// of them.
pub(crate) fn implementation_intent<'a>(
    context: &RunContext,
    config: &RunnerConfig,
    trigger: RunTrigger,
    run_environment: RunEnvironment,
    session: SessionIntent<'a>,
) -> RunIntent<'a> {
    RunIntent {
        session,
        permission_mode: trigger.permission_mode(),
        run_environment,
        system_append: String::new(),
        prompt: "",
        model: None,
        effort: None,
        // The board's budget, unless this installation's wiring overrides it —
        // which in production it never does (`RunnerConfig::default` leaves it
        // `None`), and which a test or a future strategy caller may.
        max_turns: config.max_turns.or(Some(context.limits.max_turns)),
        workspace: Path::new(""),
        forbidden: forbidden_operations(&context.limits, config.provider.as_ref(), []),
        // Empty: ADR-0012 gives an unattended implementation run
        // `bypassPermissions`, which approves everything the blocklist has not
        // already taken away. A list here would narrow that, which is task
        // 012's or 014's decision to make, not this line's.
        required_tools: Vec::new(),
        // An implementation run reaches Rimaia through nothing: the scoped
        // handle exists so a *planner* can answer, and ADR-0016 gives the
        // implementation run no reason to write to its own card.
        rimaia_handle: None,
    }
}

/// Runs one claimed task end to end: prepare, plan, spawn, stream, report.
///
/// **Every error after the claim gives it back.** The board's `release` moves
/// a task still `running` to `failed`, and keeps a verdict already written, so
/// nothing that returns early here can leave a card reading "running" with no
/// process behind it. Before task 036 the manual starters claimed and then
/// called this, and every `?` ahead of the `runs` row stranded the card until
/// the next launch reconciled it.
///
/// The order of the steps is the part worth reading:
///
/// 1. **The prelude reads nothing from the board.** The repository's opt-in
///    (ADR-0012) and [`negotiate`](provider::negotiate) (ADR-0026) are judged
///    against the claim's own context. The starter judged both on a preview a
///    moment earlier; if the board changed in between, this is a refusal after
///    the claim, and it releases.
/// 2. **The CLI exists**, again, because nothing about the claim says it still
///    does.
/// 3. **The worktree**, through task 007's idempotent [`worktree::prepare`],
///    holding the repository's [`preparation_lock`](InFlight::preparation_lock)
///    across it when the caller supplied a registry — see [`prepare_worktree`].
/// 4. **Two re-reads, both after the worktree exists.** `prepare` is what
///    writes `tasks.branch`, and the claim's context was read before it: the
///    planner's prompt and the implementation's both name the branch. The first
///    read feeds the strategy run, and on a resume the model and effort; the
///    second, after a planner may have written, is what the prompt is composed
///    from.
/// 5. **The board decides how it ended.** The runner reports the outcome and
///    the facts only it can know, and the board answers with the next step.
#[tracing::instrument(skip_all, fields(task_id = %claim.lease.task_id))]
pub async fn run_task(
    board: &dyn BoardPort,
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    claim: Claim,
    request: RunRequest,
) -> Result<Run> {
    let Claim {
        lease,
        trigger,
        resume,
        context,
        ..
    } = claim;
    let task_id = lease.task_id.clone();

    // The kind the board resumes travels with the session (D29 point 3). The
    // board refused review and fix points before it took the edge, so meeting
    // one here means a board that skipped that check: refused rather than
    // resumed as an implementation, which is the silent failure D29 prevents.
    let resumed = released(board, &lease, resumed_session(resume)).await?;
    released(
        board,
        &lease,
        repo::ensure_unattended_runs_allowed(&context.repository),
    )
    .await?;
    // Runner-owned, and read once, so the run is negotiated against the same
    // value it is then spawned with.
    let run_environment =
        released(board, &lease, settings::run_environment(&ctx.pool).await).await?;

    // Rimaia's conversation id, minted before anything exists so a resume works
    // even against a provider whose child dies before announcing itself
    // (ADR-0004, ADR-0026 point 5). It is also the retry-budget boundary
    // `scheduler::attempts` counts against, which is why a resume reuses the
    // one the session already has.
    let conversation = resumed.clone().unwrap_or_else(new_id);
    let home = paths.provider_home(config.provider.id(), &task_id);
    let mut intent = implementation_intent(
        &context,
        config,
        trigger,
        run_environment,
        session_intent(resumed.is_some(), &conversation, &home),
    );

    let plan = released(
        board,
        &lease,
        provider::negotiate(config.provider.capabilities(), &intent).map_err(Error::from),
    )
    .await?;
    for warning in &plan.warnings {
        tracing::warn!(%task_id, provider = %config.provider.id(), warning, "the provider could not honour part of this run");
    }

    let version = released(
        board,
        &lease,
        probe_cli(config.provider.as_ref(), &config.program).await,
    )
    .await?;
    tracing::debug!(
        %task_id,
        provider = %config.provider.id(),
        cli = %version,
        "the agent CLI prerequisite is installed",
    );

    let worktree = released(
        board,
        &lease,
        prepare_worktree(
            ctx,
            request.in_flight.as_ref(),
            &context.repository.id,
            &task_id,
        )
        .await,
    )
    .await?;

    let context = released(board, &lease, board.run_context(&lease).await).await?;

    // **A resume does not run the planner again**, and this is the easiest
    // thing in the retry loop to get wrong by omission. `strategy::resolve`
    // spawns a whole second agent process to decide how the work should be
    // done; running it per retry would pay for that decision once per wall
    // the task hits, and — worse — a second planner reading a half-finished
    // worktree could answer differently from the first, changing the model or
    // the effort *mid-session*. The attempt continues what the first one
    // started, so it continues with what the first one was given: the effective
    // values already on the row (ADR-0016's precedence chain), and no fresh
    // guidance, because the guidance the planner produced is already in the
    // session being resumed.
    let (model, effort, guidance) = if resumed.is_some() {
        (
            context.task.effective_model.clone(),
            context.task.effective_effort.clone(),
            None,
        )
    } else {
        let resolved = strategy::resolve(
            board,
            &lease,
            ctx,
            paths,
            config,
            &context,
            Path::new(&worktree.path),
            &request.cancel,
        )
        .await;

        match released(board, &lease, resolved).await? {
            strategy::Resolution::Ready {
                model,
                effort,
                guidance,
            } => (model, effort, guidance),
            // Stopped while planning. Spawning the implementation run now would
            // run the very thing the user just cancelled, so the claim goes
            // back and nothing else happens.
            strategy::Resolution::Cancelled => {
                give_back(board, &lease).await;
                return Err(Error::invalid(format!(
                    "\"{}\" was cancelled while its strategy was being planned",
                    context.task.task.title,
                )));
            }
        }
    };

    // Re-read once more: a planner that wrote a proposal changed this row, and
    // the prompt has to carry what the card now says rather than what it said
    // before the planner ran.
    let context = released(board, &lease, board.run_context(&lease).await).await?;
    let detail = &context.task;
    let repository = &context.repository;

    // ADR-0011: "retries resume, they do not restart... every retry is a resume
    // with a short continuation prompt". The composed prompt is already in the
    // session; sending it again would re-spend the tokens that produced the
    // context this attempt exists to reuse, and would read to the agent as a
    // fresh instruction to start over.
    //
    // **Which of the two this is, is the provider's ability to continue and not
    // the claim's resume point** (ADR-0026 point 6): a continuation delivered
    // into a fresh session produces an agent with no plan, no context and an
    // empty diff — a seam bug that would read as a bad model.
    let prompt = match plan.prompt_style {
        PromptStyle::Continuation => compose_resume_prompt(detail),
        PromptStyle::Composed => compose_prompt(
            &context.base_instructions,
            detail,
            repository,
            guidance.as_ref(),
            config.provider.fanout_noun(),
        ),
    };

    intent.system_append = compose_system_append(detail, repository);
    intent.model = model;
    intent.effort = effort;
    intent.prompt = &prompt;
    intent.workspace = Path::new(&worktree.path);

    // Read *before* the row is opened, because a repository whose credential is
    // configured and whose keychain item has since vanished must refuse the run
    // rather than fall back to the operator's ambient login (ADR-0020's
    // fail-closed rule) — and refusing after `start_run` would leave an attempt
    // recorded for a process that never started.
    let credentials = released(
        board,
        &lease,
        repository_credentials(config, repository).await,
    )
    .await?;

    // Minted here rather than by the row (D10): the report is idempotent by
    // id, which is what lets task 056 hold it in an outbox.
    let run_id = new_id();
    let started = board
        .start_run(
            &lease,
            StartRun {
                run_id: run_id.clone(),
                kind: RunKind::Implementation,
                session_id: conversation.clone(),
                prompt: prompt.clone(),
                // ADR-0008: what this attempt was actually branched from, taken
                // off the worktree `prepare` just resolved rather than resolved
                // again, so the row records the base the branch really has.
                base_ref: Some(worktree.base_ref.clone()),
                base_sha: worktree.base_sha.clone(),
            },
        )
        .await;
    released(board, &lease, started).await?;

    let attempt = Attempt {
        task_id: &task_id,
        run_id: &run_id,
        intent: &intent,
        plan: &plan,
        cancel: &request.cancel,
        credentials: &credentials,
    };

    let executed = execute(board, &lease, ctx, paths, config, attempt).await;

    // What the worktree was left as, measured before the finish below and for
    // every outcome — failed and cancelled included, because a failed run's
    // partial work is exactly what a morning review opens to decide what to do
    // next (task 033).
    //
    // The worktree is guaranteed to still be here. The child's process group
    // is dead, so nothing is still committing; and the task is still
    // `running`, which seam-contract D20's first guard — the one with no
    // override — makes every removal path refuse. A capture that fails anyway
    // has logged why and returned what it could.
    let capture =
        worktree::bundle::capture(Path::new(&worktree.path), worktree.base_sha.as_deref()).await;
    let transcript = TranscriptEnd::Complete {
        length: transcript_length(paths, &task_id, &run_id),
    };

    match executed {
        Ok(outcome) => {
            let usage_limited = outcome.exit_class == ExitClass::UsageLimit;

            // Before the board hears the run finished. `finish_run` publishes,
            // the publication wakes the queue, and a free slot would otherwise
            // start another task into the window this run just found closed.
            // The limit is the account's, not this task's, so the hold is right
            // whatever the board then decides about *this* task. Without a
            // reported reset there is nothing to hold until, and that window
            // stays open (the D31 amendment of 2026-10-09).
            if usage_limited {
                if let Some(reset) = outcome.usage_limit_resets_at {
                    hold_new_starts_until(ctx, &task_id, &run_id, reset).await;
                }
            }

            let exit_class = outcome.exit_class;
            let turns = outcome.num_turns;
            let cost_usd = outcome.cost_usd;
            let receipt = board
                .finish_run(
                    &lease,
                    &run_id,
                    FinishRun {
                        outcome,
                        head_sha: capture.head_sha,
                        bundle: capture.bundle,
                        window_closes_at: run_window_closes_at(ctx, &task_id, &run_id).await,
                        transcript,
                    },
                )
                .await?;

            let NextStep::Released { resume_after } = receipt.next;
            tracing::info!(
                %task_id,
                %run_id,
                exit_class = ?exit_class,
                turns,
                cost_usd,
                resume_after = resume_after.map(|at| at.to_rfc3339()),
                "run finished",
            );

            // ADR-0011's global pause, at the instant the board chose to resume
            // this task rather than the raw reported reset, so the queue does
            // not wake a minute of jitter before the task it is waiting for is
            // due. `note_usage_limit` only ever lengthens the pause.
            if usage_limited {
                if let Some(until) = resume_after {
                    hold_new_starts_until(ctx, &task_id, &run_id, until).await;
                }
            }

            Ok(receipt.run)
        }
        // Spawning or supervision itself failed. The row exists, so it is closed
        // as fatal rather than left open — an unfinished `runs` row and a task
        // stuck `running` are the same defect from two tables.
        Err(error) => {
            let finish = FinishRun {
                outcome: runner_fatal(error.to_string()),
                head_sha: capture.head_sha,
                bundle: capture.bundle,
                window_closes_at: None,
                transcript,
            };
            if let Err(nested) = board.finish_run(&lease, &run_id, finish).await {
                tracing::error!(%run_id, %nested, "could not record a failed run");
            }
            Err(error)
        }
    }
}

/// The session a claim's resume point continues, for an implementation run.
fn resumed_session(resume: Option<ResumePoint>) -> Result<Option<String>> {
    match resume {
        None => Ok(None),
        Some(ResumePoint {
            kind: RunKind::Implementation,
            session_id,
        }) => Ok(Some(session_id)),
        Some(ResumePoint { kind, .. }) => Err(kind_not_wired_yet(kind, "resumed")),
    }
}

/// Gives the claim back when `result` is an error, and passes it on.
///
/// Explicit at each step of [`run_task`] rather than a guard on drop, because
/// giving a claim back is an `await` and a report to the board, and neither
/// belongs in a destructor.
async fn released<T>(board: &dyn BoardPort, lease: &LeaseRef, result: Result<T>) -> Result<T> {
    if result.is_err() {
        give_back(board, lease).await;
    }
    result
}

/// Ends a claim that never became a finished run.
///
/// Best effort and deliberately not fatal: the caller is already returning an
/// error, and replacing it with "and also the release failed" would hide the
/// thing that actually went wrong. Startup reconciliation is the backstop
/// (ADR-0011).
async fn give_back(board: &dyn BoardPort, lease: &LeaseRef) {
    if let Err(error) = board.release(lease).await {
        tracing::error!(task_id = %lease.task_id, %error, "could not release a task whose run never finished");
    }
}

/// ADR-0011's "capped only by the run window", read when the run ends.
///
/// Read here rather than at the start, because a run that started inside a
/// window may well be finishing outside one — the operator pressed Stop, or the
/// stop time arrived while this run was allowed to finish — and the cap that
/// matters is the one in force when the board decides. Runner-owned state, so
/// it travels to the board as a fact (D31 point 4).
///
/// A failure to read it is "no window", which is the direction that keeps
/// ADR-0011's unbounded retry rather than inventing a cap out of a database
/// hiccup.
async fn run_window_closes_at(
    ctx: &ServiceContext,
    task_id: &str,
    run_id: &str,
) -> Option<DateTime<Utc>> {
    match crate::schedule::window::active(&ctx.pool).await {
        Ok(window) => window.and_then(|window| window.closes_at),
        Err(error) => {
            tracing::warn!(
                %task_id, %run_id, %error,
                "could not read the run window; reporting this run without its cap",
            );
            None
        }
    }
}

/// Raises ADR-0011's usage-limit pause to at least `until`.
///
/// Logged, never propagated: the run is over and recorded either way, and a
/// pause that could not be written costs at worst one start into a closed
/// window.
async fn hold_new_starts_until(
    ctx: &ServiceContext,
    task_id: &str,
    run_id: &str,
    until: DateTime<Utc>,
) {
    if let Err(error) = pause::note_usage_limit(ctx, until).await {
        tracing::error!(
            %task_id, %run_id, %error,
            "could not record the usage-limit pause; the queue may start into a closed window",
        );
    }
}

/// How many bytes of transcript this run left. In solo the board never copies
/// them, so this is a fact for task 056's outbox to check against rather than
/// something anything acts on yet.
fn transcript_length(paths: &AppPaths, task_id: &str, run_id: &str) -> u64 {
    std::fs::metadata(transcript_path(paths, task_id, run_id))
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// Which conversation this attempt belongs to, and where its provider keeps
/// them.
///
/// `last_announced` is `None` here and deliberately so: recovering the id a
/// previous attempt's *provider* announced means reading that attempt's
/// transcript, which only a provider that resumes `ById` and did not take
/// Rimaia's own id would need. Claude Code takes the id it is given, so there is
/// nothing to recover and nothing to persist (ADR-0026 point 5).
pub(crate) fn session_intent<'a>(
    continuing: bool,
    conversation: &'a str,
    home: &'a Path,
) -> SessionIntent<'a> {
    if continuing {
        SessionIntent::Continue {
            conversation,
            home,
            last_announced: None,
        }
    } else {
        SessionIntent::Open { conversation, home }
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
///
/// # The scratch directory
///
/// One per attempt, created here and removed on every exit path including a
/// panic (see [`Scratch`]). A provider whose only channel for something is a file
/// writes it there; a provider whose channels are all arguments never touches it.
/// It lives outside `runs/` so it is not mistaken for a transcript by the disk
/// accounting or the pruner.
pub async fn execute(
    board: &dyn BoardPort,
    lease: &LeaseRef,
    ctx: &ServiceContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    attempt: Attempt<'_>,
) -> Result<RunOutcome> {
    // The live tail reaches the board through the port (D31 point 7). The
    // stream hands each snapshot to this channel and the loop below passes it
    // on after every line, because the stream owns no reference to a board
    // and a tail it could not hand over costs nothing (D14).
    let (tails, tail_inbox) = mpsc::channel::<RunTail>();
    let mut stream = EventStream::create(ctx, paths, attempt.task_id, attempt.run_id)?
        .driven_by(config.provider.clone())
        // Before the first line is read, so nothing unredacted reaches the
        // transcript on disk or the D14 live tail. Redacting on read would
        // leave the secret in the file, which is the only copy that matters.
        .redacting(attempt.credentials.redactor.clone())
        .forwarding_tail(tails);

    // Before the child exists, so the record of what this run was allowed to be
    // is there even if the spawn fails. A warning rather than a refusal is the
    // whole point of the `AskBeforeCommands` arm — somebody chose this — and a
    // fact nobody wrote down is a fact nobody reads in the morning.
    for operation in &attempt.plan.unenforced {
        let note = format!(
            "rimaia: {} could not stop the agent from {}; this attempt ran without that \
             mitigation (ADR-0012, ADR-0026)",
            config.provider.id(),
            operation.describe(),
        );
        tracing::warn!(run_id = %attempt.run_id, note, "a mitigation was not enforced");
        if let Err(error) = stream.observe_stderr(&note) {
            tracing::warn!(run_id = %attempt.run_id, %error, "could not record an unenforced mitigation");
        }
    }

    let scratch = Scratch::create(paths, attempt.run_id)?;
    let plan = with_repository_credentials(
        config.provider.plan_spawn(attempt.intent, scratch.path())?,
        attempt.credentials,
    );
    let mut process = spawn(config, &attempt, &plan)?;
    let group = process.group;

    let mut stdin = process
        .child
        .stdin
        .take()
        .ok_or_else(|| Error::internal("the child's stdin was piped but is not there"))?;
    let prompt = plan.stdin.clone();
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
                Ok(Some(line)) => match observe_and_forward(&mut stream, &line, board, lease, &tail_inbox) {
                    Ok(Some(event)) => {
                        pull_request.observe(&event);
                        if let RunEvent::Init(init) = &event {
                            // ADR-0022's `runs.model`. Taken from `init` rather
                            // than from the flag because the flag may have been
                            // absent (the CLI's own default) or an alias, and
                            // this is the resolved name a later chart groups by.
                            observed_model.clone_from(&init.model);
                            report_applied_environment(init, attempt.intent);
                            if let Err(error) = verify_applied_posture(&attempt, init) {
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
        model: observed_model.or_else(|| attempt.intent.model.clone()),
        effort: attempt.intent.effort.clone(),
        run_environment: Some(attempt.intent.run_environment.as_str().to_string()),
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
            permission_mode = ?attempt.intent.permission_mode,
            "tool calls were refused for want of approval",
        );
    }

    Ok(outcome)
}

/// Observes one stdout line, then hands every tail snapshot it produced to the
/// board.
fn observe_and_forward(
    stream: &mut EventStream,
    line: &str,
    board: &dyn BoardPort,
    lease: &LeaseRef,
    tail_inbox: &mpsc::Receiver<RunTail>,
) -> Result<Option<RunEvent>> {
    let observed = stream.observe(line);
    while let Ok(tail) = tail_inbox.try_recv() {
        board.publish_tail(lease, tail);
    }
    observed
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

/// What this repository's credential contributes to the child, or the refusal
/// that stops the run (task 022, ADR-0020).
///
/// **Fail closed.** A repository with `credential_added_at` set whose keychain
/// item is missing — deleted in Keychain Access, restored from another machine,
/// or behind an unlock the user denied — refuses to start, naming the
/// repository. Never a silent fall back to the ambient login: a run that
/// quietly used the operator's whole GitHub account instead of the token
/// granted to one repository is the exact failure ADR-0020 exists to prevent,
/// and it is invisible in every artefact the run leaves behind.
pub(crate) async fn repository_credentials(
    config: &RunnerConfig,
    repository: &Repository,
) -> Result<ChildEnvironment> {
    if !repo::has_credential(repository) {
        return Ok(ChildEnvironment::ambient());
    }

    let secret = config.credentials.get(&repository.id).await?;
    let Some(secret) = secret else {
        return Err(Error::invalid(format!(
            "\"{}\" is configured to run with its own forge token, and this machine's keychain \
             does not have it. Re-add the token in Settings → Repositories, or remove the \
             credential — Rimaia will not fall back to your own GitHub login.",
            repository.name,
        )));
    };

    Ok(crate::credentials::inject::child_environment(
        Some(&secret),
        std::env::vars_os().map(|(name, _)| name.to_string_lossy().into_owned()),
    ))
}

/// Folds this repository's credential into the provider's plan, so the child's
/// environment is one delta applied in one place (task 022 through ADR-0026's
/// seam).
///
/// Rimaia's rule outranks the provider's: a name the credential removes or sets
/// is dropped from the provider's `env_set` first. No provider can restore the
/// operator's ambient `GH_TOKEN` or renumber `GIT_CONFIG_COUNT`, so ADR-0020's
/// fail-closed rule does not depend on every provider getting it right.
fn with_repository_credentials(mut plan: SpawnPlan, credentials: &ChildEnvironment) -> SpawnPlan {
    plan.env_set.retain(|(name, _)| {
        !credentials.remove.contains(name) && !credentials.set.contains_key(name)
    });
    plan.env_remove.extend(credentials.remove.iter().cloned());
    plan.env_set.extend(
        credentials
            .set
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    plan
}

/// A per-attempt directory a provider may write into, removed whichever way the
/// run ends.
///
/// `Drop` rather than a call at the end of [`execute`], because the interesting
/// exits are the ones nobody writes a line for: a cancelled future, a panic, the
/// app quitting. Removal failures are logged and swallowed — a leftover
/// directory is litter, and turning it into an error would replace whatever
/// actually went wrong.
#[derive(Debug)]
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn create(paths: &AppPaths, run_id: &str) -> Result<Self> {
        let path = paths.scratch_dir().join(run_id);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %self.path.display(), %error, "could not remove a run's scratch directory");
            }
        }
    }
}

/// Builds the command and starts it.
///
/// The provider decided *what* to start; everything here is how a child is
/// started safely, which is the same for all of them.
fn spawn(config: &RunnerConfig, attempt: &Attempt<'_>, plan: &SpawnPlan) -> Result<ChildProcess> {
    let workspace = attempt.intent.workspace;
    let mut command = Command::new(&config.program);
    command
        .args(&plan.args)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Rimaia's rule first, the provider's delta second: a provider may not undo
    // the identity stripping by setting one of the variables back.
    strip_process_identity(&mut command);
    // The credential's removals and additions arrive inside the plan
    // (`with_repository_credentials`). Removals first, then additions, so an
    // operator's own `GIT_CONFIG_COUNT` cannot be appended to — an off-by-one
    // there silently drops either their configuration or ours
    // (`credentials::inject`).
    for name in &plan.env_remove {
        command.env_remove(name);
    }
    for (name, value) in &plan.env_set {
        command.env(name, value);
    }
    set_process_group(&mut command);

    let child = command.spawn().map_err(|error| {
        missing_cli(
            config.provider.as_ref(),
            &config.program,
            format!("could not start it in {}: {error}", workspace.display()),
        )
    })?;
    let group = child.id();

    tracing::debug!(
        run_id = %attempt.run_id,
        provider = %config.provider.id(),
        pid = group,
        worktree = %workspace.display(),
        environment = attempt.intent.run_environment.as_str(),
        permission_mode = ?attempt.intent.permission_mode,
        // A boolean, never the token and never the login: this line is written
        // on every spawn, and "which repositories have credentials" is not a
        // fact a log file needs to accumulate.
        with_credential = !attempt.credentials.is_ambient(),
        "spawned the agent CLI",
    );

    Ok(ChildProcess {
        child,
        group,
        reaped: false,
    })
}

/// Which signal to deliver. Spelled as the CLI's own names, since that is what
/// crosses to [`KILL`].
///
/// `pub(crate)` since task 030: ADR-0025's archive hook stops a script the same
/// way a run is stopped, and a second copy of `kill -s TERM -- -<pgid>` in
/// another module is the one place this codebase least wants a near-duplicate.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Signal {
    Term,
    Kill,
}

impl Signal {
    // Read only by the `#[cfg(unix)]` half of this module: the non-unix
    // `signal_group` logs that cancellation is unimplemented and never names a
    // signal, which is the honest gap ADR-0004 records rather than a stub.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Kill => "KILL",
        }
    }
}

#[cfg(unix)]
pub(crate) fn set_process_group(command: &mut Command) {
    // Zero means "a new group whose id is the child's pid". Everything the agent
    // starts inherits it, which is what makes one signal reach the whole tree.
    command.process_group(0);
}

/// Process groups are a POSIX concept. ADR-0004 notes Windows needs its own
/// answer here (a job object) and calls it the process module's business; there
/// is no Windows target yet, so this is honestly a gap rather than a stub
/// pretending to be a port.
#[cfg(not(unix))]
pub(crate) fn set_process_group(_command: &mut Command) {}

#[cfg(unix)]
pub(crate) async fn signal_group(group: Option<u32>, signal: Signal) {
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
pub(crate) async fn signal_group(_group: Option<u32>, _signal: Signal) {
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
    match init.permission_mode {
        Some(applied) if applied != requested => Err(Error::internal(format!(
            "the agent CLI applied permission mode \"{}\" when Rimaia asked for \"{}\". The run \
             was stopped rather than continued under a posture nobody chose (ADR-0012).",
            claude::posture(applied),
            claude::posture(requested),
        ))),
        Some(_) => Ok(()),
        None => {
            tracing::warn!(
                requested = ?requested,
                "the init event named no permission mode; it could not be verified",
            );
            Ok(())
        }
    }
}

/// [`verify_permission_mode`], skipped for a provider that reports no posture at
/// all.
///
/// The distinction is the one [`PostureEcho`](provider::PostureEcho) draws: a
/// provider that *states* a posture is checked against it, always and fatally,
/// and a provider that states none had that recorded as a warning on the run
/// when it was admitted. This is not a licence to ignore a mismatch — a silent
/// provider that suddenly speaks is still checked.
fn verify_applied_posture(attempt: &Attempt<'_>, init: &InitEvent) -> Result<()> {
    if !attempt.plan.verify_posture && init.permission_mode.is_none() {
        return Ok(());
    }
    verify_permission_mode(init, attempt.intent.permission_mode)
}

/// Logs what the run actually inherited, and warns where it is not what was
/// asked for.
///
/// Neither of these stops a run. An MCP server surviving `strict_local` costs
/// tokens and hygiene, not authority — and ADR-0004's amendment asks for this to
/// be *visible* ("task 018's doctor should report the hooks and MCP servers a
/// run will inherit, so it is a visible choice rather than a surprise at 2am"),
/// which is a log line and a doctor, not a killed run.
fn report_applied_environment(init: &InitEvent, intent: &RunIntent<'_>) {
    let servers: Vec<&str> = init
        .mcp_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();

    tracing::debug!(
        tools = init.tools.len(),
        mcp_servers = servers.len(),
        model = init.model.as_deref().unwrap_or("-"),
        version = init.agent_version.as_deref().unwrap_or("-"),
        "the run's applied configuration",
    );

    if intent.run_environment == RunEnvironment::StrictLocal && !servers.is_empty() {
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
        // How each is *spelled* is the provider's, and is asserted there.
        assert_eq!(
            RunTrigger::Queued.permission_mode(),
            PermissionMode::BypassPermissions
        );
        assert_eq!(
            RunTrigger::Manual.permission_mode(),
            PermissionMode::AcceptEdits
        );
    }

    #[test]
    fn a_repository_credential_outranks_whatever_the_provider_put_in_its_plan() {
        // ADR-0020's fail-closed rule may not depend on a provider getting it
        // right: a provider setting `GH_TOKEN` or an ambient name the credential
        // removes loses to the credential, and its own variables survive.
        let plan = SpawnPlan {
            args: vec!["run".to_string()],
            env_set: vec![
                ("LEDGER_HOME".to_string(), "/scratch/home".to_string()),
                ("GH_TOKEN".to_string(), "the provider's".to_string()),
                ("GITHUB_TOKEN".to_string(), "the operator's".to_string()),
            ],
            env_remove: vec!["LEDGER_SESSION".to_string()],
            stdin: "the prompt".to_string(),
        };
        let credentials = ChildEnvironment {
            remove: vec!["GITHUB_TOKEN".to_string()],
            set: [
                ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                ("GH_TOKEN".to_string(), "the repository's".to_string()),
            ]
            .into_iter()
            .collect(),
            ..ChildEnvironment::default()
        };

        let folded = with_repository_credentials(plan, &credentials);

        assert_eq!(
            folded.env_set,
            vec![
                ("LEDGER_HOME".to_string(), "/scratch/home".to_string()),
                ("GH_TOKEN".to_string(), "the repository's".to_string()),
                ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
            ]
        );
        assert_eq!(
            folded.env_remove,
            vec!["LEDGER_SESSION".to_string(), "GITHUB_TOKEN".to_string()]
        );
        assert_eq!(folded.args, vec!["run".to_string()]);
        assert_eq!(folded.stdin, "the prompt");
    }

    #[test]
    fn a_repository_without_a_credential_leaves_the_providers_plan_untouched() {
        let plan = SpawnPlan {
            args: vec!["run".to_string()],
            env_set: vec![("GH_TOKEN".to_string(), "ambient".to_string())],
            env_remove: vec![],
            stdin: "the prompt".to_string(),
        };

        assert_eq!(
            with_repository_credentials(plan.clone(), &ChildEnvironment::ambient()),
            plan
        );
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
