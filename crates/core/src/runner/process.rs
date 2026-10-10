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
    BoardPort, Claim, FinishReceipt, FinishRun, ImplementationBase, LeasePurpose, LeaseRef,
    NextStep, RunContext, StartRun, TranscriptEnd,
};
use crate::consent::ceiling::{self, StrategyCeiling};
use crate::credentials::inject::ChildEnvironment;
use crate::credentials::CredentialAccess;
use crate::db::settings::{self, RunEnvironment};
use crate::db::Repository;
use crate::db::{new_id, ExitClass, Run, RunKind, RunStatus};
use crate::error::{Error, Result};
use crate::machine::{leases, Checkout, MachineContext};
use crate::mcp::{Grant, RunGrant, RunHandles, Tool, RUN_MCP_SERVER_NAME};
use crate::paths::AppPaths;
use crate::repo;
use crate::review_loop::FixSession;
use crate::runner::events::{
    transcript_path, EventStream, InitEvent, RunEvent, RunTail, TokenUsage,
};
use crate::runner::limits::{self, RunnerLimits};
use crate::runner::outcome::{PullRequestWatch, RunOutcome, SpawnedAs, Termination};
use crate::runner::prompt::{
    compose_fix_continuation, compose_fix_prompt, compose_fix_resume, compose_prompt,
    compose_resume_prompt, compose_review_prompt, compose_review_resume,
    compose_review_system_append, compose_system_append,
};
use crate::runner::provider::{
    self, claude, AgentProvider, ForbiddenOperation, PromptStyle, RimaiaHandle, RunIntent, RunPlan,
    SessionIntent, SpawnPlan,
};
use crate::runner::strategy;
use crate::runs::bundle::RunCapture;
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
    /// Facts about this attempt the runner decided before it spawned, written
    /// to its transcript's stderr beside the unenforced mitigations, so a
    /// morning reviewer reads them on the run (task 021's fix that could not
    /// resume a session is one).
    pub notes: &'a [String],
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
///
/// `context` is the claim's, never a later `run_context`'s: the base the
/// worktree is built on, and the run records, is the one the board granted
/// the claim on (task 044).
async fn prepare_worktree(
    phases: &Phases<'_>,
    in_flight: Option<&InFlight>,
    context: &RunContext,
) -> Result<Worktree> {
    let prepare = || worktree::prepare(phases.machine, phases.board, phases.lease, context);
    match in_flight {
        Some(registry) => {
            let lock = registry.preparation_lock(&context.repository.id);
            let _held = lock.lock().await;
            prepare().await
        }
        None => prepare().await,
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
///
/// `runner` is this runner's half of the limits, read through the machine
/// when the run starts; the team's half is the context's (ADR-0028 point 2).
pub(crate) fn implementation_intent<'a>(
    context: &RunContext,
    runner: &RunnerLimits,
    config: &RunnerConfig,
    trigger: RunTrigger,
    run_environment: RunEnvironment,
    session: SessionIntent<'a>,
) -> RunIntent<'a> {
    let limits = limits::effective(&context.limits, runner, config.provider.id(), []);
    RunIntent {
        session,
        permission_mode: trigger.permission_mode(),
        run_environment,
        system_append: String::new(),
        prompt: "",
        model: None,
        effort: None,
        // The stricter of the team's and the runner's budgets, unless this
        // installation's wiring overrides it — which in production it never
        // does (`RunnerConfig::default` leaves it `None`), and which a test or
        // a future strategy caller may.
        max_turns: config.max_turns.or(Some(limits.max_turns)),
        workspace: Path::new(""),
        forbidden: limits.forbidden,
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

/// Runs one claimed task end to end: the implementation, and then whatever
/// review and fix phases the board asks for (ADR-0017), under the one claim.
///
/// **Every error after the claim gives it back.** The board's `release` moves
/// a task still `running` to `failed`, and keeps a verdict already written, so
/// nothing that returns early here can leave a card reading "running" with no
/// process behind it. Before task 036 the manual starters claimed and then
/// called this, and every `?` ahead of the `runs` row stranded the card until
/// the next launch reconciled it.
///
/// # One loop, one spawn per iteration (task 021)
///
/// It enters at the claim's resume kind when the claim carries a resume, and
/// at implementation otherwise. Each phase ends in `finish_run`, and the board
/// answers: [`NextStep::Released`] ends the loop, and
/// [`NextStep::Continue`] names the next phase's kind. **The runner never
/// counts loops and never chooses to continue**: a runner that did would be a
/// second copy of ADR-0017's budget, on a machine the board does not control
/// (seam-contract D31's Why). D19's slot and the claim are held across every
/// phase and released once, by the caller, when this returns.
///
/// Between phases nothing ends through a bare `release`, which would move the
/// task to `failed` and throw away a succeeded implementation. A review or fix
/// that cannot start is recorded as a row of its kind and closed through
/// `finish_run` — see [`Phases::unspawned`] — and only a row that cannot be
/// written falls back to `release`. That, and a crash between a `Continue` and
/// the next `start_run`, are the named residual (ADR-0017's 2026-10-09
/// amendment): the board could not be written, or nothing was alive to write
/// it.
///
/// `machine` is this machine's own state (task 041): the run environment, the
/// run window and the usage-limit pause, and the clock. Everything it reads
/// from the board comes through `board`, the worktree's base included: the
/// claim carries it as `RunContext::base` (task 044).
#[tracing::instrument(skip_all, fields(task_id = %claim.lease.task_id))]
pub async fn run_task(
    board: &dyn BoardPort,
    machine: &MachineContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    claim: Claim,
    request: RunRequest,
) -> Result<Run> {
    let Claim {
        lease,
        purpose,
        trigger,
        resume,
        context,
    } = claim;
    let phases = Phases {
        board,
        machine,
        paths,
        config,
        lease: &lease,
        trigger,
        request: &request,
    };

    let mut receipt = match resume {
        Some(ResumePoint {
            kind: kind @ (RunKind::Review | RunKind::Fix),
            session_id,
        }) => {
            // A review or fix resumes as itself (D29 point 3), into the
            // worktree its implementation left; nothing is prepared again.
            let base = context
                .review
                .as_ref()
                .and_then(|review| review.implementation.clone());
            phases.run(kind, Some(session_id), base).await?
        }
        resume => {
            // The board leased a fresh start that needs ADR-0016's planner as
            // `strategy` (task 043); that, and nothing derived here, is what
            // runs the inline planner.
            let plans = purpose == LeasePurpose::Strategy;
            run_implementation(
                &phases,
                resume.map(|point| point.session_id),
                &context,
                plans,
            )
            .await?
        }
    };

    while let NextStep::Continue { kind } = receipt.next {
        receipt = phases.run(kind, None, None).await?;
    }
    Ok(receipt.run)
}

/// An implementation phase, from the claim to the board's answer.
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
async fn run_implementation(
    phases: &Phases<'_>,
    resumed: Option<String>,
    context: &RunContext,
    plans: bool,
) -> Result<FinishReceipt> {
    let Phases {
        board,
        machine,
        paths,
        config,
        lease,
        trigger,
        request,
    } = *phases;
    let task_id = lease.task_id.clone();

    // This runner's consent and its checkout: the clone the worktree is made
    // from and the credential the child spawns with (task 066).
    let checkout = released(
        board,
        machine,
        lease,
        repo::ensure_unattended_runs_allowed(machine, &context.repository).await,
    )
    .await?;
    // Runner-owned, and read once, so the run is negotiated against the same
    // value it is then spawned with.
    let run_environment = released(
        board,
        machine,
        lease,
        settings::run_environment(machine).await,
    )
    .await?;
    // This runner's half of the limits, read when the run starts by the same
    // route as the run environment, never cached across runs (ADR-0028 point
    // 2). The team's half is the claim's context.
    let runner_limits =
        released(board, machine, lease, limits::runner_limits(machine).await).await?;

    // Rimaia's conversation id, minted before anything exists so a resume works
    // even against a provider whose child dies before announcing itself
    // (ADR-0004, ADR-0026 point 5). It is also the retry-budget boundary
    // `scheduler::attempts` counts against, which is why a resume reuses the
    // one the session already has.
    let conversation = resumed.clone().unwrap_or_else(new_id);
    let home = paths.provider_home(config.provider.id(), &task_id);
    let mut intent = implementation_intent(
        context,
        &runner_limits,
        config,
        trigger,
        run_environment,
        session_intent(resumed.is_some(), &conversation, &home),
    );

    let plan = released(
        board,
        machine,
        lease,
        provider::negotiate(config.provider.capabilities(), &intent).map_err(Error::from),
    )
    .await?;
    for warning in &plan.warnings {
        tracing::warn!(%task_id, provider = %config.provider.id(), warning, "the provider could not honour part of this run");
    }

    let version = released(
        board,
        machine,
        lease,
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
        machine,
        lease,
        prepare_worktree(phases, request.in_flight.as_ref(), context).await,
    )
    .await?;

    let context = released(board, machine, lease, board.run_context(lease).await).await?;

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
            lease,
            machine,
            paths,
            config,
            &context,
            Path::new(&worktree.path),
            &request.cancel,
            plans,
        )
        .await;

        match released(board, machine, lease, resolved).await? {
            strategy::Resolution::Ready {
                model,
                effort,
                guidance,
            } => (model, effort, guidance),
            // Stopped while planning. Spawning the implementation run now would
            // run the very thing the user just cancelled, so the claim goes
            // back and nothing else happens.
            strategy::Resolution::Cancelled => {
                give_back(board, machine, lease).await;
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
    let context = released(board, machine, lease, board.run_context(lease).await).await?;
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
            context.authorship.as_ref(),
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
        machine,
        lease,
        repository_credentials(config, repository, &checkout).await,
    )
    .await?;

    // This runner's consent, read again at the last point before the spawn,
    // after the worktree and the composition (ADR-0032 point 4, task 045). The
    // prelude's read is the cheap early answer; this one is the decision,
    // because a planner may have run for minutes since, and the board is not
    // trusted to have honoured the repositories this runner listed.
    released(
        board,
        machine,
        lease,
        repo::ensure_unattended_runs_allowed(machine, repository).await,
    )
    .await?;

    // Minted here rather than by the row (D10): the report is idempotent by
    // id, which is what lets task 056 hold it in an outbox.
    let run_id = new_id();
    let started = board
        .start_run(
            lease,
            StartRun {
                run_id: run_id.clone(),
                kind: RunKind::Implementation,
                session_id: conversation.clone(),
                prompt: prompt.clone(),
                // ADR-0008: what this attempt was actually branched from, taken
                // off the worktree `prepare` built from the claim's base, never
                // the re-reads' above, so the row records the base the branch
                // really has: the label, and the commit that is authoritative.
                base_ref: Some(worktree.base.base_ref.clone()),
                base_sha: worktree.base_sha.clone(),
            },
        )
        .await;
    released(board, machine, lease, started).await?;
    leases::note_run(
        machine,
        &task_id,
        Some(&run_id),
        LeasePurpose::Implementation,
    )
    .await;

    let attempt = Attempt {
        task_id: &task_id,
        run_id: &run_id,
        intent: &intent,
        plan: &plan,
        cancel: &request.cancel,
        credentials: &credentials,
        notes: &[],
    };

    let executed = execute(board, lease, machine, paths, config, attempt).await;
    phases
        .finish(
            &run_id,
            Path::new(&worktree.path),
            worktree.base_sha.as_deref(),
            executed,
        )
        .await
}

/// What every phase of one claimed task shares.
#[derive(Clone, Copy)]
struct Phases<'a> {
    board: &'a dyn BoardPort,
    machine: &'a MachineContext,
    paths: &'a AppPaths,
    config: &'a RunnerConfig,
    lease: &'a LeaseRef,
    /// The claim's trigger, which decides every phase's posture (ADR-0012,
    /// ADR-0031 point 7).
    trigger: RunTrigger,
    request: &'a RunRequest,
}

/// The row a review or fix phase will be recorded as, whether or not it
/// spawns.
struct Pending {
    kind: RunKind,
    session_id: String,
    /// The implementation's base, which every loop row copies (D29 point 4).
    base: Option<ImplementationBase>,
}

/// A review or fix phase, ready to spawn. Owns everything its intent borrows.
struct Prepared {
    run_id: String,
    worktree: PathBuf,
    home: PathBuf,
    continuing: bool,
    permission_mode: PermissionMode,
    run_environment: RunEnvironment,
    system_append: String,
    prompt: String,
    model: Option<String>,
    effort: Option<String>,
    max_turns: Option<u32>,
    forbidden: Vec<ForbiddenOperation>,
    required_tools: Vec<&'static str>,
    handle: RimaiaHandle,
    plan: RunPlan,
    credentials: ChildEnvironment,
    /// Facts recorded on the row's transcript, such as a fix that could not
    /// resume the implementation's session.
    notes: Vec<String>,
    /// Held for its `Drop`, which revokes the token when the phase ends.
    _grant: RunGrant,
}

impl Prepared {
    fn intent<'a>(&'a self, conversation: &'a str) -> RunIntent<'a> {
        RunIntent {
            session: session_intent(self.continuing, conversation, &self.home),
            permission_mode: self.permission_mode,
            run_environment: self.run_environment,
            system_append: self.system_append.clone(),
            prompt: &self.prompt,
            model: self.model.clone(),
            effort: self.effort.clone(),
            max_turns: self.max_turns,
            workspace: &self.worktree,
            forbidden: self.forbidden.clone(),
            required_tools: self.required_tools.clone(),
            rimaia_handle: Some(self.handle.clone()),
        }
    }
}

impl Phases<'_> {
    /// One review or fix phase: composed from a fresh `run_context`, spawned
    /// through the implementation's own `plan_spawn` and [`execute`] path, so
    /// D25's credentials and redaction, D27.5's identity strip and the
    /// operator-surface denial apply to every phase without a second copy.
    ///
    /// `resumed` is the session a retry continues; `base` the implementation's
    /// base when the caller already has it, so a phase refused before the
    /// board can be read still records it.
    async fn run(
        &self,
        kind: RunKind,
        resumed: Option<String>,
        base: Option<ImplementationBase>,
    ) -> Result<FinishReceipt> {
        let mut pending = Pending {
            kind,
            session_id: resumed.clone().unwrap_or_else(new_id),
            base,
        };

        if self.request.cancel.is_cancelled() {
            return self.unspawned(&pending, cancelled_before_spawn()).await;
        }
        let prepared = match self.prepare(&mut pending, resumed.is_some()).await {
            Ok(prepared) => prepared,
            Err(refusal) => return self.unspawned(&pending, runner_fatal(refusal)).await,
        };
        if self.request.cancel.is_cancelled() {
            return self.unspawned(&pending, cancelled_before_spawn()).await;
        }

        let intent = prepared.intent(&pending.session_id);
        let started = self
            .board
            .start_run(
                self.lease,
                StartRun {
                    run_id: prepared.run_id.clone(),
                    kind,
                    session_id: pending.session_id.clone(),
                    prompt: prepared.prompt.clone(),
                    base_ref: pending.base.as_ref().and_then(|base| base.base_ref.clone()),
                    base_sha: pending.base.as_ref().and_then(|base| base.base_sha.clone()),
                },
            )
            .await;
        released(self.board, self.machine, self.lease, started).await?;
        leases::note_run(
            self.machine,
            &self.lease.task_id,
            Some(&prepared.run_id),
            kind.into(),
        )
        .await;

        let attempt = Attempt {
            task_id: &self.lease.task_id,
            run_id: &prepared.run_id,
            intent: &intent,
            plan: &prepared.plan,
            cancel: &self.request.cancel,
            credentials: &prepared.credentials,
            notes: &prepared.notes,
        };
        let mut executed = execute(
            self.board,
            self.lease,
            self.machine,
            self.paths,
            self.config,
            attempt,
        )
        .await;

        // A shell can edit what `AnyFileMutation` does not cover. A review
        // that left tracked changes behind would otherwise land clean on a
        // dirty worktree, and the next fix would commit the reviewer's edits
        // as its own. Untracked files are ignored: a test run leaves them.
        if kind == RunKind::Review {
            if let Ok(outcome) = &mut executed {
                match worktree::has_tracked_changes(&prepared.worktree).await {
                    Ok(false) => {}
                    Ok(true) => override_as_fatal(
                        outcome,
                        "The review changed the worktree without committing.".to_string(),
                    ),
                    Err(error) => override_as_fatal(
                        outcome,
                        format!("The worktree could not be checked after the review: {error}"),
                    ),
                }
            }
        }

        let base_sha = pending
            .base
            .as_ref()
            .and_then(|base| base.base_sha.as_deref());
        self.finish(&prepared.run_id, &prepared.worktree, base_sha, executed)
            .await
    }

    /// Everything that can refuse a phase before it spawns, in one place, so
    /// each refusal becomes the same recorded row. `Err` is the refusal's
    /// sentence.
    async fn prepare(
        &self,
        pending: &mut Pending,
        resumed: bool,
    ) -> std::result::Result<Prepared, String> {
        let kind = pending.kind;
        let noun = phase_noun(kind);
        let config = self.config;
        let task_id = &self.lease.task_id;

        let context = self
            .board
            .run_context(self.lease)
            .await
            .map_err(|error| format!("The {noun} could not read its task: {error}"))?;
        let review = context.review.clone().ok_or_else(|| {
            format!("The board sent no review context, so the {noun} could not be composed.")
        })?;
        if review.implementation.is_some() {
            pending.base = review.implementation.clone();
        }

        let run_environment = settings::run_environment(self.machine)
            .await
            .map_err(|error| error.to_string())?;
        // Per phase, as for the implementation: every process a runner starts
        // is held to the stricter of the two halves (ADR-0032 point 5).
        let runner_limits = limits::runner_limits(self.machine)
            .await
            .map_err(|error| error.to_string())?;
        let checkout = repo::ensure_unattended_runs_allowed(self.machine, &context.repository)
            .await
            .map_err(|error| error.to_string())?;

        let worktree = crate::machine::local::worktree_path(self.machine, task_id)
            .await
            .map_err(|error| error.to_string())?
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .ok_or_else(|| {
                format!(
                    "The task's worktree is gone, so there is nothing for the {noun} to work on."
                )
            })?;

        // The reviewer judges commits. Refused rather than reviewed, because a
        // review of a dirty worktree cannot tell its own edits from the
        // implementation's.
        if kind == RunKind::Review {
            match worktree::has_tracked_changes(&worktree).await {
                Ok(false) => {}
                Ok(true) => {
                    return Err(
                        "The worktree has uncommitted changes to tracked files, and a \
                                review judges commits, so the review was not started."
                            .to_string(),
                    )
                }
                Err(error) => {
                    return Err(format!(
                        "The worktree could not be checked before the review: {error}"
                    ))
                }
            }
        }

        // Minted before the grant, which names it: what a review records or
        // a fix resolves is attributed to this run and to no id a request
        // could name (D30 point 5).
        let run_id = new_id();
        let (grant, tool) = match kind {
            RunKind::Review => (
                Grant::Review {
                    run_id: run_id.clone(),
                },
                Tool::RecordReviewFindings,
            ),
            _ => (
                Grant::Fix {
                    run_id: run_id.clone(),
                },
                Tool::ResolveReviewFinding,
            ),
        };
        let grant = config
            .run_handles
            .grant(task_id, &self.lease.team_id, grant);
        let url = config.run_handles.endpoint_for(&grant).ok_or_else(|| {
            format!(
                "The {noun} needs Rimaia's MCP server, which is not listening (see Settings → MCP)."
            )
        })?;
        let tool_name = config
            .provider
            .tool_handle(RUN_MCP_SERVER_NAME, tool.as_str());

        // A review always opens a fresh session: that is the mechanism
        // (ADR-0017). A fix resumes only the newest implementation's session,
        // never through `resume_point` (D29 point 3), and falls back to a
        // fresh one with a note when there is none, or the provider cannot.
        let mut notes = Vec::new();
        let mut continuing = resumed;
        let mut from_implementation = false;
        if kind == RunKind::Fix && !resumed && review.config.fix_session == FixSession::Resume {
            let can_continue = config.provider.capabilities().session.can_continue();
            match review.implementation.as_ref() {
                Some(base) if can_continue => {
                    pending.session_id = base.session_id.clone();
                    continuing = true;
                    from_implementation = true;
                }
                _ => notes.push(format!(
                    "rimaia: fix_session is resume, and this fix opened a fresh session instead: {}",
                    if can_continue {
                        "the task has no implementation session to continue"
                    } else {
                        "this provider cannot continue a session"
                    }
                )),
            }
        }

        let provider_id = config.provider.id();
        let (model, effort, limits, system_append) = match kind {
            RunKind::Review => (
                review
                    .config
                    .review_model
                    .clone()
                    .or_else(|| context.strategy.model.clone()),
                review
                    .config
                    .review_effort
                    .clone()
                    .or_else(|| context.strategy.effort.clone()),
                limits::effective(
                    &context.limits,
                    &runner_limits,
                    provider_id,
                    [ForbiddenOperation::AnyFileMutation],
                ),
                compose_review_system_append(&tool_name),
            ),
            _ => (
                context.strategy.model.clone(),
                context.strategy.effort.clone(),
                limits::effective(&context.limits, &runner_limits, provider_id, []),
                compose_system_append(&context.task, &context.repository),
            ),
        };

        let mut prepared = Prepared {
            run_id,
            home: self.paths.provider_home(config.provider.id(), task_id),
            worktree,
            continuing,
            permission_mode: self.trigger.permission_mode(),
            run_environment,
            system_append,
            prompt: String::new(),
            model,
            effort,
            max_turns: config.max_turns.or(Some(limits.max_turns)),
            forbidden: limits.forbidden,
            required_tools: vec![tool.as_str()],
            handle: RimaiaHandle {
                url,
                server: RUN_MCP_SERVER_NAME,
            },
            plan: RunPlan {
                prompt_style: PromptStyle::Composed,
                verify_posture: true,
                unenforced: Vec::new(),
                warnings: Vec::new(),
            },
            credentials: ChildEnvironment::ambient(),
            notes,
            _grant: grant,
        };

        let plan = provider::negotiate(
            config.provider.capabilities(),
            &prepared.intent(&pending.session_id),
        )
        .map_err(|refusal| refusal.message)?;
        for warning in &plan.warnings {
            tracing::warn!(%task_id, provider = %config.provider.id(), warning, "the provider could not honour part of this phase");
        }

        let detail = &context.task;
        let repository = &context.repository;
        prepared.prompt = match (kind, plan.prompt_style) {
            (RunKind::Review, PromptStyle::Continuation) => {
                compose_review_resume(detail, &tool_name, review.phase_recorded)
            }
            (RunKind::Review, PromptStyle::Composed) => compose_review_prompt(
                detail,
                repository,
                context.authorship.as_ref(),
                &review,
                &tool_name,
            ),
            (_, PromptStyle::Continuation) if from_implementation => {
                compose_fix_continuation(detail, &review, &tool_name)
            }
            (_, PromptStyle::Continuation) => compose_fix_resume(detail, &tool_name),
            (_, PromptStyle::Composed) => compose_fix_prompt(
                &context.base_instructions,
                detail,
                repository,
                context.authorship.as_ref(),
                &review,
                &tool_name,
            ),
        };
        prepared.plan = plan;

        prepared.credentials = repository_credentials(config, repository, &checkout)
            .await
            .map_err(|error| error.to_string())?;
        probe_cli(config.provider.as_ref(), &config.program)
            .await
            .map_err(|error| error.to_string())?;

        // The consent read again at the last point before the spawn, after the
        // composition (ADR-0032 point 4, task 045): the read above is the early
        // answer, and this one is what a phase `Continue` started spawns on.
        repo::ensure_unattended_runs_allowed(self.machine, &context.repository)
            .await
            .map_err(|error| error.to_string())?;

        Ok(prepared)
    }

    /// Records a phase that never spawned as a row of its kind, and closes it
    /// through `finish_run`, so the exit table lands the task and the history
    /// shows why (ADR-0017's 2026-10-09 amendment).
    ///
    /// The row gets an empty transcript at its derived path, so
    /// `startup::missing_run_logs` does not report it on every launch. Only a
    /// row that cannot be written falls back to `release`.
    async fn unspawned(&self, pending: &Pending, outcome: RunOutcome) -> Result<FinishReceipt> {
        let run_id = new_id();
        let task_id = &self.lease.task_id;
        tracing::warn!(
            %task_id,
            %run_id,
            kind = ?pending.kind,
            reason = outcome.error_message.as_deref().unwrap_or("cancelled"),
            "a loop phase ended before it spawned",
        );

        let started = self
            .board
            .start_run(
                self.lease,
                StartRun {
                    run_id: run_id.clone(),
                    kind: pending.kind,
                    session_id: pending.session_id.clone(),
                    prompt: String::new(),
                    base_ref: pending.base.as_ref().and_then(|base| base.base_ref.clone()),
                    base_sha: pending.base.as_ref().and_then(|base| base.base_sha.clone()),
                },
            )
            .await;
        released(self.board, self.machine, self.lease, started).await?;
        leases::note_run(self.machine, task_id, Some(&run_id), pending.kind.into()).await;

        let transcript = transcript_path(self.paths, task_id, &run_id);
        let written = transcript
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&transcript, b""));
        if let Err(error) = written {
            tracing::warn!(%run_id, %error, "could not write an unspawned row's empty transcript");
        }

        let finished = self
            .finish_reported(
                &run_id,
                outcome,
                RunCapture::default(),
                TranscriptEnd::Complete { length: 0 },
            )
            .await;
        released(self.board, self.machine, self.lease, finished).await
    }

    /// Measures the worktree and reports how a spawned phase ended.
    ///
    /// What the worktree was left as is measured for every outcome — failed
    /// and cancelled included, because a failed run's partial work is exactly
    /// what a morning review opens to decide what to do next (task 033). The
    /// worktree is guaranteed to still be here: the child's process group is
    /// dead, and the task is still `running`, which seam-contract D20's first
    /// guard makes every removal path refuse.
    async fn finish(
        &self,
        run_id: &str,
        worktree: &Path,
        base_sha: Option<&str>,
        executed: Result<RunOutcome>,
    ) -> Result<FinishReceipt> {
        let capture = worktree::bundle::capture(worktree, base_sha).await;
        let transcript = TranscriptEnd::Complete {
            length: transcript_length(self.paths, &self.lease.task_id, run_id),
        };

        match executed {
            Ok(outcome) => {
                self.finish_reported(run_id, outcome, capture, transcript)
                    .await
            }
            // Spawning or supervision itself failed. The row exists, so it is
            // closed as fatal rather than left open — an unfinished `runs` row
            // and a task stuck `running` are the same defect from two tables.
            Err(error) => {
                let finish = FinishRun {
                    outcome: runner_fatal(error.to_string()),
                    head_sha: capture.head_sha,
                    bundle: capture.bundle,
                    window_closes_at: None,
                    transcript,
                    ceiling: next_phase_ceiling(self.machine).await,
                };
                match self.board.finish_run(self.lease, run_id, finish).await {
                    Ok(receipt) => {
                        note_receipt(self.machine, &self.lease.task_id, receipt.next).await;
                    }
                    Err(nested) => {
                        tracing::error!(%run_id, %nested, "could not record a failed run");
                    }
                }
                Err(error)
            }
        }
    }

    /// Reports an outcome, holding ADR-0011's usage-limit pause on either side
    /// of the board's answer.
    async fn finish_reported(
        &self,
        run_id: &str,
        outcome: RunOutcome,
        capture: RunCapture,
        transcript: TranscriptEnd,
    ) -> Result<FinishReceipt> {
        let task_id = &self.lease.task_id;
        let usage_limited = outcome.exit_class == ExitClass::UsageLimit;

        // Before the board hears the run finished. `finish_run` publishes, the
        // publication wakes the queue, and a free slot would otherwise start
        // another task into the window this run just found closed. The limit
        // is the account's, not this task's, so the hold is right whatever the
        // board then decides about *this* task. Without a reported reset there
        // is nothing to hold until, and that window stays open (the D31
        // amendment of 2026-10-09).
        if usage_limited {
            if let Some(reset) = outcome.usage_limit_resets_at {
                hold_new_starts_until(self.machine, task_id, run_id, reset).await;
            }
        }

        let exit_class = outcome.exit_class;
        let turns = outcome.num_turns;
        let cost_usd = outcome.cost_usd;
        let receipt = self
            .board
            .finish_run(
                self.lease,
                run_id,
                FinishRun {
                    outcome,
                    head_sha: capture.head_sha,
                    bundle: capture.bundle,
                    window_closes_at: run_window_closes_at(self.machine, task_id, run_id).await,
                    transcript,
                    ceiling: next_phase_ceiling(self.machine).await,
                },
            )
            .await?;
        note_receipt(self.machine, task_id, receipt.next).await;

        let resume_after = match receipt.next {
            NextStep::Released { resume_after } => resume_after,
            NextStep::Continue { .. } => None,
        };
        tracing::info!(
            %task_id,
            %run_id,
            kind = ?receipt.run.kind,
            exit_class = ?exit_class,
            turns,
            cost_usd,
            resume_after = resume_after.map(|at| at.to_rfc3339()),
            next = ?receipt.next,
            "run finished",
        );

        // ADR-0011's global pause, at the instant the board chose to resume
        // this task rather than the raw reported reset, so the queue does not
        // wake a minute of jitter before the task it is waiting for is due.
        // `note_usage_limit` only ever lengthens the pause.
        if usage_limited {
            if let Some(until) = resume_after {
                hold_new_starts_until(self.machine, task_id, run_id, until).await;
            }
        }

        Ok(receipt)
    }
}

