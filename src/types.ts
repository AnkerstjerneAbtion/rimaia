/** Mirrors `rimaia_core::ErrorCode`. Coarse on purpose: it picks a presentation,
 *  it does not reimplement backend logic. */
export type ErrorCode =
  | "database"
  | "io"
  | "not_found"
  | "invalid"
  | "internal";

/** Mirrors the payload `rimaia_core::Error` serializes to. */
export interface RimaiaError {
  code: ErrorCode;
  message: string;
}

/** Mirrors `AppInfo` in `src-tauri/src/commands/app.rs`. */
export interface AppInfo {
  appVersion: string;
  dataDir: string;
  dbFile: string;
  logsDir: string;
  /**
   * Task 018's first-run flag. It rides on this read rather than having a
   * command of its own because the opening view has to be decided before the
   * first frame — a second round trip is a flash of the board before the
   * welcome screen replaces it.
   */
  onboardingDismissed: boolean;
}

/**
 * `welcome` is deliberately **not** in `Sidebar`'s `VIEWS` array: it is a
 * destination the app can *start* on and that Settings can send you to, not a
 * permanent nav item. Task 001's no-router decision still holds — four views,
 * no URLs, no nesting, nothing to deep-link.
 */
export type View = "board" | "runs" | "settings" | "welcome";

// ---------------------------------------------------------------------------
// The preflight doctor (task 018) — mirrors `rimaia_core::doctor`.
// ---------------------------------------------------------------------------

/** Mirrors `rimaia_core::doctor::Check`. */
export type DoctorCheck =
  | "claude_cli"
  | "claude_authenticated"
  | "git"
  | "github_cli"
  | "data_directory"
  | "disk_space"
  | "repository_path"
  | "mcp_port";

/** Mirrors `rimaia_core::doctor::CheckStatus`. Only `fail` blocks queue start. */
export type DoctorStatus = "pass" | "warn" | "fail";

/** Mirrors `rimaia_core::doctor::CheckResult`. */
export interface DoctorCheckResult {
  check: DoctorCheck;
  /** `Check::label()`, sent rather than re-spelled here — there is one place
   *  the words for a check live, and it is Rust. */
  label: string;
  /**
   * The repository this row is about, for the two per-repository checks;
   * `null` for the six that describe the installation as a whole. `detail`
   * names it too — a sentence that only makes sense beside its own heading is
   * not a warning that "names the affected repository".
   */
  repository: string | null;
  status: DoctorStatus;
  detail: string;
  /** `null` only on a passing row. */
  remediation: string | null;
}

/** Mirrors `rimaia_core::doctor::DoctorReport`, in `Check::ALL` order. */
export interface DoctorReport {
  results: DoctorCheckResult[];
}

// ---------------------------------------------------------------------------
// Repositories (task 003) — mirrors `rimaia_core::db::{Repository, ...}` and
// `rimaia_core::repo::RemoteInfo`.
// ---------------------------------------------------------------------------

/** Mirrors `rimaia_core::db::Repository`. */
export interface Repository {
  id: string;
  name: string;
  path: string;
  defaultBranch: string;
  worktreeRoot: string;
  /** ADR-0012's per-repository opt-in to unattended runs. */
  allowUnattendedRuns: boolean;
  /** ADR-0010's per-repository cap: how many runs this repository will hold at
   *  once. `1` unless the user opted out, and the opt-out is deliberate —
   *  worktree isolation makes two agents in one repository safe for git and
   *  does nothing about ports, test databases and lockfiles. */
  maxConcurrency: number;
  /** RFC 3339 UTC, as sqlx writes it — see the module note above `RimaiaError`. */
  createdAt: string;
}

/**
 * Mirrors `rimaia_core::repo::RemoteInfo` — fresh on every call, never
 * cached (there is no stored remote URL; `git remote get-url` answers it
 * every time).
 */
export interface RemoteInfo {
  remoteUrl: string | null;
  /**
   * `null` when there is no remote to open a PR against. `false` is the
   * warning case — `gh` missing, or not authenticated for the remote's host
   * — which base instructions that ask for a PR read as a reason to skip
   * that step rather than a reason to fail the run.
   */
  ghReady: boolean | null;
}

/** What [`registerRepository`](./commands) sends. Mirrors `NewRepository`. */
export interface RegisterRepositoryInput {
  path: string;
  name?: string;
  worktreeRoot?: string;
}

/**
 * What [`updateRepository`](./commands) sends. Mirrors `RepositoryPatch` — a
 * field left out leaves that column unchanged; there is no "clear" for any
 * of these, they are all `NOT NULL`.
 */
export interface UpdateRepositoryInput {
  name?: string;
  defaultBranch?: string;
  worktreeRoot?: string;
}

// ---------------------------------------------------------------------------
// Tasks (task 004) — mirrors `rimaia_core::db::{Task, TaskLink, Run, ...}`
// and `rimaia_core::tasks::TaskDetail`.
// ---------------------------------------------------------------------------

