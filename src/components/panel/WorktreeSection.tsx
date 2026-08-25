import { useEffect, useState } from "react";

import { getWorktreeStatus, revealTaskWorktree, toRimaiaError } from "../../lib/commands";
import { subscribeToTasksChanged } from "../../lib/events";
import type { RimaiaError, WorktreeStatus } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

interface WorktreeSectionProps {
  readonly taskId: string;
}

/**
 * Task 007's worktree section — branch, path and live status, recomputed
 * fresh from git on every read via `get_worktree_status` (ADR-0005). Owns
 * its own fetch and its own `tasks:changed` subscription, the same shape
 * every other section in this panel uses (see `TaskDetailPanel`'s own doc
 * comment): the fields shown here — ahead/behind, dirty, the diff stat —
 * are not on `Task`/`TaskDetail` at all, so there is nothing the parent
 * could hand down instead.
 *
 * Renders the no-worktree case deliberately rather than as an error: most
 * cards on the board have never run, and "no worktree yet" is what that
 * looks like, not a failure (task 007's own instruction).
 */
export function WorktreeSection({ taskId }: WorktreeSectionProps) {
  const [status, setStatus] = useState<WorktreeStatus | null>(null);
  const [statusError, setStatusError] = useState<RimaiaError | null>(null);
  const [revealing, setRevealing] = useState(false);
  const [actionError, setActionError] = useState<RimaiaError | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    function load() {
      getWorktreeStatus(taskId).then(
        (result) => {
          if (active) {
            setStatus(result);
            setStatusError(null);
          }
        },
        (thrown) => {
          if (active) setStatusError(toRimaiaError(thrown));
        },
      );
    }

    load();

    // Narrowed the same way `TaskDetailPanel`'s own subscription is: an
    // empty payload is ADR-0018's lag-recovery "re-read everything" case and
    // always refetches; otherwise only this task's id does. A run starting
    // (which creates the worktree this section has nothing to show yet) is
    // exactly the kind of change that arrives this way.
    subscribeToTasksChanged((taskIds) => {
      if (active && (taskIds.length === 0 || taskIds.includes(taskId))) load();
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview) — the section
        // still shows whatever the initial `load()` above resolved.
      },
    );

    return () => {
      active = false;
      unlisten?.();
    };
  }, [taskId]);

  async function handleReveal() {
    setRevealing(true);
    setActionError(null);
    try {
      await revealTaskWorktree(taskId);
    } catch (thrown) {
      setActionError(toRimaiaError(thrown));
    } finally {
      setRevealing(false);
    }
  }

  async function handleCopyPath(path: string) {
    setActionError(null);
    setCopied(false);
    try {
      await navigator.clipboard.writeText(path);
      setCopied(true);
    } catch {
      // No Tauri command backs this (seam-contract note in `commands.ts`) —
      // the only failure mode is the browser Clipboard API itself refusing,
      // which a message here is the only way to surface.
      setActionError({ code: "internal", message: "could not copy the path to the clipboard" });
    }
  }

  return (
    <section className="task-detail-section worktree-section">
      <h4>Worktree</h4>
      {statusError && (
        <ErrorBanner error={statusError} onDismiss={() => setStatusError(null)} />
      )}
      {status === null && !statusError && <p className="muted">Loading…</p>}
      {status && status.path === null && (
        <p className="muted">
          No worktree yet — this task has never run. Starting a run creates one (task 008).
        </p>
      )}
      {status && status.path !== null && (
        <WorktreeDetails
          status={status}
          revealing={revealing}
          copied={copied}
          onReveal={handleReveal}
          onCopyPath={() => handleCopyPath(status.path as string)}
        />
      )}
      {actionError && (
        <ErrorBanner error={actionError} onDismiss={() => setActionError(null)} />
      )}
    </section>
  );
}

interface WorktreeDetailsProps {
  readonly status: WorktreeStatus;
  readonly revealing: boolean;
  readonly copied: boolean;
  readonly onReveal: () => void;
  readonly onCopyPath: () => void;
}

function WorktreeDetails({ status, revealing, copied, onReveal, onCopyPath }: WorktreeDetailsProps) {
  return (
    <>
      <dl className="detail-list">
        <dt>Branch</dt>
        <dd>{status.branch ? <code>{status.branch}</code> : <span className="muted">—</span>}</dd>
        <dt>Path</dt>
        <dd>
          <code>{status.path}</code>
        </dd>
        <dt>Status</dt>
        <dd>
          {status.exists ? (
            <>
              {status.dirty ? "Uncommitted changes" : "Clean"} · {status.ahead} ahead /{" "}
              {status.behind} behind {status.baseRef} · {status.commitCount}{" "}
              {status.commitCount === 1 ? "commit" : "commits"} · {status.diff.filesChanged}{" "}
              {status.diff.filesChanged === 1 ? "file" : "files"} changed (+
              {status.diff.insertions} / -{status.diff.deletions})
            </>
          ) : (
            <span className="muted">
              Missing on disk — deleted outside Rimaia. Reconciled automatically the next time
              the app starts (ADR-0005).
            </span>
          )}
        </dd>
      </dl>

      <div className="worktree-actions">
        <button type="button" onClick={onReveal} disabled={!status.exists || revealing}>
          {revealing ? "Opening…" : "Open in Finder/Explorer"}
        </button>
        <button type="button" onClick={onCopyPath}>
          {copied ? "Copied" : "Copy path"}
        </button>
      </div>
      {!status.exists && (
        <p className="worktree-locked muted">
          Reveal is unavailable until the worktree is recreated — the directory this path names
          is gone.
        </p>
      )}
    </>
  );
}
