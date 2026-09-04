import type {
  CSSProperties,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
} from "react";
import { useEffect, useState } from "react";

import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";

import { cardBadge, relativeTime } from "../../lib/board";
import { getQueueStatus, listRepositories, startRun, toRimaiaError } from "../../lib/commands";
import {
  subscribeToRepositoriesChanged,
  subscribeToRunsChanged,
  subscribeToSettingsChanged,
  subscribeToTasksChanged,
} from "../../lib/events";
import type {
  EffectiveStrategyFields,
  QueueEntry,
  Repository,
  RimaiaError,
  Task,
  TaskSummary,
} from "../../types";
import {
  STRATEGY_ORIGIN_LABELS,
  parseStrategyPlan,
  strategyBadgeText,
} from "../panel/StrategySection";
import { QUEUE_SKIP_LABELS } from "../runs/QueuePlanList";
import { OpenInMenu } from "./OpenInMenu";
import { RunStateBadge } from "./RunStateBadge";

type KeyDownHandler = (event: ReactKeyboardEvent<HTMLElement>) => void;

const ARROW_KEYS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]);

// ---------------------------------------------------------------------------
// "Run now" (task 008) — a shared, single-flight repository lookup.
//
// `Column` hands this component only a resolved repository *name*
// (`repositoryName`, below), never the `Repository` row itself — so
// `allowUnattendedRuns` (ADR-0012's per-repository opt-in, which "Run now"
// must be disabled against) is not on hand here. The correct fix is threading
// the row `Board`'s own `useRepositories()` already holds down through
// `Column`, but both files sit outside this stage's file ownership (see this
// task's own final report for the flag). This module-level, reference-counted
// cache is the seam this file *can* own: every mounted card shares one
// `list_repositories` call and one `repositories:changed` subscription rather
// than firing its own, and both are torn down the moment the last card
// watching them unmounts — so a board that re-mounts (a real navigation, or a
// test's own render/cleanup cycle) always starts the next one from a real
// fetch, never a stale snapshot left over from before.
let repositoryCache: ReadonlyMap<string, Repository> | null = null;
let repositoryFetch: Promise<void> | null = null;
let repositoryUnlisten: (() => void) | null = null;
const repositorySubscribers = new Set<
  (cache: ReadonlyMap<string, Repository> | null) => void
>();

function notifyRepositorySubscribers() {
  for (const subscriber of repositorySubscribers) subscriber(repositoryCache);
}

function loadRepositories() {
  if (repositoryCache || repositoryFetch) return;
  repositoryFetch = listRepositories()
    .then((repositories) => {
      repositoryCache = new Map(repositories.map((repository) => [repository.id, repository]));
    })
    .catch(() => {
      // No event bridge (a non-Tauri preview, or a test that never mocks
      // `list_repositories`), or the call itself failed: every card falls
      // back to its own "unknown" disabled state below rather than retrying
      // in a loop.
      repositoryCache = new Map();
    })
    .finally(() => {
      repositoryFetch = null;
      notifyRepositorySubscribers();
    });
}

function useRepositoryLookup(): ReadonlyMap<string, Repository> | null {
  const [lookup, setLookup] = useState(repositoryCache);

  useEffect(() => {
    repositorySubscribers.add(setLookup);
    loadRepositories();
    if (!repositoryUnlisten) {
      subscribeToRepositoriesChanged(() => {
        repositoryCache = null;
        loadRepositories();
      }).then(
        (unlisten) => {
          repositoryUnlisten = unlisten;
        },
        () => {
          // No event bridge — the fetch above still resolved (or failed) on
          // its own; there is simply nothing to keep it fresh with.
        },
      );
    }

    return () => {
      repositorySubscribers.delete(setLookup);
      if (repositorySubscribers.size === 0) {
        repositoryCache = null;
        repositoryFetch = null;
        repositoryUnlisten?.();
        repositoryUnlisten = null;
      }
    };
  }, []);

  return lookup;
}