/** Mirrors `rimaia_core::db::BoardColumn` (ADR-0007). Where a card is in the
 *  user's process — four, and there is no fifth. */
export type BoardColumn = "not_ready" | "ready" | "in_review" | "done";

/** Mirrors `rimaia_core::db::RunState` (ADR-0007). Where a task is in the
 *  machine's process — separate from `BoardColumn` on purpose. `interrupted`
 *  is deliberately absent (seam-contract D9); read it off the task's last
 *  run instead. */
export type RunState =
  | "idle"
  | "queued"
  | "running"
  | "blocked"
  | "waiting_retry"
  | "failed"
  | "cancelled";

/** Mirrors `rimaia_core::db::StrategyMode` (ADR-0016). */
export type StrategyMode = "default" | "manual" | "planned";

/** Mirrors `rimaia_core::db::StrategySource` (ADR-0016). Whose decision a run
 *  will execute. "Accepted" is this flipping `"planner"` → `"user"`
 *  (seam-contract D17.7) — there is no `accepted` column to read instead. */
export type StrategySource = "user" | "planner";

/**
 * Mirrors `rimaia_core::strategy::StrategyOrigin` (ADR-0016, seam-contract
 * D17.6). Which link of the precedence chain — task, then repository, then
 * global — answered for a value.
 *
 * `"claude_code"` is the fourth on purpose: nothing configured anywhere means
 * no `--model` reaches the command line and the CLI picks for itself, which is
 * an answer with consequences and not the same as a global default.
 */
export type StrategyOrigin = "task" | "repository" | "global" | "claude_code";

/** Mirrors `rimaia_core::db::RunStatus` (ADR-0013). The coarse lifecycle of
 *  one attempt, as the Runs view queries it. */
export type RunStatus = "running" | "succeeded" | "failed" | "cancelled" | "interrupted";

/** Mirrors `rimaia_core::db::ExitClass` (ADR-0011). Why a run stopped. */
export type ExitClass = "success" | "usage_limit" | "transient" | "interrupted" | "fatal" | "cancelled";

/** Mirrors `rimaia_core::db::MutationSource` (ADR-0019). Which door a mutation
 *  came through: the board, an MCP tool call from another Claude Code session,
 *  or the run scheduler. */
export type MutationSource = "ui" | "mcp" | "system";

/** Mirrors `rimaia_core::db::Task`. */
export interface Task {
  id: string;
  repositoryId: string;
  title: string;
  /** `null` is precisely `not_ready`'s "captured, plan missing or incomplete". */
  plan: string | null;
  extraInstructions: string | null;
  column: BoardColumn;
  /** Fractional priority within `(repositoryId, column)` — board order *is*
   *  execution order (ADR-0007). Never computed on the frontend; sent as
   *  `beforeId`/`afterId` to `moveTask` and read back from the result. */
  position: number;
  runState: RunState;
  branch: string | null;
  worktreePath: string | null;
  strategyMode: StrategyMode;
  /** What the *card* asks for. Read {@link TaskSummary.effectiveModel} to
   *  render what a run would actually spawn with: a task in resolved
   *  `"default"` mode ignores this column entirely (seam-contract D17.6). */
  model: string | null;
  effort: string | null;
  /** The {@link StrategyPlan} envelope as stored text — `JSON.parse` it; the
   *  column is `TEXT` and the backend hands it over verbatim (D17.3). */
  strategyPlan: string | null;
  strategySource: StrategySource | null;
  strategyUpdatedAt: string | null;
  createdAt: string;
  updatedAt: string;
  /** Creation provenance, never rewritten (ADR-0019): a task created on the
   *  board and later patched over MCP still reads `ui`. */
  source: MutationSource;
}

/** Mirrors `rimaia_core::db::TaskLink`. One `{label, url}` external reference. */
export interface TaskLink {
  id: string;
  taskId: string;
  label: string;
  url: string;
  position: number;
}

/** Mirrors `rimaia_core::db::Run`, holding only what the UI queries — the
 *  full transcript is a JSONL file (ADR-0013), not this row. */
export interface Run {
  id: string;
  taskId: string;
  attempt: number;
  status: RunStatus;
  sessionId: string;
  /** The composed prompt verbatim (ADR-0009). */
  prompt: string;
  startedAt: string;
  endedAt: string | null;
  exitClass: ExitClass | null;
  errorMessage: string | null;
  numTurns: number | null;
  costUsd: number | null;
  logPath: string;
  prUrl: string | null;
  resumeAfter: string | null;
  /** The branch this attempt was created from (ADR-0008) — the repository's
   *  default branch, or a dependency's branch when the task has one. */
  baseRef: string | null;
  /** ADR-0022's capture columns. **`null` means "not recorded", never zero**
   *  (seam-contract D18): every row written before the capture migration
   *  honestly has none, and a run that dies before its terminal `result` event
   *  never learns its token counts. Nothing here is ever backfilled, and a view
   *  that averages a `null` as zero is lying about the past. */
  model: string | null;
  effort: string | null;
  runEnvironment: string | null;
  inputTokens: number | null;
  outputTokens: number | null;
  cacheReadTokens: number | null;
  cacheCreationTokens: number | null;
}

