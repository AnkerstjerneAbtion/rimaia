import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { PlanProgress, RunTail } from "../types";

export type { UnlistenFn };

/**
 * What every subscribe wrapper goes through: an event name and a callback
 * that receives the **payload**, never a Tauri event envelope. Replaceable
 * for the same reason `CommandTransport` is (ADR-0034 point 4).
 */
export type EventTransport = <P>(
  event: string,
  onPayload: (payload: P) => void,
) => Promise<UnlistenFn>;

let installedTransport: EventTransport | undefined;

/** Replace the transport; an entry point calls this, a component never does. */
export function setEventTransport(transport: EventTransport | undefined): void {
  installedTransport = transport;
}

const tauriTransport: EventTransport = <P>(event: string, onPayload: (payload: P) => void) =>
  listen<P>(event, (e) => onPayload(e.payload));

function subscribe<P>(event: string, onPayload: (payload: P) => void): Promise<UnlistenFn> {
  return (installedTransport ?? tauriTransport)<P>(event, onPayload);
}

/**
 * The only module in the frontend that imports `@tauri-apps/api/event`
 * (seam-contract D7) — the event-side mirror of the rule `commands.ts`
 * states for `invoke`. Every Tauri event ADR-0018's shell forwarder emits
 * gets one named, typed subscribe wrapper here: payload typed once, the
 * `unlisten` handle handed back to the caller, so a component never guesses
 * at a payload shape the backend might not actually be sending.
 *
 * Created here rather than by task 005, which seam-contract D7 names as this
 * file's creator for `tasks:changed` — task 003 and 004's shell work landed
 * `repositories:changed` and `tasks:changed` together, so starting the file
 * now (with those two) rather than waiting is strictly earlier, not a
 * deviation. `runs:changed` (task 008) and `settings:changed` (task 006) are
 * left for the tasks that first have something to publish on them, the same
 * incremental-extension pattern D7 already describes for 008 and 009.
 *
 * Task 009 adds no new event of its own: a queue Start/Pause/Resume/Stop
 * writes `queue_state`, a runner key, which announces itself on
 * `runner:changed` below (task 048), and a task the queue claims or finishes
 * already announces itself on `tasks:changed` / `runs:changed`. The Runs view
 * re-fetches `getQueueStatus` from `./commands` on those rather than waiting
 * on a dedicated channel.
 *
 * **Board events and local events** (task 048, ADR-0034 point 4). Most events
 * here are about the board, which a server streams to every member of a team.
 * `runner:changed` and `schedules:changed` are about this machine only, and
 * `tasks:changed` and `repositories:changed` have a source on each side: a
 * board row, or this machine's worktree record or checkout. Every name and
 * payload is `rimaia_core::api::events`'s, so the two forwarders cannot
 * disagree.
 *
 * **An empty id array means "re-read this entity wholesale," never "nothing
 * changed."** Only a subscription's own recovery sends one: every board
 * stream opens with one per board entity, and a forwarder that falls behind
 * its channel's buffer hears `RecvError::Lagged` and answers by emitting
 * every entity's event once with an empty array rather than trying to
 * replay what it missed. A listener that ignores an empty array because it
 * looks like "no ids" silently stops refreshing the moment a lag happens —
 * treat it exactly like "every id changed".
 */
export function subscribeToTasksChanged(
  onChanged: (taskIds: string[]) => void,
): Promise<UnlistenFn> {
  return subscribe<string[]>("tasks:changed", onChanged);
}

/** See {@link subscribeToTasksChanged} for the empty-array contract; the
 *  same rule applies here, scoped to `repositories:changed`. */
export function subscribeToRepositoriesChanged(
  onChanged: (repositoryIds: string[]) => void,
): Promise<UnlistenFn> {
  return subscribe<string[]>("repositories:changed", onChanged);
}

/**
 * `settings:changed` carries no ids — a team's settings, or your own, are a
 * handful of rows, so every write (base instructions, the strategy catalogue,
 * the review loop, your subscription cost) announces the same signal and
 * every consumer just re-reads all of it, mirroring `rimaia_core::db::settings`'s
 * own doc comment on why the event it publishes is untyped. This machine's
 * own keys are not here: see {@link subscribeToRunnerChanged}.
 */
