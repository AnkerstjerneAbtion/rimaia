import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { useCallback, useEffect, useRef, useState } from "react";

import { listOpenInTargets, openTaskWorktreeIn, toRimaiaError } from "../../lib/commands";
import type { DetectedOpenInTarget, RimaiaError } from "../../types";

/**
 * The card's **Open in** control (task 026): the tools this machine can
 * actually open a worktree in.
 *
 * Rendered only for a card whose task has a worktree — the caller decides that
 * off `task.worktreePath`, and never by asking the disk. "No worktree yet" is
 * the normal state of most of the board (task 007), not a failure to report, so
 * a card without one shows no control at all rather than a disabled one.
 *
 * # Detection is shared, and never per render
 *
 * The same module-level, reference-counted cache `TaskCard` uses for its
 * repository lookup, and for the same reason one level sharper: detection is
 * `PATH` scans and `stat` calls, and forty cards probing on every drag is a
 * different feature from the one asked for. One fetch is shared by every
 * mounted menu, torn down when the last one unmounts — so the next board mount
 * (a real navigation, or a test's render/cleanup cycle) starts from a real
 * probe rather than a stale snapshot.
 *
 * A stale probe is still possible — an editor uninstalled while the window was
 * open — and it is handled where it has to be: the open command re-detects in
 * Rust and fails with the service's own message on the card. "Re-check" is the
 * cheap way to fix the menu without waiting for that.
 */

let targetCache: DetectedOpenInTarget[] | null = null;
let targetFetch: Promise<void> | null = null;
const targetSubscribers = new Set<(targets: DetectedOpenInTarget[] | null) => void>();

function notifyTargetSubscribers() {
  for (const subscriber of targetSubscribers) subscriber(targetCache);
}

function loadTargets() {
  if (targetCache || targetFetch) return;
  targetFetch = listOpenInTargets()
    .then((targets) => {
      targetCache = targets;
    })
    .catch(() => {
      // No backend (a non-Tauri preview, or a test that never mocks the
      // command): an empty list means no control, which is the honest answer
      // — never a menu of entries that open nothing.
      targetCache = [];
    })
    .finally(() => {
      targetFetch = null;
      notifyTargetSubscribers();
    });
}

function useOpenInTargets(): {
  targets: DetectedOpenInTarget[] | null;
  recheck: () => void;
} {
  const [targets, setTargets] = useState(targetCache);

  useEffect(() => {
    targetSubscribers.add(setTargets);
    loadTargets();
    return () => {
      targetSubscribers.delete(setTargets);
      if (targetSubscribers.size === 0) {
        targetCache = null;
        targetFetch = null;
      }
    };
  }, []);

  const recheck = useCallback(() => {
    targetCache = null;
    targetFetch = null;
    notifyTargetSubscribers();
    loadTargets();
  }, []);

  return { targets, recheck };
}

export function OpenInMenu({
  taskId,
  onError,
}: {
  taskId: string;
  /** Rendered where `TaskCard`'s existing `runError` line is rendered. */
  onError: (error: RimaiaError | null) => void;
}) {
  const { targets, recheck } = useOpenInTargets();
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement | null>(null);

  // A click anywhere else closes it. Registered only while open, so a board of
  // forty cards is not forty document listeners.
  useEffect(() => {
    if (!open) return;
    function onDocumentPointerDown(event: PointerEvent) {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("pointerdown", onDocumentPointerDown);
    return () => document.removeEventListener("pointerdown", onDocumentPointerDown);
  }, [open]);

  if (targets !== null && targets.length === 0) return null;

  function choose(target: DetectedOpenInTarget) {
    setOpen(false);
    onError(null);
    openTaskWorktreeIn(taskId, target.target).catch((thrown) =>
      onError(toRimaiaError(thrown)),
    );
  }

  /**
   * The card claims Enter and the four arrows for its own navigation, and
   * dnd-kit claims Space to lift it — so every key that lands inside this menu
   * is stopped here rather than allowed to reach the `<article>`. Arrow keys
   * then mean "the next item", which is what they mean in a menu.
   */
  function handleMenuKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    event.stopPropagation();
    if (event.key === "Escape") {
      setOpen(false);
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;

    event.preventDefault();
    const items = Array.from(
      root.current?.querySelectorAll<HTMLButtonElement>("[role='menuitem']") ?? [],
    );
    if (items.length === 0) return;
    const current = items.indexOf(document.activeElement as HTMLButtonElement);
    const step = event.key === "ArrowDown" ? 1 : -1;
    const next = current === -1 ? 0 : (current + step + items.length) % items.length;
    items[next]?.focus();
  }

  return (
    <div
      ref={root}
      className="task-card-open-in"
      // The card is the drag surface and the select surface; this control is
      // neither. Both stoppers sit here rather than on each button so the
      // menu's own contents are covered by one rule.
      onPointerDown={(event) => event.stopPropagation()}
      onKeyDown={handleMenuKeyDown}
      onClick={(event) => event.stopPropagation()}
    >
      <button
        type="button"
        className="task-card-open-in-button"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((previous) => !previous)}
      >
        Open in
      </button>
      {open && (
        <ul className="task-card-open-in-menu" role="menu" aria-label="Open the worktree in">
          {(targets ?? []).map((target) => (
            <li key={target.target} role="none">
              <button type="button" role="menuitem" onClick={() => choose(target)}>
                {target.label}
              </button>
            </li>
          ))}
          <li role="none" className="task-card-open-in-recheck">
            {/* "Refreshed on user request", the other half of never probing per
                render. An editor installed while the window was open is
                otherwise missing from the menu until the next launch. */}
            <button
              type="button"
              role="menuitem"
              onClick={() => {
                recheck();
                setOpen(false);
              }}
            >
              Re-check installed apps
            </button>
          </li>
        </ul>
      )}
    </div>
  );
}