/**
 * The three fields seam-contract D12's 2026-08-28 amendment adds to *both*
 * projections: what a run would actually spawn with, and who decided it.
 *
 * Computed in Rust by `strategy::effective_strategy` after the query, never
 * here. The precedence chain is a business rule, and a TypeScript copy of it
 * would be a second implementation free to disagree with the first — the whole
 * reason these ride along on a read instead of being derived from
 * {@link Task.model} and a settings fetch.
 *
 * Declared once and mixed into {@link TaskSummary} and {@link TaskDetail}
 * rather than into {@link Task}: they are not columns, and a card is not
 * allowed to think they are.
 */
export interface EffectiveStrategyFields {
  /** `null` when nothing anywhere names a model — see
   *  {@link StrategyOrigin}'s `"claude_code"`. */
  effectiveModel: string | null;
  effectiveEffort: string | null;
  /** Which link of the chain answered. The card renders an inherited value
   *  differently from a chosen one, and the winning link cannot be
   *  reconstructed from the value alone. */
  effectiveOrigin: StrategyOrigin;
}

/**
 * Mirrors `rimaia_core::tasks::TaskDetail`. Rust `#[serde(flatten)]`s the
 * task onto this shape, so the wire object is `Task`'s own fields plus these
 * three siblings — not a nested `{ task: {...} }` — which is why this
 * `extends Task` rather than nesting one.
 */
export interface TaskDetail extends Task, EffectiveStrategyFields {
  links: TaskLink[];
  /** Outgoing edges only — what this task depends on, not what depends on it. */
  dependsOn: string[];
  lastRun: Run | null;
}

/**
 * Mirrors `rimaia_core::tasks::LastRunSummary` — the four fields of a `Run`
 * a card draws, not the row. `interrupted` reaches the board through
 * `exitClass` and nowhere else (seam-contract D9).
 */
export interface LastRunSummary {
  status: RunStatus;
  exitClass: ExitClass | null;
  /** `null` while the attempt is still in flight. */
  endedAt: string | null;
  /** When ADR-0011's retry policy scheduled the next attempt, or `null` for an
   *  attempt nothing will follow (seam-contract D12's 2026-09-03 amendment).
   *  This is what a `waiting_retry` badge shows the time of — a card that says
   *  only "Waiting for retry" cannot tell a task coming back at 06:12 from one
   *  that is stuck. */
  resumeAfter: string | null;
}

/**
 * Mirrors `rimaia_core::tasks::TaskSummary`, what [`listTasks`](./commands)
 * returns (seam-contract D12). Flattened on the Rust side for the reason
 * `TaskDetail` is, so this `extends Task` rather than nesting one.
 *
 * The card renders from this; the panel renders from `TaskDetail`. That split
 * is what keeps a fifty-card board one query instead of fifty.
 */
export interface TaskSummary extends Task, EffectiveStrategyFields {
  linkCount: number;
  dependencyCount: number;
  /** Reserved for task 011 and a constant `false` until it lands — the card
   *  renders it now so that task 011 changes one backend query and nothing
   *  else (seam-contract D12). */
  blockedByIncomplete: boolean;
  lastRun: LastRunSummary | null;
}

/** What [`createTask`](./commands) sends. Mirrors `NewTaskLink`, and also
 *  what [`addTaskLink`](./commands) sends for one link added afterwards. */
export interface NewTaskLinkInput {
  label: string;
  url: string;
}

/** What [`createTask`](./commands) sends. Mirrors `NewTask`. */
export interface NewTaskInput {
  repositoryId: string;
  title: string;
  plan?: string;
  extraInstructions?: string;
  /** Omitted takes ADR-0007's default: `not_ready`. */
  column?: BoardColumn;
  links?: NewTaskLinkInput[];
}

/** What [`listTasks`](./commands) sends. Mirrors `TaskFilter` — a field left
 *  out matches everything; combining fields narrows the result. */
export interface TaskFilterInput {
  repositoryId?: string;
  column?: BoardColumn;
  runState?: RunState;
}

/**
 * A field on a nullable task column, as [`updateTask`](./commands) sends it.
 * Mirrors `rimaia_core::tasks::Patch<T>` the only way JSON can: omit the key
 * to leave it unset, send `null` to clear it, send a value to set it. See
 * `commands/tasks.rs::opt_patch` on the Rust side for the other half of this
 * contract.
 */
export type PatchField<T> = T | null;

/** What [`updateTask`](./commands) sends. Mirrors `TaskPatch` — `repositoryId`
 *  and `title` are plain optional strings (never "clear", both columns are
 *  `NOT NULL`). */
