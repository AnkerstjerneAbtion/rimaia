import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import { useRepositories } from "../../hooks/useTasks";
import { useReviewTarget } from "../../hooks/useReviewTarget";
import { approveTask, rejectTask, requestTaskChanges, toRimaiaError } from "../../lib/commands";
import { isEditableTarget } from "../../lib/keyboard";
import { openExternalUrl } from "../../lib/open";
import {
  affectedDependents,
  describeDeparture,
  reviewCommandForKey,
  reviewQueue,
  successor,
} from "../../lib/review";
import type { ReviewCommand } from "../../lib/review";
import type { RimaiaError, TaskSummary } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { OpenInMenu } from "../board/OpenInMenu";
import { KeyLegend } from "./KeyLegend";
import { NoteStep } from "./NoteStep";
import type { NoteKind } from "./NoteStep";
import { TaskReview } from "./TaskReview";

interface ReviewQueueProps {
  /** The board's own read (`useTasks`), archived tasks already excluded. */
  readonly tasks: readonly TaskSummary[];
  readonly loading: boolean;
  readonly readError: RimaiaError | null;
  readonly refresh: () => Promise<void>;
  readonly now: Date;
  readonly onShowDigest: () => void;
}

interface Note {
  readonly kind: NoteKind;
  readonly text: string;
}

/**
 * The review queue (task 017): `in_review` tasks one at a time, in board order,
 * walked and decided with the keyboard alone.
 *
 * The decisions are task 034's services — this component sends them and shows
 * what they answered. In particular it adds no validation to a note: a blank
 * one is the service's refusal to make, and it is shown as the service said it
 * (seam-contract D8).
 */
