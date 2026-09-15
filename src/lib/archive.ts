import { formatBytes } from "./format";
import type { ArchiveReport, OnArchive, OnArchiveOutcome, Repository } from "../types";

/**
 * Sentences about ADR-0025's archiving, kept out of the components that render
 * them.
 *
 * Here rather than inline for the reason `lib/board.ts` exists: these are the
 * parts a test can pin exactly, and the parts most likely to be got subtly
 * wrong — an outcome variant left unhandled, or a confirmation that says
 * "Archive" when the repository is configured to delete a 900 MB checkout.
 */

/** One sentence for what a repository's cleanup did to one task. */
export function describeCleanup(outcome: OnArchiveOutcome): string | null {
  switch (outcome.kind) {
    case "nothing":
      return null;
    case "worktreeRemoved":
      return `Removed its worktree, freeing ${formatBytes(outcome.bytesFreed)}.`;
    case "scriptRan":
      if (outcome.exitCode === 0) return "Your cleanup script ran.";
      if (outcome.exitCode === null) {
        return "Your cleanup script did not finish and was stopped.";
      }
      return `Your cleanup script exited ${outcome.exitCode}.`;
    case "failed":
      return outcome.reason;
  }
}

/**
 * What the archive confirmation has to say *before* the click.
 *
 * "Archive" and "Archive, and delete a 900 MB checkout" must not be the same
 * sentence — that is the whole reason this is computed from the repository
 * rather than written once into a button label.
 */
export function describeCleanupIntent(repository: Repository | undefined): string | null {
  switch (repository?.onArchive) {
    case undefined:
    case "none":
      return null;
    case "remove_worktree":
      return "Its git worktree will be deleted. The branch is kept, and a dirty or unpushed worktree is left alone.";
    case "script":
      return `Your cleanup script will run: ${repository.onArchiveScript ?? "(none configured)"}. Rimaia applies none of its own guards to it.`;
  }
}

/** One sentence for a bulk archive, naming both halves — what went and what
 *  was refused. A report that only counted successes would hide the refusals,
 *  which are the part the user has to act on. */
export function describeArchiveReport(report: ArchiveReport): string {
  const archived = `Archived ${report.archived.length} ${plural(report.archived.length, "task")}.`;
  const attention = report.archived.filter((entry) => needsAttention(entry.cleanup));

  const notes: string[] = [];
  if (attention.length > 0) {
    notes.push(
      `Cleanup did not go cleanly for ${attention.length} of them — ${attention
        .map((entry) => `${entry.title}: ${describeCleanup(entry.cleanup)}`)
        .join(" ")}`,
    );
  }
  if (report.refused.length > 0) {
    notes.push(
      `Left ${report.refused.length} alone — ${report.refused
        .map((refusal) => `${refusal.title}: ${refusal.reason}`)
        .join(" ")}`,
    );
  }

  return [archived, ...notes].join(" ");
}

/** Worth putting in front of the user as a problem. Mirrors
 *  `OnArchiveOutcome::needs_attention` in core — a non-zero exit counts,
 *  because the user configured a cleanup and it reported that it did not
 *  work. */
export function needsAttention(outcome: OnArchiveOutcome): boolean {
  switch (outcome.kind) {
    case "nothing":
    case "worktreeRemoved":
      return false;
    case "scriptRan":
      return outcome.exitCode !== 0;
    case "failed":
      return true;
  }
}

/** The label the Settings radio and the card confirmation both use, so one
 *  vocabulary describes the slot everywhere. */
export const ON_ARCHIVE_LABELS: Record<OnArchive, string> = {
  none: "Leave everything alone",
  remove_worktree: "Delete the task's worktree",
  script: "Run my own script",
};

function plural(count: number, noun: string): string {
  return count === 1 ? noun : `${noun}s`;
}