export interface TaskPatchInput {
  /**
   * Re-files the task under another repository. Legal only while the task has
   * no worktree and no runs (seam-contract D13): `update_task` refuses
   * otherwise, with a message naming the worktree or the run count, and the
   * panel renders that text beside the selector it has disabled. The refusal
   * is the rule; the disabled control is a courtesy on top of it.
   */
  repositoryId?: string;
  title?: string;
  /**
   * Task 020's mode, which has no command of its own on purpose: sending it
   * here is what makes the panel and the `update_task` MCP tool reach the same
   * rule (ADR-0006). A plain optional, never a {@link PatchField} — the column
   * is `NOT NULL` and `"default"` is how it spells "no opinion".
   *
   * Setting {@link model} or {@link effort} to a value flips the mode to
   * `"manual"` on the backend, and clearing both flips it back to `"default"`
   * (seam-contract D17.6); the panel does not have to send both.
   */
  strategyMode?: StrategyMode;
  plan?: PatchField<string>;
  extraInstructions?: PatchField<string>;
  model?: PatchField<string>;
  effort?: PatchField<string>;
}

/** What [`updateTaskLink`](./commands) sends. Mirrors `TaskLinkPatch` —
 *  `label` and `url` are both `NOT NULL`, so plain optionals are enough. */
export interface TaskLinkPatchInput {
  label?: string;
  url?: string;
}

// ---------------------------------------------------------------------------
// Settings (task 006) — mirrors `rimaia_core::db::settings` and
// `rimaia_core::runner::prompt` (ADR-0009, ADR-0012, seam-contract D3).
// ---------------------------------------------------------------------------

/**
 * Mirrors `rimaia_core::db::RunEnvironment` (ADR-0004's amendment). How much
 * of the operator's own Claude Code configuration a run inherits. An absent
 * setting reads as `"inherit"` — there is no third "unset" spelling.
 */
/**
 * What finished runs on this machine have cost.
 *
 * Median rather than mean: run costs are wildly skewed, and one expensive run
 * should not decide what the settings panel tells the user about every run.
 * `medianRunCostUsd` is null until something has finished and reported a cost.
 */
export interface RunCostSummary {
  medianUsd: number | null;
  sampleSize: number;
}

export type RunEnvironment = "inherit" | "strict_local";

// ---------------------------------------------------------------------------
// Execution strategy (task 020) — mirrors `rimaia_core::strategy` and the
// `strategy_plan` envelope seam-contract D17.3 fixes (ADR-0016).
//
// Two spellings meet in this section, and the difference is deliberate. The
// command payloads below are camelCase like everything else on this boundary;
// {@link Catalogue} and {@link StrategyPlan} are **stored JSON documents** —
// one hand-edited in a textarea, one written by a planner and read by task 021
// — so their keys are the stored spelling, `max_turns` and `num_turns` and
// all, and renaming them on the way through would make Settings show a
// document that does not match the row.
// ---------------------------------------------------------------------------

/**
 * Mirrors `rimaia_core::strategy::CatalogueEntry`. One choice in a dropdown:
 * `id` reaches `--model` or `--effort` verbatim, `label` draws the option.
 */
export interface CatalogueEntry {
  id: string;
  label: string;
}

/**
 * Mirrors `rimaia_core::strategy::PlannerBudget` — the strategy run's own
 * budget, configuration for the same reason the model list is.
 *
 * `model` and `effort` absent means *no flag at all*: the CLI chooses. That is
 * not the same as the built-in haiku/low pair, which is what an unedited
 * catalogue reads as.
 */
export interface PlannerBudget {
  model: string | null;
  effort: string | null;
  /** Stored spelling — see this section's note. */
  max_turns: number;
}

/**
 * Mirrors `rimaia_core::strategy::Catalogue`, as
 * {@link getStrategyCatalogue} in `./lib/commands` returns it parsed.
 *
 * An explicitly empty list means no choices, **not** the built-in list — an
 * operator turning a dropdown off is a thing they are allowed to do. A model a
 * task already names but the catalogue no longer lists is still spawned
 * verbatim; the panel shows it with a hint rather than dropping it.
 */
export interface Catalogue {
  models: CatalogueEntry[];
  efforts: CatalogueEntry[];
  planner: PlannerBudget;
}

/**
 * What {@link getStrategyCatalogue} returns: the same key read three ways,
 * because Settings' textarea and the panel's dropdowns want different things
 * out of it and a second round trip would let them disagree. Mirrors
 * `StrategyCatalogueView` in `src-tauri/src/commands/strategy.rs`.
 */
export interface StrategyCatalogueView {
  /** The parsed value the dropdowns render — already the built-in list if the
   *  stored text will not parse, since that tolerance rule is the backend's. */
  catalogue: Catalogue;
  /** The stored text verbatim, so the editor reopens on the user's own key
   *  order and indentation. The built-in JSON while the key is unset, which is
   *  every fresh install. */
  json: string;
  /** What "Restore defaults" writes. Crosses the boundary rather than being
   *  retyped here: a second copy of the default list is a second thing to
   *  update when a model is added. */
  defaultJson: string;
}

/**
 * Mirrors `rimaia_core::strategy::StrategyDefaults` — one repository's
 * default, or the global one under it. The same shape at both levels, because
 * one precedence chain reads both.
 *
 * A `"default"` mode with no model and no effort *is* "no opinion", and reads
 * identically to a key that was never written. There is nothing to clear.
 */