/// This runner's strategy ceiling, sent with a finish for the phase a
/// `Continue` would start (task 045). A ceiling that cannot be read is sent as
/// none rather than failing a finish that has already happened: the board's
/// refusal is a cost control, not consent, and judging the ceiling again
/// before the next phase spawns is task 072's.
async fn next_phase_ceiling(machine: &MachineContext) -> StrategyCeiling {
    ceiling::strategy_ceiling(machine)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "could not read this runner's strategy ceiling for a finish");
            StrategyCeiling::default()
        })
}

fn phase_noun(kind: RunKind) -> &'static str {
    match kind {
        RunKind::Implementation => "implementation",
        RunKind::Review => "review",
        RunKind::Fix => "fix",
    }
}

/// A Cancel that arrived while no process was running, between two phases.
fn cancelled_before_spawn() -> RunOutcome {
    RunOutcome {
        exit_class: ExitClass::Cancelled,
        status: RunStatus::Cancelled,
        error_message: Some("the run was cancelled before this phase started".to_string()),
        ..runner_fatal(String::new())
    }
}

/// Gives the claim back when `result` is an error, and passes it on.
///
/// Explicit at each step of [`run_task`] rather than a guard on drop, because
/// giving a claim back is an `await` and a report to the board, and neither
/// belongs in a destructor.
async fn released<T>(
    board: &dyn BoardPort,
    machine: &MachineContext,
    lease: &LeaseRef,
    result: Result<T>,
) -> Result<T> {
    if result.is_err() {
        give_back(board, machine, lease).await;
    }
    result
}