// ---------------------------------------------------------------------------
// Queued position (task 009) — the same shared, single-flight lookup shape as
// the repository cache above, and for the same reason: `getQueueStatus` is
// not something `Column`/`Board` already fetch and could hand down, and both
// sit outside this stage's file ownership (this task's own final report names
// the flag). Keyed by task id, from `QueueStatus.plan` — every mounted card
// shares one `get_queue_status` call rather than one each.
// ---------------------------------------------------------------------------

let queueCache: ReadonlyMap<string, QueueEntry> | null = null;
let queueFetch: Promise<void> | null = null;
// Set by `invalidate` when a change event lands while `queueFetch` is
// already outstanding — `loadQueueStatus`'s own early-return guard below
// would otherwise treat that call as a no-op, and the fetch already in
// flight (started before whatever just changed) would then overwrite the
// cache with a snapshot nothing left in place to correct. See `loadQueueStatus`'s
// `.finally` for the retry this flag actually triggers.
let queueDirty = false;
let queueUnlistenTasks: (() => void) | null = null;
let queueUnlistenRuns: (() => void) | null = null;
let queueUnlistenSettings: (() => void) | null = null;
const queueSubscribers = new Set<(cache: ReadonlyMap<string, QueueEntry> | null) => void>();

function notifyQueueSubscribers() {
  for (const subscriber of queueSubscribers) subscriber(queueCache);
}

function loadQueueStatus() {
  if (queueCache || queueFetch) return;
  queueFetch = getQueueStatus()
    .then((status) => {
      queueCache = new Map(status.plan.map((entry) => [entry.taskId, entry]));
    })
    .catch(() => {
      // No event bridge, or the call itself failed: every card falls back to
      // "nothing to show" below rather than retrying in a loop.
      queueCache = new Map();
    })
    .finally(() => {
      queueFetch = null;
      if (queueDirty) {
        // An invalidation landed while this fetch was already in flight and
        // found nothing to act on but `queueDirty` — the fetch's own `.then`
        // above just wrote the stale result into `queueCache`, so clear it
        // again before retrying or `loadQueueStatus`'s own guard would read
        // that stale (but non-null) cache and skip the fetch entirely.
        // Notifying subscribers with the stale value first, then immediately
        // re-fetching, would only flash the wrong value before the retry
        // lands — so this goes straight to the retry instead.
        queueDirty = false;
        queueCache = null;
        loadQueueStatus();
        return;
      }
      notifyQueueSubscribers();
    });
}

/**
 * Every `ready` task's place in the queue's own plan, invalidated on the same
 * three publishes the Runs view refreshes on: a task moving or ending, or the
 * switch itself (`settings:changed` — `queue_state` lives there).
 */
function useQueueLookup(): ReadonlyMap<string, QueueEntry> | null {
  const [lookup, setLookup] = useState(queueCache);

  useEffect(() => {
    queueSubscribers.add(setLookup);
    loadQueueStatus();

    function invalidate() {
      queueCache = null;
      if (queueFetch) {
        // A fetch already in flight was started with data from before
        // whatever just changed — `loadQueueStatus`'s own early-return guard
        // would otherwise discard this call as a no-op. Mark it dirty
        // instead: that fetch's own `.finally` is what re-fetches once it
        // settles, so this invalidation is never silently dropped.
        queueDirty = true;
        return;
      }
      loadQueueStatus();
    }

    if (!queueUnlistenTasks) {
      subscribeToTasksChanged(invalidate).then(
        (unlisten) => {
          queueUnlistenTasks = unlisten;
        },
        () => {},
      );
    }
    if (!queueUnlistenRuns) {
      subscribeToRunsChanged(invalidate).then(
        (unlisten) => {
          queueUnlistenRuns = unlisten;
        },
        () => {},
      );
    }
    if (!queueUnlistenSettings) {
      subscribeToSettingsChanged(invalidate).then(
        (unlisten) => {
          queueUnlistenSettings = unlisten;
        },
        () => {},
      );
    }

    return () => {
      queueSubscribers.delete(setLookup);
      if (queueSubscribers.size === 0) {
        queueCache = null;
        queueFetch = null;
        queueDirty = false;
        queueUnlistenTasks?.();
        queueUnlistenTasks = null;
        queueUnlistenRuns?.();
        queueUnlistenRuns = null;
        queueUnlistenSettings?.();
        queueUnlistenSettings = null;
      }
    };
  }, []);

  return lookup;
}

