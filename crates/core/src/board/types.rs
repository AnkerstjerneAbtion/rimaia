//! What crosses the board port (seam-contract D31 point 2).
//!
//! Every type here is a wire type from the day it lands, although only the
//! in-process adapter exists yet: task 052 sends these as JSON, and a type that
//! could not survive that trip would be found by a runner on another machine
//! rather than by this crate's tests. So every one derives both serde traits,
//! spells its fields `camelCase`, and none uses `deny_unknown_fields`: a newer
//! board's extra field must not make an older runner's reply unreadable (D31
//! point 6). `every_board_dto_round_trips_through_json` holds that line.
//!
//! The core types carried inside (`TaskDetail`, `Repository`, `RunOutcome` and
//! the rest) keep their own attributes. Their `Serialize` output is what
//! `src/types.ts` already reads, so they gained `Deserialize` and nothing else.
//! `Catalogue` is the one that still refuses unknown keys, deliberately; the
//! D31 amendment of 2026-10-09 leaves that to task 052.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::{Repository, Run, RunKind};
use crate::events::TeamId;
use crate::review::findings::ReviewFinding;
use crate::review_loop::EffectiveReviewConfig;
use crate::runner::outcome::RunOutcome;
use crate::runner::RunTrigger;
use crate::runs::bundle::{BundleFile, ReviewBundle};
use crate::scheduler::ResumePoint;
use crate::strategy::{Catalogue, EffectiveStrategy};
use crate::tasks::TaskDetail;
use crate::worktree::DiffStat;

/// Names one lease, never what it is for (D31 point 3).
///
/// `LeaseRef` and not `Lease`, because `scheduler::Lease` is D19's in-process
/// slot until task 042 renames it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRef {
    pub task_id: String,
    /// Always `0` before task 043, which makes it strictly increasing per task.
    pub generation: i64,
    /// The task's team, read from its row at claim (D31 point 2). The runner
    /// store cannot join the board, so the lease carries it on every report.
    /// The board narrows its context to it only after checking its own scope
    /// contains it (D31 point 13): a claim to verify, never an input.
    pub team_id: TeamId,
}

impl LeaseRef {
    /// The only lease solo can hold before task 043: generation `0`.
    pub fn solo(task_id: impl Into<String>, team_id: impl Into<TeamId>) -> Self {
        Self {
            task_id: task_id.into(),
            generation: 0,
            team_id: team_id.into(),
        }
    }
}

/// What a lease is for, in seam-contract D28's `CHECK` spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeasePurpose {
    Implementation,
    Strategy,
    Review,
    Fix,
}

impl From<RunKind> for LeasePurpose {
    /// A lease opened for a `runs` row is for that row's kind (D29 point 1).
    fn from(kind: RunKind) -> Self {
        match kind {
            RunKind::Implementation => LeasePurpose::Implementation,
            RunKind::Review => LeasePurpose::Review,
            RunKind::Fix => LeasePurpose::Fix,
        }
    }
}

/// What a runner asks the board to let it start (D31 point 4).
///
/// `Next`, the runner loop's form, is task 042's: a variant whose only body
/// was a refusal could not be told apart from a bug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClaimTarget {
    /// Run now (`continue_session: false`), or Retry now and a due retry
    /// (`true`). Only the second comes back with [`Claim::resume`] set.
    Run {
        task_id: String,
        trigger: RunTrigger,
        continue_session: bool,
    },
    /// A planner: purpose `strategy`, no `runs` row and no `run_state` edge.
    Plan { task_id: String },
}

/// A claim the board granted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    pub lease: LeaseRef,
    pub purpose: LeasePurpose,
    /// ADR-0031 point 7's permission posture. A `Plan` claim carries
    /// `Manual`: every door that plans is a person at the machine, and the
    /// planner's own posture is fixed whatever this says.
    pub trigger: RunTrigger,
    /// What a `continue_session` claim resumes, chosen by the board (D29).
    pub resume: Option<ResumePoint>,
    /// The board as it read **before** the claim's edges were taken, and so
    /// before any worktree existed for a first run. A prompt is composed from
    /// a later [`run_context`](super::BoardPort::run_context), never from this.
    pub context: RunContext,
}

/// Everything a runner needs from the board to compose and bound a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunContext {
    pub task: TaskDetail,
    pub repository: Repository,
    pub base_instructions: String,
    /// ADR-0016's precedence chain, resolved board-side.
    pub strategy: EffectiveStrategy,
    /// For the provider of the runner the adapter was built for.
    pub catalogue: Catalogue,
    pub limits: TeamLimits,
    /// What a review or fix phase is composed from (task 021). `None` only
    /// from a board that predates the loop.
    #[serde(default)]
    pub review: Option<ReviewContext>,
}