export function ReviewQueue({
  tasks,
  loading,
  readError,
  refresh,
  now,
  onShowDigest,
}: ReviewQueueProps) {
  const { repositories } = useRepositories();
  const queue = useMemo(() => reviewQueue(tasks), [tasks]);

  const [currentId, setCurrentId] = useState<string | null>(null);
  const [note, setNote] = useState<Note | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [notice, setNotice] = useState<ReactNode>(null);
  const [menuOpen, setMenuOpen] = useState(false);

  const surface = useRef<HTMLDivElement | null>(null);
  const currentIdRef = useRef<string | null>(null);
  const previousQueue = useRef<readonly TaskSummary[]>([]);
  const tasksRef = useRef(tasks);
  tasksRef.current = tasks;
  const queueRef = useRef(queue);
  queueRef.current = queue;
  // Tasks this view is deciding right now, or just decided. The backend's
  // `tasks:changed` for them arrives like any other door's; without this the
  // view would announce that its own approve "left the queue".
  const ownActions = useRef(new Set<string>());

  // Before the first selection lands, the first task is the one on screen, so
  // the queue never paints a frame with nothing selected.
  const current =
    queue.find((task) => task.id === currentId) ?? (currentId === null ? (queue[0] ?? null) : null);
  const { target, error: targetError } = useReviewTarget(current?.id ?? null);

  const select = useCallback((id: string | null) => {
    currentIdRef.current = id;
    setCurrentId(id);
    setNote(null);
    setMenuOpen(false);
    setError(null);
  }, []);

  const go = useCallback(
    (id: string | null) => {
      select(id);
      setNotice(null);
    },
    [select],
  );

  // The queue is live. A task that left `in_review` drops out, one that arrived
  // joins in board order (both fall out of `reviewQueue`). What needs care is
  // the task on screen leaving: the view moves to the next, and says so unless
  // this view is the one that sent it away.
  useEffect(() => {
    const previous = previousQueue.current;
    previousQueue.current = queue;
    if (loading) return;
    const ids = queue.map((task) => task.id);
    const onScreen = currentIdRef.current;
    if (onScreen !== null && ids.includes(onScreen)) return;
    if (onScreen === null) {
      if (ids.length > 0) select(ids[0]);
      return;
    }
    select(
      successor(
        previous.map((task) => task.id),
        onScreen,
        ids,
      ),
    );
    if (!ownActions.current.has(onScreen)) {
      const left = previous.find((task) => task.id === onScreen);
      setNotice(
        describeDeparture(
          left?.title ?? "A task",
          tasksRef.current.find((task) => task.id === onScreen),
        ),
      );
    }
  }, [queue, loading, select]);

  // Focus stays on the review surface after every action and navigation, so the
  // next key works without a click. Not while a field or the menu has it.
  useEffect(() => {
    if (note === null && !menuOpen) surface.current?.focus();
  }, [currentId, note === null, menuOpen, pending]);

  const decide = useCallback(
    async (kind: "approve" | NoteKind, text: string) => {
      const task = queueRef.current.find((candidate) => candidate.id === currentIdRef.current);
      if (!task) return;
      setPending(true);
      setError(null);
      ownActions.current.add(task.id);
      try {
        let message: ReactNode;
        if (kind === "approve") {
          await approveTask(task.id);
          message = null;
        } else if (kind === "reject") {
          const outcome = await rejectTask(task.id, text);
          message =
            outcome.setAsideBranch === null ? (
              <>Rejected “{task.title}”. It had no branch to set aside.</>
            ) : (
              <>
                Rejected “{task.title}”. Its work is on <code>{outcome.setAsideBranch}</code>, and
                a pull request opened from it stays open.
              </>
            );
        } else {
          await requestTaskChanges(task.id, text);
          message = (
            <>
              Sent “{task.title}” back for another round. Its worktree and branch are kept.
            </>
          );
        }
        // Advance to the next in board order, unless a read that landed while
        // the command was in flight already did.
        if (currentIdRef.current === task.id) {
          const ids = queueRef.current.map((candidate) => candidate.id);
          go(
            successor(
              ids,
              task.id,
              ids.filter((id) => id !== task.id),
            ),
          );
        }
        setNotice(message);
        setNote(null);
        void refresh();
      } catch (thrown) {
        setError(toRimaiaError(thrown));
      } finally {
        ownActions.current.delete(task.id);
        setPending(false);
      }
    },
    [go, refresh],
  );

  const openPullRequest = useCallback(() => {
    if (!target) return;
    // The newest run's own `pr_url`, never an older run's: after a reject an
    // older run's PR belongs to the branch that was set aside.
    const url = target.task.lastRun?.prUrl ?? null;
    if (url === null) {
      setNotice("No pull request was recorded for the latest run.");
      return;
    }
    setNotice(null);
    setError(null);
    openExternalUrl(url).catch((thrown) => setError(toRimaiaError(thrown)));
  }, [target]);

  const step = useCallback(
    (direction: 1 | -1) => {
      const ids = queueRef.current.map((task) => task.id);
      const index = currentIdRef.current === null ? -1 : ids.indexOf(currentIdRef.current);
      // Stops at the ends: wrapping would hide that the queue is finished.
      const next = ids[index + direction];
      if (next !== undefined) go(next);
    },
    [go],
  );

  const run = useCallback(
    (command: ReviewCommand) => {
      switch (command) {
        case "next":
          return step(1);
        case "previous":
          return step(-1);
        case "show_digest":
          return onShowDigest();
        case "approve":
          return void decide("approve", "");
        case "reject":
        case "request_changes":
          if (currentIdRef.current === null) return;
          setNotice(null);
          setError(null);
          return setNote({ kind: command, text: "" });
        case "open_pull_request":
          return openPullRequest();
        case "open_worktree":
          if (current?.worktreePath) setMenuOpen(true);
          return;
        case "start_review":
          return;
      }
    },
    [current, decide, onShowDigest, openPullRequest, step],
  );

  const handleKey = useRef<(event: KeyboardEvent) => void>(() => {});
  handleKey.current = (event) => {
    if (event.defaultPrevented) return;
    // Bare letters are never shortcuts while the user is typing.
    if (isEditableTarget(event.target)) return;
    if (note !== null || pending) return;
    const command = reviewCommandForKey("queue", event);
    if (command === null) return;
    event.preventDefault();
    run(command);
  };
  useEffect(() => {
    const listener = (event: KeyboardEvent) => handleKey.current(event);
    window.addEventListener("keydown", listener);
    return () => window.removeEventListener("keydown", listener);
  }, []);

  const repositoryName = (task: TaskSummary) =>
    repositories.find((repository) => repository.id === task.repositoryId)?.name ?? null;

  const affected = affectedDependents(target?.dependents ?? []);
  const index = current ? queue.indexOf(current) : -1;

  return (
    <section className="review-queue" aria-label="Review queue">
      <div ref={surface} tabIndex={-1} className="review-surface">
        {readError && <ErrorBanner error={readError} />}
        {targetError && <ErrorBanner error={targetError} />}
        {notice && (
          <p className="review-notice" role="status">
            {notice}
          </p>
        )}

        {loading && queue.length === 0 ? (
          <p className="muted">Reading the board…</p>
        ) : queue.length === 0 ? (
          <div className="review-empty">
            <p>Nothing is waiting for review.</p>
            <button type="button" onClick={onShowDigest}>
              Back to the digest
            </button>
          </div>
        ) : (
          <div className="review-queue-layout">
            <ol className="review-queue-list" aria-label="In review">
              {queue.map((task) => (
                <li key={task.id}>
                  <button
                    type="button"
                    className="review-queue-item"
                    aria-current={task.id === currentId ? "true" : undefined}
                    onClick={() => go(task.id)}
                  >
                    <span className="review-queue-item-title">{task.title}</span>
                    {repositoryName(task) && (
                      <span className="muted review-queue-item-repo">{repositoryName(task)}</span>
                    )}
                  </button>
                </li>
              ))}
            </ol>
            {current && (
              <TaskReview
                task={current}
                repositoryName={repositoryName(current)}
                place={{ index: index + 1, total: queue.length }}
                target={target}
                board={tasks}
                now={now}
              >
                <div className="review-actions">
                  <button
                    type="button"
                    className="btn-primary"
                    disabled={pending}
                    onClick={() => void decide("approve", "")}
                  >
                    Approve <kbd>a</kbd>
                  </button>
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => run("reject")}
                  >
                    Reject <kbd>r</kbd>
                  </button>
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => run("request_changes")}
                  >
                    Needs changes <kbd>c</kbd>
                  </button>
                  <button type="button" disabled={!target} onClick={openPullRequest}>
                    Open PR <kbd>o</kbd>
                  </button>
                  {current.worktreePath && (
                    <OpenInMenu
                      taskId={current.id}
                      onError={setError}
                      open={menuOpen}
                      onOpenChange={setMenuOpen}
                    />
                  )}
                </div>
                {error && note === null && <ErrorBanner error={error} />}
                {note && (
                  <NoteStep
                    kind={note.kind}
                    text={note.text}
                    onChange={(text) => setNote({ kind: note.kind, text })}
                    onSubmit={() => void decide(note.kind, note.text)}
                    onCancel={() => setNote(null)}
                    affected={affected}
                    pending={pending}
                    error={error}
                  />
                )}
              </TaskReview>
            )}
          </div>
        )}
      </div>
      <KeyLegend mode="queue" noteOpen={note !== null} emptyQueue={queue.length === 0} />
    </section>
  );
}