/** Whether "Run now" is clickable right now, and — when it is not — why. */
type RunNowState =
  | { readonly kind: "ready" }
  | { readonly kind: "unknown" }
  | { readonly kind: "running" }
  | { readonly kind: "blocked"; readonly reason: string };

/**
 * ADR-0012's opt-in, applied to one card. The wording mirrors
 * `repo::ensure_unattended_runs_allowed`'s own refusal (`crates/core/src/
 * repo/mod.rs`) — the same sentence a rejected `start_task_run` call would
 * carry, so a card that guessed wrong about a repository's state still shows
 * the service's own reason rather than a paraphrase of it (seam-contract D8's
 * "specificity lives in the message", applied to a disabled button instead of
 * a rejection).
 */
function runNowState(task: CardTask, repositories: ReadonlyMap<string, Repository> | null): RunNowState {
  if (task.runState === "running" || task.runState === "queued") {
    return { kind: "running" };
  }
  if (repositories === null) {
    return { kind: "unknown" };
  }
  const repository = repositories.get(task.repositoryId);
  if (!repository || !repository.allowUnattendedRuns) {
    const name = repository?.name ?? "this task's repository";
    return {
      kind: "blocked",
      reason: `"${name}" has not enabled unattended agent runs. Enable it in Settings → Repositories before starting tasks here.`,
    };
  }
  return { kind: "ready" };
}

/**
 * The D12 summary fields a card renders, layered onto `Task` as *optional*
 * fields rather than requiring the whole `TaskSummary` — `Board`'s real
 * render path always supplies them (`list_tasks` returns `TaskSummary`,
 * seam-contract D12), but keeping them optional here means a bare `Task`
 * (what this file's and `Column`'s own tests, and `TaskCardPreview`'s few
 * other callers, construct) is still assignable without every fixture
 * fabricating a link count. Missing renders as "no links / no dependencies",
 * which is also the correct reading for a task genuinely built with none.
 */
type CardTask = Task &
  Partial<
    Pick<
      TaskSummary,
      "linkCount" | "dependencyCount" | "lastRun" | "blockedByIncomplete" | "blockingTitle"
    >
  > &
  Partial<EffectiveStrategyFields>;

interface CardFace {
  readonly task: CardTask;
  readonly repositoryName: string;
  readonly now: Date;
}

/**
 * What `board.css` paints the card's left rail and background wash from —
 * exactly the badge the face is about to render, or `"idle"` for the card that
 * renders none.
 *
 * The same `cardBadge` call the badge itself makes, rather than `task.runState`
 * verbatim, so the rail can never disagree with the word beside it: D9's
 * `interrupted` and ADR-0008's derived `blocked` are both renderings that
 * `runState` alone does not spell.
 */
function cardRunState(task: CardTask): string {
  return (
    cardBadge(task.runState, task.lastRun ?? null, task.blockedByIncomplete ?? false) ?? "idle"
  );
}

