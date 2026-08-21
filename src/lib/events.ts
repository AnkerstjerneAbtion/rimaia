import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";

import type { RunTail } from "../types";

export type { UnlistenFn };

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
 * **An empty id array means "re-read this entity wholesale," never "nothing
 * changed."** ADR-0018's shell forwarder is the only thing entitled to send
 * one: when its subscription falls behind the change-event broadcast
 * channel's buffer, it hears `RecvError::Lagged` and answers by emitting
 * every entity's event once with an empty array rather than trying to
 * replay what it missed. A listener that ignores an empty array because it
 * looks like "no ids" silently stops refreshing the moment a lag happens —
 * treat it exactly like "every id changed".
 */
export function subscribeToTasksChanged(
  onChanged: (taskIds: string[]) => void,
): Promise<UnlistenFn> {
  return listen<string[]>("tasks:changed", (event) => onChanged(event.payload));
}

/** See {@link subscribeToTasksChanged} for the empty-array contract; the
 *  same rule applies here, scoped to `repositories:changed`. */
export function subscribeToRepositoriesChanged(
  onChanged: (repositoryIds: string[]) => void,
): Promise<UnlistenFn> {
  return listen<string[]>("repositories:changed", (event) => onChanged(event.payload));
}

/**
 * `settings:changed` carries no ids — the whole `settings` table is a
 * handful of rows, so every write (base instructions, run environment)
 * announces the same signal and every consumer just re-reads all of it,
 * mirroring `rimaia_core::db::settings`'s own doc comment on why the event
 * it publishes is untyped.
 */
export function subscribeToSettingsChanged(onChanged: () => void): Promise<UnlistenFn> {
  return listen<null>("settings:changed", () => onChanged());
}

/** See {@link subscribeToTasksChanged} for the empty-array contract; the
 *  same rule applies here, scoped to `runs:changed` (task 008). */
export function subscribeToRunsChanged(
  onChanged: (runIds: string[]) => void,
): Promise<UnlistenFn> {
  return listen<string[]>("runs:changed", (event) => onChanged(event.payload));
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
  return listen<RunTail>("runs:tail", (event) => onTail(event.payload));
}