/// What a review or fix phase is composed from, read board-side under the
/// lease (seam-contract D31 point 6, task 021).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewContext {
    /// The task's override when it says something, the global text otherwise,
    /// template variables unexpanded. Empty when neither is set.
    pub instructions: String,
    pub config: EffectiveReviewConfig,
    /// The newest review's open blocking findings: what a fix is composed
    /// from, and nothing else.
    pub open_blocking: Vec<ReviewFinding>,
    /// Findings a fix run rejected, with its reason, so a reviewer does not
    /// raise them again.
    pub rejected: Vec<ReviewFinding>,
    /// The newest implementation row's session and base. Every review and fix
    /// row records the same base (D29 point 4), and a fix that resumes
    /// continues this session, never the review's (D29 point 3).
    pub implementation: Option<ImplementationBase>,
    /// The newest row's head commit and the bundle it recorded.
    pub head_sha: Option<String>,
    pub change: Option<ChangeSummary>,
    /// Whether the review phase the newest row belongs to has already
    /// recorded, which decides which resume prompt it is sent.
    pub phase_recorded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementationBase {
    pub session_id: String,
    pub base_ref: Option<String>,
    pub base_sha: Option<String>,
}

/// A recorded bundle without its patch: the review is told the size and the
/// files, and reads the patch from the worktree, where it is never truncated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSummary {
    pub diff: DiffStat,
    pub files: Vec<BundleFile>,
}

/// The board's half of what bounds a run. The runner applies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamLimits {
    /// The per-attempt turn budget (`runner::process::max_turns`).
    pub max_turns: u32,
    /// The stored blocklist, one rule per entry, or `None` when nobody has set
    /// one. `None` and `Some(vec![])` differ: the first means ADR-0012's
    /// defaults, the second an operator who turned the blocklist off.
    pub disallowed_tools: Option<Vec<String>>,
}

/// The board's answer to a heartbeat. Both lists are empty in solo.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heartbeat {
    pub fenced: Vec<LeaseRef>,
    pub cancel: Vec<String>,
}

/// Opens a `runs` row under an id the runner minted (D10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRun {
    pub run_id: String,
    pub kind: RunKind,
    pub session_id: String,
    pub prompt: String,
    pub base_ref: Option<String>,
    pub base_sha: Option<String>,
}

/// One stretch of a run's transcript, keyed by byte offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptChunk {
    pub run_id: String,
    pub offset: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptAck {
    pub stored_through: u64,
}

/// The facts a runner reports when a run ends. The board decides what they
/// mean (D31 point 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishRun {
    /// `resume_after` must be `None`: choosing it is the board's.
    pub outcome: RunOutcome,
    pub head_sha: Option<String>,
    pub bundle: Option<ReviewBundle>,
    /// When the runner's own run window closes, the cap ADR-0011 puts on a
    /// retry. Runner-owned state, so it travels as a fact.
    pub window_closes_at: Option<DateTime<Utc>>,
    pub transcript: TranscriptEnd,
}

/// Where the transcript of a finished run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TranscriptEnd {
    /// Every byte up to `length` has been sent.
    Complete { length: u64 },
    /// The runner keeps it. Task 056 gives this its meaning; in process both
    /// arms are acknowledged alike.
    KeptOnRunner,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishReceipt {
    pub run: Run,
    pub next: NextStep,
}

/// What happens after a finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum NextStep {
    /// The lease ended with the finish. `resume_after` is when the board will
    /// try the task again, or `None` when nothing follows.
    Released { resume_after: Option<DateTime<Utc>> },
    /// The lease is kept, and the next phase is a `start_run` of `kind` under
    /// it (ADR-0017, task 021). The board decided it in the same step that
    /// closed the row; the runner never chooses to continue.
    Continue { kind: RunKind },
}

/// One variant per [`BoardPort`](super::BoardPort) method.
///
/// The registry task 052's routes are wired from and the contract suite
/// iterates, so a method added without a variant fails a test rather than a
/// reviewer's attention. `run_tool` is task 055's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoardMethod {
    Preview,
    Claim,
    Heartbeat,
    RunContext,
    RecordBranch,
    StartRun,
    AppendTranscript,
    PublishTail,
    FinishRun,
    Release,
    RecordStrategy,
    RecordReviewFindings,
}

impl BoardMethod {
    pub const ALL: [BoardMethod; 12] = [
        Self::Preview,
        Self::Claim,
        Self::Heartbeat,
        Self::RunContext,
        Self::RecordBranch,
        Self::StartRun,
        Self::AppendTranscript,
        Self::PublishTail,
        Self::FinishRun,
        Self::Release,
        Self::RecordStrategy,
        Self::RecordReviewFindings,
    ];

    /// The trait method's name, which is also task 052's route segment.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Claim => "claim",
            Self::Heartbeat => "heartbeat",
            Self::RunContext => "run_context",
            Self::RecordBranch => "record_branch",
            Self::StartRun => "start_run",
            Self::AppendTranscript => "append_transcript",
            Self::PublishTail => "publish_tail",
            Self::FinishRun => "finish_run",
            Self::Release => "release",
            Self::RecordStrategy => "record_strategy",
            Self::RecordReviewFindings => "record_review_findings",
        }
    }

    /// Whether the method acts under a lease (D31 point 3). The other three
    /// are scoped by the runner the adapter was built for.
    pub const fn takes_a_lease(self) -> bool {
        !matches!(self, Self::Preview | Self::Claim | Self::Heartbeat)
    }
}