/**
 * The visual content shared by the real, draggable card and the
 * `DragOverlay` clone — factored out so the overlay never calls
 * `useSortable` a second time for an id already registered by the card it is
 * floating above.
 *
 * Renders all six things task 005's Scope requires: title, repository, the
 * run-state badge (via `cardBadge`'s real `lastRun.exitClass`, so D9's
 * `interrupted` can actually appear), link count, dependency indicator and
 * relative time of last activity. Link/dependency counts of 0 render nothing
 * rather than "0 links" — task 005's own Notes ask for polish on ordering,
 * not card decoration, and a task with neither has nothing to indicate.
 *
 * Task 020's amendment to D12 adds a seventh: what a run would actually spawn
 * with. It is read off `effectiveModel`/`effectiveEffort`/`effectiveOrigin`,
 * which `list_tasks` resolves in Rust, and **never** recomputed from
 * `task.model` and a settings fetch — the precedence chain is a business rule,
 * and a TypeScript copy of it would be a second implementation free to
 * disagree with the one the runner actually obeys.
 */
function CardFace({ task, repositoryName, now }: CardFace) {
  const linkCount = task.linkCount ?? 0;
  const dependencyCount = task.dependencyCount ?? 0;
  const blocked = task.blockedByIncomplete ?? false;

  // The three effective fields are one projection on the Rust side and arrive
  // together or not at all, so an absent origin means this card was built from
  // a bare `Task` (see `CardTask`) — nothing true to render, which is the same
  // answer as a task with nothing configured anywhere.
  const origin = task.effectiveOrigin;
  const strategy = origin
    ? strategyBadgeText(task.effectiveModel ?? null, task.effectiveEffort ?? null)
    : null;
  // An inherited value is still what runs, but it is not a decision anybody
  // made about *this* card, so it recedes.
  const inherited = origin === "global" || origin === "claude_code";
  const proposalWaiting =
    task.strategyMode === "planned" &&
    task.strategySource === "planner" &&
    parseStrategyPlan(task.strategyPlan)?.status === "proposed";

  return (
    <>
      {/* State leads. The badge sits in an eyebrow row *above* the title
          rather than beside the repository below it, because the first
          question at 08:00 is "did anything happen to this" and the second is
          "which task is it" — and because a badge is the widest, most
          coloured thing on the card, so putting it anywhere else makes the eye
          land on it second and jump back. The repository takes the quiet half
          of the row as a monospace tag: it is an identifier, not prose. */}
      <div className="task-card-head">
        <span className="task-card-repo" title={repositoryName}>
          {repositoryName}
        </span>
        <RunStateBadge
          runState={task.runState}
          lastRun={task.lastRun ?? null}
          blockedByIncomplete={blocked}
        />
      </div>
      <h4 className="task-card-title" title={task.title}>
        {task.title}
      </h4>
      {/* ADR-0008's "the card shows which task is blocking it", and task 011's
          acceptance criterion "each showing A as the reason" — a visible line
          on the card face, not a `title=` attribute. A blocked card without a
          name on it makes the user open every card in the chain to find the
          one that stalled, which is the opposite of the one-glance morning
          review the ADR is written for. Directly under the title, which is the
          thing it qualifies. */}
      {blocked && task.blockingTitle && (
        <p className="task-card-blocked-by">Blocked by {task.blockingTitle}</p>
      )}
      {(strategy || proposalWaiting) && (
        <div className="task-card-strategy">
          {origin && strategy && (
            <span
              className={
                inherited ? "task-card-strategy-badge muted" : "task-card-strategy-badge"
              }
              title={`Model and effort from ${STRATEGY_ORIGIN_LABELS[origin]}`}
            >
              {strategy}
            </span>
          )}
          {proposalWaiting && (
            <span
              className="task-card-strategy-proposal"
              title="The planner has proposed a strategy — open the task to accept, edit or override it."
            >
              Proposal
            </span>
          )}
        </div>
      )}
      <div className="task-card-footer">
        <span className="task-card-time tabular-nums">{relativeTime(task.updatedAt, now)}</span>
        <span className="task-card-indicators">
          {linkCount > 0 && (
            <span
              className="task-card-indicator"
              title={`${linkCount} link${linkCount === 1 ? "" : "s"}`}
            >
              {linkCount} {linkCount === 1 ? "link" : "links"}
            </span>
          )}
          {dependencyCount > 0 && (
            <span
              className="task-card-indicator"
              title={`Depends on ${dependencyCount} task${dependencyCount === 1 ? "" : "s"}`}
            >
              {dependencyCount} dep{dependencyCount === 1 ? "" : "s"}
            </span>
          )}
        </span>
      </div>
    </>
  );
}

