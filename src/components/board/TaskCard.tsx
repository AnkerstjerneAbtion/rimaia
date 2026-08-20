import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent } from "react";

import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";

import { relativeTime } from "../../lib/board";
import type { Task, TaskSummary } from "../../types";
import { RunStateBadge } from "./RunStateBadge";

type KeyDownHandler = (event: ReactKeyboardEvent<HTMLElement>) => void;

const ARROW_KEYS = new Set(["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]);

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
    </article>
  );
}