export interface StrategyDefaults {
  mode: StrategyMode;
  /**
   * Optional in both directions, and that is the Rust shape rather than a
   * convenience: the backend omits an absent model from the JSON entirely
   * (`skip_serializing_if`), so a level that has no opinion arrives as
   * `{ "mode": "default" }` and nothing else. Sending `null` and leaving the
   * key out both mean "no opinion" on the way back in.
   */
  model?: string | null;
  effort?: string | null;
}

/**
 * Mirrors `rimaia_core::strategy::StrategyApproval`. Whether a proposal runs
 * on its own or waits for a human.
 *
 * **Stored and rendered by task 020, read by nothing yet** — the gate lands
 * after tasks 011 and 012. It is settable now because a radio group that
 * forgets its answer on relaunch is worse than no radio group.
 */
export type StrategyApproval = "automatic" | "manual";

/** Whether the planner produced a proposal or failed trying. A `"failed"`
 *  envelope is written on every planner failure — non-zero exit, `max_turns`,
 *  a usage limit, or a run that never called the tool — and is what suppresses
 *  a re-plan on every later queue pass (seam-contract D17.8). */
export type StrategyPlanStatus = "proposed" | "failed";

/** Whether the planner thinks the work fans out. Rimaia never orchestrates
 *  the phases; the guidance is injected into the implementation prompt and the
 *  agent runs them itself with its own subagents (ADR-0016). */
export type StrategyWorkflow = "single_agent" | "multi_agent";

/** One phase of a {@link StrategyPlan}. Stored spelling throughout. */
export interface StrategyPlanPhase {
  name: string;
  model: string | null;
  effort: string | null;
  agents: number;
  summary: string;
}

/**
 * The planner's own accounting, carried inside the envelope because a strategy
 * run deliberately gets **no `runs` row** (seam-contract D17.5) — the panel
 * still has to render "Planner: 4 turns, $0.03", and there is no row to read
 * it off.
 */
export interface StrategyPlanRun {
  session_id: string | null;
  num_turns: number | null;
  cost_usd: number | null;
  /** Why the planner failed, on a `"failed"` envelope. `null` otherwise. */
  error: string | null;
}

/**
 * The `strategy_plan` envelope, version 1, as seam-contract D17.3 fixes it —
 * `JSON.parse`d from {@link Task.strategyPlan}, which the backend hands over as
 * text.
 *
 * Every field a failed envelope has no answer for is optional here rather than
 * nullable-and-required: this is a document being parsed, not a command's
 * response, and a panel that renders a failure must not depend on the writer
 * having emitted a key it had nothing to put in.
 */
export interface StrategyPlan {
  version: number;
  status: StrategyPlanStatus;
  model?: string | null;
  effort?: string | null;
  workflow?: StrategyWorkflow;
  phases?: StrategyPlanPhase[];
  rationale?: string | null;
  run?: StrategyPlanRun;
}

// ---------------------------------------------------------------------------
// Runs (task 008) — mirrors `rimaia_core::runner::events` (ADR-0004,
// ADR-0011, ADR-0013, seam-contract D14). `Run` itself is defined above,
// alongside `Task`, because it was already needed by `TaskDetail.lastRun`.
// ---------------------------------------------------------------------------

/** Mirrors `rimaia_core::runner::events::ToolCall`. A bounded, one-line
 *  rendering of a tool call's most identifying argument — never the raw
 *  input, which for a `Write` call is an entire file. */
export interface ToolCall {
  /** The `tool_use_id`, so a matching result can be correlated with it. */
  id: string;
  name: string;
  /** `null` when the tool's input carried none of the recognised keys —
   *  the name alone is still shown. */
  detail: string | null;
}

/**
 * Mirrors `rimaia_core::runner::events::RunTail` (seam-contract D14). The
 * payload of the `runs:tail` event and of
 * [`getRunTail`](./lib/commands) — a live view of a run in progress, never
 * the source of truth for anything persisted. If this and a {@link Run} row
 * ever disagree, the row wins.
 */
export interface RunTail {
  runId: string;
  elapsedMs: number;
  /** Approximate while the run is in flight — replaced by the `Run` row's
   *  own `numTurns` once it ends. */
  turns: number;
  currentTool: ToolCall | null;
  lastAssistantText: string | null;
}

// ---------------------------------------------------------------------------
// The run queue (task 009) — mirrors `rimaia_core::scheduler` (ADR-0010,
// ADR-0007, ADR-0012).
// ---------------------------------------------------------------------------

/**
 * Mirrors `rimaia_core::scheduler::QueueState`. The queue's whole on/off
 * switch — not a {@link RunState}, which is one *task's* place in the
 * machine's process, and not a fifth thing to track per repository. Exactly
 * two values, because Start/Resume both mean {@link "running"} and
 * Pause/Stop both mean {@link "paused"}. Persisted in `settings` under
 * `queue_state`; an absent key reads `"paused"`, the direction that costs a
 * queue that does not start rather than one that starts unasked.
 */