export function subscribeToSettingsChanged(onChanged: () => void): Promise<UnlistenFn> {
  return subscribe<null>("settings:changed", () => onChanged());
}

/**
 * `runner:changed` (task 048) — one of this machine's runner keys changed:
 * the queue switch (`queue_state`), its capacity and mode, the run window,
 * the usage-limit pause, the MCP port, the run environment and the rest of
 * seam-contract D28 part 4's *Runner* row. Also sent when the queue records a
 * refused scheduled start. Carries no ids, for `settings:changed`'s reason.
 *
 * **Moved off `settings:changed`, and why.** These keys belong to the machine
 * a runner runs on, not to a team (ADR-0031 point 6), and a server has no
 * machine to announce: they are a local event the desktop shell forwards,
 * which a server's stream never carries. A component that reads a runner key
 * subscribes here; one that reads both halves, such as the queue status,
 * subscribes to both.
 */
export function subscribeToRunnerChanged(onChanged: () => void): Promise<UnlistenFn> {
  return subscribe<null>("runner:changed", () => onChanged());
}

/** See {@link subscribeToTasksChanged} for the empty-array contract; the
 *  same rule applies here, scoped to `runs:changed` (task 008). */
export function subscribeToRunsChanged(
  onChanged: (runIds: string[]) => void,
): Promise<UnlistenFn> {
  return subscribe<string[]>("runs:changed", onChanged);
}

/**
 * `runs:tail` (task 008, seam-contract D14) — a live snapshot of whatever
 * run is in flight: elapsed time, turn count, the current tool call, the
 * last assistant text. **Not** the empty-array recovery signal every other
 * event here uses: D14 rule 1 is that a dropped snapshot is discarded and
 * counted on the backend, never recovered, because it is already on disk in
 * the run's transcript — there is nothing for a listener to special-case. A
 * component that starts watching mid-run should seed itself from
 * `getRunTail` in `./commands` before subscribing here, and filter on the
 * snapshot's own `runId` if more than one run might be in flight.
 */
export function subscribeToRunsTail(onTail: (tail: RunTail) => void): Promise<UnlistenFn> {
  return subscribe<RunTail>("runs:tail", onTail);
}

/**
 * `schedules:changed` (task 013) — a row in `schedules` was created, edited,
 * enabled, disabled, deleted, or fired.
 *
 * **A channel of its own rather than riding `settings:changed`**, and the
 * distinction is the one `ChangeEvent` is built on. `settings` is a key/value
 * table whose every consumer re-reads all of it, which is why that event
 * carries no ids. `schedules` is a table of *entities* the user creates, names
 * and deletes — the same kind of thing `tasks` and `repositories` are — so it
 * carries ids for the same reason they do, and a panel listing thirty schedules
 * is not obliged to re-read them because a base-instructions textarea was
 * saved. Seam-contract D24.
 *
 * The *window* a schedule opens is a runner key and announces itself on
 * `runner:changed`, not here. That is not an inconsistency: a window is one
 * singleton fact about the installation, and the Runs view reading it re-reads
 * the whole queue status anyway. Like `runner:changed`, this is a local event
 * (task 048): a schedule is this machine's.
 *
 * See {@link subscribeToTasksChanged} for the empty-array contract; the same
 * rule applies here.
 */
export function subscribeToSchedulesChanged(
  onChanged: (scheduleIds: string[]) => void,
): Promise<UnlistenFn> {
  return subscribe<string[]>("schedules:changed", onChanged);
}

/**
 * `plan-pass:progress` (task 023) — one card of a planning pass just finished.
 *
 * Its own channel rather than a `ChangeEvent`, and the distinction matters:
 * ADR-0018's channel says "a row changed, re-read it", and every proposal a
 * pass writes already announces itself there through `tasks:changed`. This
 * carries something no row holds — which card the pass is on, how many are
 * left, and what has been spent so far — which is the progress the user stayed
 * to watch. The same argument seam-contract D14 makes for the live run tail.
 *
 * Only ever emitted while `plan_tasks_strategy` is outstanding.
 */
export function subscribeToPlanPassProgress(
  onProgress: (progress: PlanProgress) => void,
): Promise<UnlistenFn> {
  return subscribe<PlanProgress>("plan-pass:progress", onProgress);
}