/** The floating clone `Board` renders inside `DragOverlay` while a card is
 *  being dragged — static, no drag machinery of its own. It carries the same
 *  `data-run-state` as the card it is floating above, because a dragged card
 *  that lost its rail would read as a different card. */
export function TaskCardPreview(props: CardFace) {
  return (
    <article className="task-card task-card-overlay" data-run-state={cardRunState(props.task)}>
      <CardFace {...props} />
    </article>
  );
}

interface TaskCardProps extends CardFace {
  readonly selected: boolean;
  readonly onSelect: (id: string) => void;
  readonly registerCardRef: (id: string, element: HTMLElement | null) => void;
  /** Only for the arrow keys dnd-kit's own `KeyboardSensor` did not consume
   *  itself — see the combined handler below. */
  readonly onArrowNavigate: (id: string, key: string) => void;
}

/**
 * One card: draggable (pointer and keyboard, via `useSortable`), selectable
 * by click, and focus-navigable by arrow key when no drag is in progress.
 *
 * The whole card is the drag handle — `attributes`/`listeners` spread onto
 * the same element `onClick`/`onKeyDown` are bound to — because task 005's
 * cards have no separate handle affordance. `PointerSensor`'s activation
 * distance (set where `Board` builds the sensor) is what keeps a plain click
 * from being read as a drag.
 */
