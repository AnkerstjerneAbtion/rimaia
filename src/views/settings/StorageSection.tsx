import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  cleanupDoneWorktrees,
  cleanupMergedWorktrees,
  getAppInfo,
  getRunLogSize,
  getWorktreeAutoCleanup,
  getWorktreeInventory,
  pruneRunLogs,
  removeTaskWorktree,
  revealAppDataDir,
  setWorktreeAutoCleanup,
  toRimaiaError,
} from "../../lib/commands";
import { formatBytes } from "../../lib/format";
import type {
  AppInfo,
  AutoCleanup,
  CleanupReport,
  PruneResult,
  RimaiaError,
  WorktreeInventory,
  WorktreeInventoryEntry,
} from "../../types";

/** Presets task 015's "by age" prune offers — a plain number input inviting
 *  an arbitrary value is more to get wrong (0, a negative number, a typo
 *  with an extra digit) than a short, reviewable list of sensible ages. */
const PRUNE_AGE_PRESETS = [
  { label: "Older than 7 days", days: 7 },
  { label: "Older than 30 days", days: 30 },
  { label: "Older than 90 days", days: 90 },
];

/**
 * Which destructive control is currently showing its confirmation.
 *
 * One piece of state rather than four booleans, so that opening a second
 * confirmation closes the first — two "are you sure?" prompts on screen at
 * once is how somebody answers the wrong one.
 */
type PendingConfirmation =
  | { kind: "prune"; days: number }
  | { kind: "worktree"; taskId: string }
  | { kind: "done" }
  | { kind: "merged" }
  | { kind: "auto-cleanup" }
  | null;

/**
 * Settings → Storage: what Rimaia has written to disk, and the guarded ways to
 * take it off again (task 015's run logs, task 016's worktrees).
 *
 * **Every deletion here is confirmed by a two-state control, never by
 * `window.confirm`** — the same idiom `RunHistorySection` uses, for the reason
 * it gives: a native modal would be the app's only one, and the dangerous step
 * should be a button the user has to find rather than a dialog they dismiss.
 *
 * The panel deliberately does *not* re-derive whether a worktree is removable.
 * `entry.live`, `entry.uncommittedChanges` and `entry.unpushedCommits` come
 * from the service that enforces the guards, so a disabled button and a backend
 * refusal cannot come to disagree — and the refusal is still the authority. The
 * UI's job is to say what will happen before it happens, not to be a second
 * copy of the rule.
 */