export type QueueState = "running" | "paused";

/**
 * Mirrors `rimaia_core::scheduler::SkipReason`. Why the queue passes over a
 * `ready` task it can otherwise see — a badge's worth of explanation, never
 * a severity. Every value but `"unattended_runs_not_allowed"` clears on its
 * own; that one is the only reason the user has to act on before the queue
 * can ever start the task (ADR-0012).
 *
 * `"waiting_for_retry"` is task 014's, and it is deliberately not folded into
 * `"already_in_flight"`: nothing is running, nothing is wrong, and the card can
 * say *when* it comes back (seam-contract D22).
 */
export type SkipReason =
  | "unattended_runs_not_allowed"
  | "dependency_not_satisfied"
  | "already_in_flight"
  | "waiting_for_retry"
  | "needs_attention";

/**
 * Mirrors `rimaia_core::scheduler::QueueEntry`. One `ready` task as the queue
 * sees it — ids and a title, never a row, since whoever renders this already
 * re-reads the board on `tasks:changed` (ADR-0018's argument for ids-only
 * events, applied to a projection).
 */
export interface QueueEntry {
  taskId: string;
  title: string;
  repositoryId: string;
  /** Where in the queue this task sits, counting only what the queue will
   *  actually start: `1` is next up. `null` for a task the queue is passing
   *  over — see {@link skip}. This is what a board card's "queued position"
   *  badge reads. */
  queuePosition: number | null;
  /** `null` when the queue would start this task right now. */
  skip: SkipReason | null;
  /** When this task's next attempt becomes due. Populated **only** for a task
   *  in `waiting_retry`, so an old deadline on a task that has since been
   *  started again by hand does not read as a pending resume. */
  resumeAfter: string | null;
}

/**
 * Mirrors `rimaia_core::scheduler::QueueStatus` — everything the Runs view
 * asks the queue about, in one read: {@link getQueueStatus} in `./lib/commands`.
 */
export interface QueueStatus {
  state: QueueState;
  /** Every task this process has a `claude` child for right now, in a stable
   *  order — the queue's own runs and any a button started, since they share
   *  one registry. A list rather than the single id this was: task 012 fills
   *  more than one slot, and a wire field that changed shape once a setting was
   *  flipped would be worse than one that is always a list. */
  runningTaskIds: string[];
  /** Every `ready` task in board order, with the reason the queue will pass
   *  over each one it cannot start. Re-read fresh on every call — never a
   *  snapshot from when the queue was started — so a card dragged to the top
   *  mid-queue shows up here as what runs next before the queue itself gets
   *  there. */
  plan: QueueEntry[];
  /** Why the loop's last pass could not be completed, if it couldn't. `null`
   *  once a later pass gets all the way through. The one failure no
   *  {@link SkipReason} can name — a missing `claude` fails before any task
   *  is even chosen, so nothing on the board explains it, and without this
   *  {@link state} still read `"running"` over a full {@link plan} while
   *  nothing was happening. */
  lastStepError: string | null;
  /** When ADR-0011's global usage-limit hold lifts, or `null` when there is
   *  none. The other way a queue that reads `"running"` over a full
   *  {@link plan} can be starting nothing — surfaced for the same reason
   *  {@link lastStepError} is, because a hold the operator cannot see is one
   *  they will debug as a bug. */
  usageLimitPauseUntil: string | null;
}

/**
 * Mirrors `rimaia_core::db::ScheduleMode`. How many runs the queue works at
 * once (ADR-0010's Modes): one at a time, or up to {@link
 * RunCapacity.maxConcurrency}.
 *
 * Not `StrategyMode`, which is ADR-0016's per-task model and effort selection.
 * The two share the word "mode" and nothing else.
 */
export type ScheduleMode = "sequential" | "parallel";

/**
 * Mirrors `rimaia_core::scheduler::capacity::RunCapacity` — what {@link
 * getRunCapacity} answers with, and what both of its setters answer with too.
 */
export interface RunCapacity {
  mode: ScheduleMode;
  /** The **stored** limit, which is deliberately not what `"sequential"`
   *  resolves to: sequential always runs exactly one, and the number the user
   *  chose is remembered rather than overwritten, so flipping back to
   *  `"parallel"` restores it. A control that showed `1` here would make the
   *  setting look forgotten every time the mode was switched. */
  maxConcurrency: number;
  /** The most runs Rimaia will ever supervise, whatever {@link
   *  maxConcurrency} says — `CONCURRENCY_CEILING`, a Rust constant. Sent over
   *  the wire rather than duplicated here, because a hard-coded copy of it in
   *  TypeScript is a second version of a number whose whole purpose is that
   *  there is one. */
  ceiling: number;
}

// ---------------------------------------------------------------------------
// Worktrees (task 007) — mirrors `rimaia_core::worktree` (ADR-0005).
// ---------------------------------------------------------------------------

/** Mirrors `rimaia_core::worktree::DiffStat`. */
export interface DiffStat {
  filesChanged: number;
  insertions: number;
  deletions: number;
}