export function TaskCard({
  task,
  repositoryName,
  now,
  selected,
  onSelect,
  registerCardRef,
  onArrowNavigate,
}: TaskCardProps) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: task.id,
  });

  const style: CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
  };

  function setRefs(element: HTMLElement | null) {
    setNodeRef(element);
    registerCardRef(task.id, element);
  }

  const repositories = useRepositoryLookup();
  const runNow = runNowState(task, repositories);
  const [starting, setStarting] = useState(false);
  const [runError, setRunError] = useState<RimaiaError | null>(null);

  // Queued position (task 009): looked up unconditionally, same as
  // `repositories` above — a hook cannot be called only for some renders —
  // and only ever rendered for a `ready`, `idle` card. Every other run state
  // already has its own badge (`RunStateBadge`) for "why this is not running
  // right now", and `selection::skip_reason` can only ever return
  // `AlreadyInFlight`/`DependencyNotSatisfied`/`NeedsAttention` for a task in
  // one of those states — never for `idle` — so gating on `idle` costs
  // nothing this task's own scheduler code could otherwise show here.
  const queuePlan = useQueueLookup();
  const queueEntry =
    task.column === "ready" && task.runState === "idle" ? (queuePlan?.get(task.id) ?? null) : null;

  /** Never reaches `onSelect`/dnd-kit's own drag activation — both listen on
   *  the `<article>` this button sits inside, and a click or a keypress here
   *  is stopped before it bubbles that far (see the wrapping `<div>` below). */
  function handleRunNow(event: ReactMouseEvent<HTMLButtonElement>) {
    event.stopPropagation();
    if (runNow.kind !== "ready") return;
    setRunError(null);
    setStarting(true);
    startRun(task.id).then(
      () => setStarting(false),
      (thrown) => {
        setRunError(toRimaiaError(thrown));
        setStarting(false);
      },
    );
  }

  function handleKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
    // dnd-kit's own keyboard handling first (Space to lift, arrows and
    // Escape while a keyboard drag is active) — it calls `preventDefault()`
    // on every key it consumes, which is how we tell "dnd-kit is mid-drag"
    // from "nothing is happening" without a second piece of state to track.
    // `Board`'s sensor narrows dnd-kit's own activation keys to Space alone
    // (dnd-kit's default also claims Enter), which is what leaves Enter free
    // to open the panel below — a card announces `role="button"` (dnd-kit's
    // own `attributes`), and Enter is the activation key a screen-reader
    // user expects a button to respond to.
    (listeners?.onKeyDown as KeyDownHandler | undefined)?.(event);
    if (event.defaultPrevented) return;

    if (ARROW_KEYS.has(event.key)) {
      event.preventDefault();
      onArrowNavigate(task.id, event.key);
      return;
    }

    if (event.key === "Enter") {
      event.preventDefault();
      onSelect(task.id);
    }
  }

  return (
    <article
      ref={setRefs}
      style={style}
      className={[
        "task-card",
        isDragging && "task-card-dragging",
        selected && "task-card-selected",
      ]
        .filter(Boolean)
        .join(" ")}
      data-task-id={task.id}
      // What `board.css` colours the left rail and the background wash from —
      // the card's own state, made visible without having to read the badge.
      data-run-state={cardRunState(task)}
      // Not `aria-selected` — that is only defined for `option`/`row`/`tab`/
      // `treeitem`/`gridcell`/`columnheader`/`rowheader` roles, and dnd-kit's
      // own `attributes` (spread below) set `role="button"`, which is not
      // among them, so it would carry no assistive-tech meaning here.
      // `aria-current` is valid on any element and is the pattern
      // `Sidebar.tsx` already uses for "this is the one currently open".
      aria-current={selected ? "true" : undefined}
      {...attributes}
      {...listeners}
      onKeyDown={handleKeyDown}
      onClick={() => onSelect(task.id)}
    >
      <CardFace task={task} repositoryName={repositoryName} now={now} />

      {/* One action row: task 009's queue position on the left, task 008's
          "Run now" on the right. They used to be two stacked blocks, the
          second of which was a full-width accent button on every card in four
          columns — the loudest thing on the board, for the verb that overrides
          a decision the scheduler is already making correctly. Quiet control,
          filled in on hover; the queue position beside it is the readout that
          earns the row. */}
      <div className="task-card-actions">
        {/* ADR-0012's whole security posture depends on a skipped reason being
            visible rather than silent — the skip line below is why this is not
            simply hidden when the queue passes a task over. */}
        {queueEntry && queueEntry.skip === null && (
          <span className="task-card-indicator task-card-queue-position tabular-nums">
            Queued #{queueEntry.queuePosition}
          </span>
        )}
        {/* Task 026. Rendered off the card's own row — `worktree_path` is
            already on every card (seam-contract D12), so no board read
            changes and nothing here asks the disk. A task that has never run
            has no worktree and shows no control at all, which is the normal
            state of most of the board rather than something to report. */}
        {task.worktreePath !== null && (
          <OpenInMenu taskId={task.id} onError={setRunError} />
        )}
        {/* Task 008's "Run now", isolated from the drag/select machinery
            `{...attributes}`/`{...listeners}` and `onClick`/`onKeyDown` above
            bind to the whole article — a click or a keypress here must start a
            run, never lift the card or open the panel underneath it. The two
            stoppers sit on the button itself rather than on a wrapper, so the
            explanations below stay part of the card's drag surface. */}
        <button
          type="button"
          className="task-card-run-button"
          disabled={runNow.kind !== "ready" || starting}
          title={runNow.kind === "blocked" ? runNow.reason : undefined}
          onPointerDown={(event) => event.stopPropagation()}
          onKeyDown={(event) => event.stopPropagation()}
          onClick={handleRunNow}
        >
          {starting ? "Starting…" : runNow.kind === "running" ? "Running…" : "Run now"}
        </button>
      </div>

      {queueEntry && queueEntry.skip !== null && (
        <p className="task-card-queue-skip muted">
          Not queued — {QUEUE_SKIP_LABELS[queueEntry.skip]}
        </p>
      )}
      {runNow.kind === "blocked" && <p className="task-card-run-locked muted">{runNow.reason}</p>}
      {runError && <p className="task-card-run-error">{runError.message}</p>}
    </article>
  );
}