export function StorageSection() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [logSize, setLogSize] = useState<number | null>(null);
  const [pruning, setPruning] = useState(false);
  const [pruneResult, setPruneResult] = useState<PruneResult | null>(null);

  const [inventory, setInventory] = useState<WorktreeInventory | null>(null);
  const [worktreeError, setWorktreeError] = useState<RimaiaError | null>(null);
  const [cleaning, setCleaning] = useState(false);
  const [cleanupResult, setCleanupResult] = useState<string | null>(null);
  const [autoCleanup, setAutoCleanup] = useState<AutoCleanup | null>(null);

  const [confirming, setConfirming] = useState<PendingConfirmation>(null);

  useEffect(() => {
    getAppInfo().then(setInfo, (thrown) => setError(toRimaiaError(thrown)));
  }, []);

  function loadLogSize() {
    getRunLogSize().then(setLogSize, (thrown) => setError(toRimaiaError(thrown)));
  }

  useEffect(loadLogSize, []);

  const loadInventory = useCallback(async () => {
    try {
      setInventory(await getWorktreeInventory());
      setWorktreeError(null);
    } catch (thrown) {
      setWorktreeError(toRimaiaError(thrown));
    }
  }, []);

  useEffect(() => {
    void loadInventory();
    getWorktreeAutoCleanup().then(setAutoCleanup, (thrown) =>
      setWorktreeError(toRimaiaError(thrown)),
    );
  }, [loadInventory]);

  async function openInFinder() {
    setError(null);
    try {
      await revealAppDataDir();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }

  async function handlePrune(days: number) {
    setPruning(true);
    setConfirming(null);
    setPruneResult(null);
    try {
      const result = await pruneRunLogs({ kind: "older_than_days", days });
      setPruneResult(result);
      setError(null);
      // The whole point of reporting a size here at all: a prune that ran
      // must be reflected in the number right below it, not just in the
      // one-off "removed N, freed M" line.
      loadLogSize();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setPruning(false);
    }
  }

  /**
   * Runs one worktree action and reports it in words.
   *
   * Every path refreshes the inventory rather than mutating the list in place:
   * a removal changes sizes, merged states and the total, and a client-side
   * patch would be a second, worse computation of numbers the backend just
   * recomputed anyway.
   */
  async function runCleanup(action: () => Promise<string>) {
    setCleaning(true);
    setConfirming(null);
    setCleanupResult(null);
    try {
      setCleanupResult(await action());
      setWorktreeError(null);
    } catch (thrown) {
      setWorktreeError(toRimaiaError(thrown));
    } finally {
      setCleaning(false);
      await loadInventory();
    }
  }

  function handleRemoveWorktree(entry: WorktreeInventoryEntry, event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    // The branch is a separate choice from the removal, and an unmerged branch
    // needs the confirmation that names it — hence three radio values rather
    // than a "delete branch too" checkbox that would mean different things for
    // a merged and an unmerged branch.
    const branch = String(form.get("branch") ?? "keep") as
      | "keep"
      | "delete_if_merged"
      | "delete_even_if_unmerged";

    void runCleanup(async () => {
      const removed = await removeTaskWorktree(entry.taskId, {
        // Forced only because the confirmation the user just gave was shown
        // alongside the counts below — see the confirmation's own copy.
        uncommittedChanges: entry.uncommittedChanges > 0 ? "confirmed_by_user" : "no",
        unpushedCommits: entry.unpushedCommits > 0 ? "confirmed_by_user" : "no",
        branch,
      });
      const branchNote = removed.branchDeleted
        ? ` and deleted ${removed.branchDeleted}`
        : " and kept its branch";
      return `Removed the worktree for "${entry.taskTitle}"${branchNote}, freeing ${formatBytes(
        removed.bytesFreed,
      )}.`;
    });
  }

  async function handleAutoCleanupChange(next: AutoCleanup) {
    // Turning it *off* needs no acknowledgement — nothing is destroyed by
    // deciding to keep more. Only the "on" direction goes through the confirm
    // gate, which is why this is not a plain toggle handler.
    if (next === "on_done_acknowledged") {
      setConfirming({ kind: "auto-cleanup" });
      return;
    }
    await commitAutoCleanup("off");
  }

  async function commitAutoCleanup(next: AutoCleanup) {
    setConfirming(null);
    try {
      await setWorktreeAutoCleanup(next);
      setAutoCleanup(next);
      setWorktreeError(null);
    } catch (thrown) {
      setWorktreeError(toRimaiaError(thrown));
    }
  }

  return (
    <section className="panel">
      <h3>Storage</h3>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {info ? (
        <>
          <dl className="detail-list">
            <dt>Application data</dt>
            <dd>
              <code>{info.dataDir}</code>
            </dd>
            <dt>Database</dt>
            <dd>
              <code>{info.dbFile}</code>
            </dd>
            <dt>Logs</dt>
            <dd>
              <code>{info.logsDir}</code>
            </dd>
            <dt>Version</dt>
            <dd>{info.appVersion}</dd>
          </dl>
          <button type="button" onClick={openInFinder}>
            Open in Finder
          </button>
        </>
      ) : (
        !error && <p className="muted">Reading…</p>
      )}

      {/* Task 015's run logs and task 016's worktrees, side by side, because
          "why is this directory so large" is one question and answering it
          with two separate numbers on two separate screens is not an answer. */}
      <dl className="detail-list">
        <dt>Run logs</dt>
        <dd>{logSize === null ? <span className="muted">Reading…</span> : formatBytes(logSize)}</dd>
        <dt>Worktrees</dt>
        <dd>
          {inventory === null ? (
            <span className="muted">Reading…</span>
          ) : (
            `${formatBytes(inventory.totalBytes)} across ${inventory.entries.length} worktree${
              inventory.entries.length === 1 ? "" : "s"
            }`
          )}
        </dd>
      </dl>

      {/* Confirmed before it runs, like every other action in this app that
          deletes files (`worktree::ForceRemoval::ConfirmedByUser`) — and this
          one spans every task, not one. */}
      <div className="storage-prune-actions">
        {confirming?.kind === "prune" ? (
          <>
            <span>
              Delete the transcript of every finished run started more than {confirming.days} days
              ago, across every task? The run history itself stays.
            </span>
            <button type="button" onClick={() => handlePrune(confirming.days)} disabled={pruning}>
              {pruning ? "Pruning…" : "Delete transcripts"}
            </button>
            <button type="button" onClick={() => setConfirming(null)}>
              Cancel
            </button>
          </>
        ) : (
          PRUNE_AGE_PRESETS.map((preset) => (
            <button
              key={preset.days}
              type="button"
              onClick={() => setConfirming({ kind: "prune", days: preset.days })}
              disabled={pruning}
            >
              {preset.label}
            </button>
          ))
        )}
      </div>
      {pruneResult && (
        <p role="status" className="muted">
          Removed {pruneResult.runsPruned} log{pruneResult.runsPruned === 1 ? "" : "s"}
          {pruneResult.strategyTranscriptsPruned > 0 &&
            ` and ${pruneResult.strategyTranscriptsPruned} planner transcript${
              pruneResult.strategyTranscriptsPruned === 1 ? "" : "s"
            }`}
          , freed {formatBytes(pruneResult.bytesFreed)}.
        </p>
      )}

      {/* ------------------------------------------------------------------
          Worktrees (task 016)
          ------------------------------------------------------------------ */}
      <h4>Worktrees</h4>
      {worktreeError && (
        <ErrorBanner error={worktreeError} onDismiss={() => setWorktreeError(null)} />
      )}

      {inventory !== null && inventory.entries.length === 0 && (
        <p className="muted">No worktrees on disk.</p>
      )}

      {inventory !== null && inventory.entries.length > 0 && (
        <ul className="worktree-list">
          {inventory.entries.map((entry) => (
            <li key={entry.taskId} className="worktree-entry">
              <div className="worktree-entry-header">
                <strong>{entry.taskTitle}</strong>
                <span className="muted">{entry.repositoryName}</span>
              </div>
              <dl className="detail-list">
                <dt>Branch</dt>
                <dd>
                  <code>{entry.branch ?? "—"}</code>{" "}
                  {entry.merged ? (
                    <span className="muted">merged into {entry.baseRef}</span>
                  ) : (
                    <span className="muted">not merged into {entry.baseRef}</span>
                  )}
                </dd>
                <dt>Size</dt>
                <dd>{entry.exists ? formatBytes(entry.sizeBytes) : "gone from disk"}</dd>
                <dt>Last activity</dt>
                <dd>
                  {entry.lastActivity
                    ? new Date(entry.lastActivity).toLocaleString()
                    : /* `null` is "nothing to read an mtime off", which is a
                         different fact from a date at the epoch. */
                      "unknown"}
                </dd>
              </dl>

              {/* The two counts that decide whether removing is safe, shown
                  only when they are non-zero — a row of zeroes on every
                  worktree would make the one that matters invisible. */}
              {(entry.uncommittedChanges > 0 || entry.unpushedCommits > 0) && (
                <p className="worktree-warning">
                  {entry.uncommittedChanges > 0 &&
                    `${entry.uncommittedChanges} uncommitted change${
                      entry.uncommittedChanges === 1 ? "" : "s"
                    }`}
                  {entry.uncommittedChanges > 0 && entry.unpushedCommits > 0 && ", "}
                  {entry.unpushedCommits > 0 &&
                    `${entry.unpushedCommits} commit${
                      entry.unpushedCommits === 1 ? "" : "s"
                    } no remote has`}
                  .
                </p>
              )}

              {entry.live ? (
                <p className="muted">
                  A run is working in this directory — it stays until the run finishes. There is no
                  way to force this one.
                </p>
              ) : confirming?.kind === "worktree" && confirming.taskId === entry.taskId ? (
                /* A real form, not a bare button: the branch choice is part of
                   the same submission as the confirmation, so there is one act
                   rather than a setting the user might change after
                   confirming. */
                <form
                  className="worktree-confirm"
                  onSubmit={(event) => handleRemoveWorktree(entry, event)}
                >
                  <p>
                    Remove this worktree
                    {entry.uncommittedChanges > 0 &&
                      `, discarding ${entry.uncommittedChanges} uncommitted change${
                        entry.uncommittedChanges === 1 ? "" : "s"
                      } committed nowhere else`}
                    ?
                  </p>
                  <fieldset>
                    <legend>Its branch</legend>
                    <label>
                      <input type="radio" name="branch" value="keep" defaultChecked /> Keep{" "}
                      {entry.branch ?? "the branch"}
                    </label>
                    <label>
                      <input type="radio" name="branch" value="delete_if_merged" /> Delete it if it
                      is merged into {entry.baseRef}
                    </label>
                    <label>
                      <input type="radio" name="branch" value="delete_even_if_unmerged" /> Delete it
                      even though it is not merged — this discards its commits
                    </label>
                  </fieldset>
                  <button type="submit" disabled={cleaning}>
                    {cleaning ? "Removing…" : "Remove worktree"}
                  </button>
                  <button type="button" onClick={() => setConfirming(null)}>
                    Cancel
                  </button>
                </form>
              ) : (
                <button
                  type="button"
                  onClick={() => setConfirming({ kind: "worktree", taskId: entry.taskId })}
                  disabled={cleaning}
                >
                  Remove worktree
                </button>
              )}
            </li>
          ))}
        </ul>
      )}

      <div className="worktree-bulk-actions">
        {confirming?.kind === "done" ? (
          <>
            <span>
              Remove the worktree of every task in Done? Branches are kept, and anything with
              uncommitted or unpushed work is left alone and reported.
            </span>
            <button
              type="button"
              disabled={cleaning}
              onClick={() =>
                void runCleanup(async () => describeReport(await cleanupDoneWorktrees()))
              }
            >
              {cleaning ? "Removing…" : "Remove Done worktrees"}
            </button>
            <button type="button" onClick={() => setConfirming(null)}>
              Cancel
            </button>
          </>
        ) : confirming?.kind === "merged" ? (
          <>
            <span>
              Remove every worktree whose branch is already merged into its default branch?
              Branches are kept, and a squash-merged branch counts as unmerged, so it is left
              alone.
            </span>
            <button
              type="button"
              disabled={cleaning}
              onClick={() =>
                void runCleanup(async () => describeReport(await cleanupMergedWorktrees()))
              }
            >
              {cleaning ? "Removing…" : "Remove merged worktrees"}
            </button>
            <button type="button" onClick={() => setConfirming(null)}>
              Cancel
            </button>
          </>
        ) : (
          <>
            <button
              type="button"
              disabled={cleaning}
              onClick={() => setConfirming({ kind: "done" })}
            >
              Remove worktrees for Done tasks
            </button>
            <button
              type="button"
              disabled={cleaning}
              onClick={() => setConfirming({ kind: "merged" })}
            >
              Remove merged worktrees
            </button>
          </>
        )}
      </div>
      {cleanupResult && (
        <p role="status" className="muted">
          {cleanupResult}
        </p>
      )}

      {/* The policy. Off by default, and turning it on is the one setting in
          this app that needs the user to have read a sentence first. */}
      {autoCleanup !== null &&
        (confirming?.kind === "auto-cleanup" ? (
          <div className="worktree-auto-cleanup">
            <p>
              With this on, every task you move to Done loses its checkout — including any
              uncommitted file a run left in it. It never forces past a running task and never
              deletes a branch, so committed work survives.
            </p>
            <button type="button" onClick={() => void commitAutoCleanup("on_done_acknowledged")}>
              I understand — turn it on
            </button>
            <button type="button" onClick={() => setConfirming(null)}>
              Cancel
            </button>
          </div>
        ) : (
          <label className="worktree-auto-cleanup">
            <input
              type="checkbox"
              checked={autoCleanup === "on_done_acknowledged"}
              onChange={(event) =>
                void handleAutoCleanupChange(event.target.checked ? "on_done_acknowledged" : "off")
              }
            />
            Remove a task's worktree automatically when it reaches Done
          </label>
        ))}
    </section>
  );
}

/** One sentence for a bulk action, naming both halves — what went and what was
 *  refused. A report that only counted successes would hide the refusals,
 *  which are the part the user has to act on. */
function describeReport(report: CleanupReport): string {
  const removed = `Removed ${report.removed.length} worktree${
    report.removed.length === 1 ? "" : "s"
  }, freeing ${formatBytes(report.bytesFreed)}.`;
  if (report.refused.length === 0) {
    return removed;
  }
  const refusals = report.refused
    .map((refusal) => `${refusal.taskTitle}: ${refusal.reason}`)
    .join(" ");
  return `${removed} Left ${report.refused.length} alone — ${refusals}`;
}