/**
 * Mirrors `rimaia_core::worktree::WorktreeStatus` — what the task detail
 * panel's worktree section shows, recomputed fresh from git on every read.
 * Every numeric field is zero and `dirty` is `false` whenever `exists` is
 * `false`: a task that has never run, or one whose branch was deleted, reads
 * as "no worktree yet" rather than as five fields to unwrap.
 */
export interface WorktreeStatus {
  taskId: string;
  /** The directory is on disk **and** git still lists it as a worktree of
   *  this repository on this branch. */
  exists: boolean;
  /** What the row records, whether or not it still resolves — shown so the
   *  user can go and look, including when it is gone. */
  path: string | null;
  branch: string | null;
  baseRef: string;
  ahead: number;
  behind: number;
  /** Uncommitted work in the worktree: modified, staged or untracked alike. */
  dirty: boolean;
  /** Commits on the branch that are not on the base — the same number as
   *  `ahead`, computed from one `rev-list` on the Rust side. */
  commitCount: number;
  diff: DiffStat;
}

/** Mirrors `rimaia_core::worktree::FileDiffStat` (task 015) — one file's
 *  insertions and deletions out of a {@link DiffSummary}. Both counts are
 *  `null` for a binary file, which `git diff --numstat` reports as `-` in
 *  both columns; that is a different fact from "zero lines changed". */
export interface FileDiffStat {
  path: string;
  insertions: number | null;
  deletions: number | null;
}

/** Mirrors `rimaia_core::worktree::CommitSummary` (ADR-0013) — one commit on
 *  a task's branch, as a review lists it. */
export interface CommitSummary {
  sha: string;
  shortSha: string;
  subject: string;
  author: string;
  committedAt: string;
}

/**
 * Mirrors `rimaia_core::worktree::DiffSummary` (ADR-0013, task 015): the
 * diff and the commits a run detail view opens with, second only to the
 * run's own outcome. Scoped to the branch, not to one attempt — every
 * attempt of a task shares one branch (ADR-0005), so this is the same
 * summary regardless of which attempt's detail view fetched it.
 */
export interface DiffSummary {
  taskId: string;
  branch: string | null;
  baseRef: string;
  diff: DiffStat;
  /** The same diff, broken out per file — `diff`'s totals are a sum over
   *  this list. */
  files: FileDiffStat[];
  /** Newest first. */
  commits: CommitSummary[];
}

// ---------------------------------------------------------------------------
// Run history and the transcript viewer (task 015) — mirrors
// `rimaia_core::runs` and `rimaia_core::runs::transcript`
// (ADR-0013, seam-contract D14).
// ---------------------------------------------------------------------------

/**
 * Mirrors `rimaia_core::runs::RunListEntry` — one row of the global Runs
 * view's history, filterable by repository, outcome and date range. A
 * per-task history list has no need of `taskTitle`/`repositoryName` — it
 * already knows which task it is looking at — so {@link listRunsForTask}
 * returns bare {@link Run} rows instead of this.
 */
export interface RunListEntry extends Run {
  taskTitle: string;
  repositoryId: string;
  repositoryName: string;
  /** Computed fresh on every read, never trusted from a cache — ADR-0013's
   *  "a runs row pointing at a missing file is marked, not trusted". `false`
   *  is what "Reveal raw log" and the transcript viewer disable themselves on,
   *  rendering "log unavailable" instead of erroring. */
  logAvailable: boolean;
}

/** What {@link listRuns} sends. Mirrors `rimaia_core::runs::RunFilter` via
 *  `commands::runs::RunFilterInput` — a field left out matches everything. */
export interface RunFilterInput {
  repositoryId?: string;
  status?: RunStatus;
  /** Matches a run started at or after this instant (RFC 3339). */
  since?: string;
  /** Matches a run started at or before this instant (RFC 3339). */
  until?: string;
}

/**
 * Mirrors `rimaia_core::runs::RunDetail` — what a run detail view opens on,
 * in ADR-0013's order: the run's own outcome (status, exit class, duration,
 * turn count, cost, attempt — all on the flattened {@link Run} fields), then
 * `diff` (files changed, insertions, deletions, the per-file breakdown, and
 * the commits), then the PR link (`prUrl`) and the exact prompt (`prompt`) —
 * already on {@link Run}. The transcript itself is read separately, page by
 * page, through {@link readRunTranscriptPage}.
 */
export interface RunDetail extends Run {
  diff: DiffSummary;
  logAvailable: boolean;
}

/** Mirrors `rimaia_core::runs::transcript::TranscriptBlock` — one block
 *  inside a transcript entry's `content` array. Unlike {@link ToolCall}'s
 *  live-tail rendering, `content` on a `tool_result` block is kept whole
 *  rather than dropped, because this is read on demand from disk rather than
 *  held resident in a bounded ring buffer. */
export type TranscriptBlock =
  | { kind: "text"; text: string }
  | { kind: "tool_use"; id: string; name: string; input: unknown }
  | { kind: "tool_result"; toolUseId: string; isError: boolean; content: string | null }
  | { kind: "other" };

