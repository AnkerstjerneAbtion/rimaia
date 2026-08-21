import type {
  CSSProperties,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
} from "react";
import { useEffect, useState } from "react";

import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";

import { relativeTime } from "../../lib/board";
import { listRepositories, startRun, toRimaiaError } from "../../lib/commands";
import { subscribeToRepositoriesChanged } from "../../lib/events";
import type { Repository, RimaiaError, Task, TaskSummary } from "../../types";
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
type CardTask = Task & Partial<Pick<TaskSummary, "linkCount" | "dependencyCount" | "lastRun">>;

interface CardFace {
  readonly task: CardTask;
  readonly repositoryName: string;
  readonly now: Date;
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
 */
function CardFace({ task, repositoryName, now }: CardFace) {
  const linkCount = task.linkCount ?? 0;
  const dependencyCount = task.dependencyCount ?? 0;

  return (
    <>
      <h4 className="task-card-title">{task.title}</h4>
      <div className="task-card-meta">
        <span className="task-card-repo">{repositoryName}</span>
        <RunStateBadge runState={task.runState} lastRun={task.lastRun ?? null} />
      </div>
      <div className="task-card-footer">
        <span className="task-card-time">{relativeTime(task.updatedAt, now)}</span>
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
 *  being dragged — static, no drag machinery of its own. */
export function TaskCardPreview(props: CardFace) {
  return (
    <article className="task-card task-card-overlay">
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

      {/* Task 008's "Run now": a plain nested `<button>`, isolated from the
          drag/select machinery `{...attributes}`/`{...listeners}` and
          `onClick`/`onKeyDown` above bind to the whole article — a click or a
          keypress here must start a run, never lift the card or open the
          panel underneath it. */}
      <div
        className="task-card-run"
        onPointerDown={(event) => event.stopPropagation()}
        onKeyDown={(event) => event.stopPropagation()}
      >
        <button
          type="button"
          className="task-card-run-button"
          disabled={runNow.kind !== "ready" || starting}
          title={runNow.kind === "blocked" ? runNow.reason : undefined}
          onClick={handleRunNow}
        >
          {starting ? "Starting…" : runNow.kind === "running" ? "Running…" : "Run now"}
        </button>
        {runNow.kind === "blocked" && <p className="task-card-run-locked muted">{runNow.reason}</p>}
        {runError && <p className="task-card-run-error">{runError.message}</p>}
      </div>
    </article>
  );
}