/// Ends a claim that never became a finished run.
///
/// Best effort and deliberately not fatal: the caller is already returning an
/// error, and replacing it with "and also the release failed" would hide the
/// thing that actually went wrong. Startup reconciliation is the backstop
/// (ADR-0011), which is why this runner's record of the lease is forgotten
/// only once the board has no such lease.
async fn give_back(board: &dyn BoardPort, machine: &MachineContext, lease: &LeaseRef) {
    let released = board.release(lease).await;
    if let Err(error) = &released {
        tracing::error!(task_id = %lease.task_id, %error, "could not release a task whose run never finished");
    }
    leases::forget_released(machine, &lease.task_id, &released).await;
}

/// What this runner notes about its lease once the board answered a finish:
/// a `Released` lease is forgotten, and a `Continue` keeps it with no run and
/// the next phase's purpose, so a crash before that phase's `start_run` is
/// released at the next launch rather than finished a second time.
async fn note_receipt(machine: &MachineContext, task_id: &str, next: NextStep) {
    match next {
        NextStep::Released { .. } => leases::forget(machine, task_id).await,
        NextStep::Continue { kind } => {
            leases::note_run(machine, task_id, None, kind.into()).await;
        }
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
    machine: &MachineContext,
    task_id: &str,
    run_id: &str,
) -> Option<DateTime<Utc>> {
    match crate::schedule::window::active(machine).await {
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
    machine: &MachineContext,
    task_id: &str,
    run_id: &str,
    until: DateTime<Utc>,
) {
    if let Err(error) = pause::note_usage_limit(machine, until).await {
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
    machine: &MachineContext,
    paths: &AppPaths,
    config: &RunnerConfig,
    attempt: Attempt<'_>,
) -> Result<RunOutcome> {
    // The live tail reaches the board through the port (D31 point 7). The
    // stream hands each snapshot to this channel and the loop below passes it
    // on after every line, because the stream owns no reference to a board
    // and a tail it could not hand over costs nothing (D14).
    let (tails, tail_inbox) = mpsc::channel::<RunTail>();
    let mut stream = EventStream::forwarding(
        machine.clock.clone(),
        paths,
        attempt.task_id,
        attempt.run_id,
        tails,
    )?
    .driven_by(config.provider.clone())
    // Before the first line is read, so nothing unredacted reaches the
    // transcript on disk or the D14 live tail. Redacting on read would leave
    // the secret in the file, which is the only copy that matters.
    .redacting(attempt.credentials.redactor.clone());

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

    for note in attempt.notes {
        tracing::warn!(run_id = %attempt.run_id, note, "a fact about this attempt");
        if let Err(error) = stream.observe_stderr(note) {
            tracing::warn!(run_id = %attempt.run_id, %error, "could not record a note on the run");
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
    checkout: &Checkout,
) -> Result<ChildEnvironment> {
    if !repo::has_credential(checkout) {
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