/** Mirrors `rimaia_core::runs::transcript::TranscriptEntryKind` — what one
 *  transcript line was. `"malformed"` is a line that was not valid JSON at
 *  all, kept in place rather than skipped so a page's entry count still
 *  matches what a reader counts by eye in the raw file. */
export type TranscriptEntryKind =
  | { type: "assistant"; blocks: TranscriptBlock[] }
  | { type: "user"; blocks: TranscriptBlock[] }
  | { type: "result"; summary: string | null; errors: string[]; isError: boolean }
  | { type: "other"; eventType: string; subtype: string | null }
  | { type: "malformed"; raw: string };

/**
 * Mirrors `rimaia_core::runs::transcript::TranscriptEntry` — one line of a
 * transcript, as the viewer renders it. `line` is the 1-based line number in
 * the file *including* blank lines, for "open in editor"-style links; it is
 * not the same count {@link TranscriptPage.offset} advances over, which
 * counts only non-blank entries.
 */
export interface TranscriptEntry {
  line: number;
  kind: TranscriptEntryKind;
}

/**
 * Mirrors `rimaia_core::runs::transcript::TranscriptPage` — one bounded page
 * of a transcript, read straight off disk rather than loading the whole
 * file: the hard requirement behind this shape is that a 50MB transcript
 * opens without freezing the UI, which pagination (not virtualization)
 * satisfies by bounding both the backend read and the IPC payload to
 * `entries.length` lines at a time. See {@link readRunTranscriptPage}.
 */
export interface TranscriptPage {
  entries: TranscriptEntry[];
  /** How many non-blank lines precede this page. */
  offset: number;
  /** How many non-blank lines the whole file holds — enough to render
   *  "page 3 of 40" and disable "next" on the last page. */
  totalLines: number;
}

/**
 * Mirrors `rimaia_core::runs::transcript::TranscriptSummary` — the few facts
 * that explain a run's shape before anyone reads its thousand lines: what the
 * CLI said it was running under, how many tool calls it was refused, and
 * whether the stream ever reached a `result`. Read once per run detail, via
 * {@link summarizeRunTranscript}; every field is already in the transcript
 * and none of them is findable by paging it.
 */
export interface TranscriptSummary {
  permissionMode: string | null;
  model: string | null;
  deniedToolCalls: number;
  endedWithResult: boolean;
  /** The last entry would not parse — a stream cut mid-write, not a bad line
   *  in the middle of a run that carried on past it. */
  endsMidLine: boolean;
  malformedLines: number;
}

/**
 * Mirrors `rimaia_core::runs::transcript::SearchHit` — one line matching a
 * {@link searchRunTranscript} query. The search itself runs against the raw
 * JSON text of every line, not the parsed {@link TranscriptEntry} model,
 * which is what lets it find a match inside a tool call's input and not only
 * inside assistant text.
 */
export interface SearchHit {
  /** The same 1-based file line {@link TranscriptEntry.line} uses — what to
   *  show a reader. */
  line: number;
  /** Where the hit sits in the entry numbering {@link TranscriptPage.offset}
   *  advances over: 0-based, blank lines not counted. Pass it straight to
   *  {@link readRunTranscriptPage} as the offset — `line` cannot be
   *  converted into it without re-reading the file. */
  entry: number;
  /** A bounded excerpt centred on the match. */
  snippet: string;
}

/** What {@link pruneRunLogs} sends. Mirrors
 *  `commands::runs::PruneCriterionInput` — the by-age and by-task actions
 *  task 015's Scope names, and nothing else: there is deliberately no
 *  "prune everything". */
export type PruneCriterionInput =
  | { kind: "older_than_days"; days: number }
  | { kind: "task"; taskId: string };

/** Mirrors `rimaia_core::runs::PruneResult` — what one prune actually
 *  removed, so Settings can report it and refresh the total size against a
 *  number that agrees with what just happened. */
export interface PruneResult {
  runsPruned: number;
  bytesFreed: number;
}

// ---------------------------------------------------------------------------
// The local MCP server (task 010) — see `crates/core/src/mcp/mod.rs`.
// ---------------------------------------------------------------------------

/** Mirrors `rimaia_core::mcp::McpState`. */
export type McpServerState = "listening" | "port_in_use" | "stopped";

/** Mirrors `rimaia_core::mcp::McpStatus`. */
export interface McpStatus {
  state: McpServerState;
  /** What the port *should* be. It disagrees with {@link boundAddress} in
   *  exactly the case the panel exists to explain, which is why every URL on
   *  screen is built from the address and never from this. */
  configuredPort: number;
  /** `"127.0.0.1:4517"`, and `null` unless the server is listening. */
  boundAddress: string | null;
  /** The operating system's own words about a failed bind, plus the remedy. */
  message: string | null;
}

/** Mirrors `rimaia_core::mcp::McpProbe`: what one real round trip measured. */
export interface McpProbe {
  endpoint: string;
  latencyMs: number;
  serverName: string;
  protocolVersion: string;
  toolCount: number;
}
