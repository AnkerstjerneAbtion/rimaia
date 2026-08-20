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
}

export type View = "board" | "runs" | "settings";

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

/** Mirrors `rimaia_core::db::StrategySource` (ADR-0016). */
export type StrategySource = "user" | "planner";

/** Mirrors `rimaia_core::db::RunStatus` (ADR-0013). The coarse lifecycle of
 *  one attempt, as the Runs view queries it. */
export type RunStatus = "running" | "succeeded" | "failed" | "cancelled" | "interrupted";

/** Mirrors `rimaia_core::db::ExitClass` (ADR-0011). Why a run stopped. */
export type ExitClass = "success" | "usage_limit" | "transient" | "interrupted" | "fatal" | "cancelled";

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
  model: string | null;
  effort: string | null;
  /** Opaque JSON text (ADR-0016); task 020 is the first thing that parses it. */
  strategyPlan: string | null;
  strategySource: StrategySource | null;
  strategyUpdatedAt: string | null;
  createdAt: string;
  updatedAt: string;
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
}

/**
 * Mirrors `rimaia_core::tasks::TaskDetail`. Rust `#[serde(flatten)]`s the
 * task onto this shape, so the wire object is `Task`'s own fields plus these
 * three siblings — not a nested `{ task: {...} }` — which is why this
 * `extends Task` rather than nesting one.
 */
export interface TaskDetail extends Task {
  links: TaskLink[];
  /** Outgoing edges only — what this task depends on, not what depends on it. */
  dependsOn: string[];
  lastRun: Run | null;
}

/**
 * Mirrors `rimaia_core::tasks::LastRunSummary` — the three fields of a `Run`
 * a card draws, not the row. `interrupted` reaches the board through
 * `exitClass` and nowhere else (seam-contract D9).
 */
export interface LastRunSummary {
  status: RunStatus;
  exitClass: ExitClass | null;
  /** `null` while the attempt is still in flight. */
  endedAt: string | null;
}

/**
 * Mirrors `rimaia_core::tasks::TaskSummary`, what [`listTasks`](./commands)
 * returns (seam-contract D12). Flattened on the Rust side for the reason
 * `TaskDetail` is, so this `extends Task` rather than nesting one.
 *
 * The card renders from this; the panel renders from `TaskDetail`. That split
 * is what keeps a fifty-card board one query instead of fifty.
 */
export interface TaskSummary extends Task {
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
