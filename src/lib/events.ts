import { fetchEventSource } from "@microsoft/fetch-event-source";
import { listen } from "@tauri-apps/api/event";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { credentialsMode, errorFromResponse, transportHeaders } from "./commands";
import type { Credential } from "./commands";
import type { PlanProgress, RimaiaError, RunTail } from "../types";

export type { UnlistenFn };

/** What a subscription can tell the transport that the event name cannot. */
export interface SubscribeOptions {
  /** The run a `runs:tail` subscription watches. Only the SSE transport reads
   *  it: the stream carries a run's tail only if its URL named the run. */
  runId?: string;
}

/**
 * What every subscribe wrapper goes through: an event name and a callback
 * that receives the **payload**, never a Tauri event envelope. Replaceable
 * for the same reason `CommandTransport` is (ADR-0034 point 4).
 */
export type EventTransport = <P>(
  event: string,
  onPayload: (payload: P) => void,
  options?: SubscribeOptions,
) => Promise<UnlistenFn>;

/**
 * One transport per event source (task 049). `board` carries what a server
 * announces; `local` carries what this machine announces, and is `null` in a
 * browser, which has no machine.
 */
export interface EventTransports {
  board: EventTransport;
  local: EventTransport | null;
}

/** Where an event arrives from. `both` is an event two machines announce. */
export type EventSource = "board" | "local" | "both";

/**
 * Where each event a subscribe wrapper uses arrives from (task 048's wire
 * table, ADR-0034 point 4).
 *
 * Two names are `both`, because two machines write what they announce: a
 * task's board row changes on the server and its worktree record changes on
 * the desktop; a repository's row is the team's and its checkout is the
 * runner's. The payload means the same on both sides, ids to re-read, so a
 * subscriber cannot tell which side spoke and does not need to.
 * `plan-pass:progress` is `local`: its one producer is the desktop's own
 * planning pass, which emits straight into Tauri.
 */
export const EVENT_SOURCES: Readonly<Record<string, EventSource>> = {
  "tasks:changed": "both",
  "repositories:changed": "both",
  "runs:changed": "board",
  "settings:changed": "board",
  "teams:changed": "board",
  "runs:tail": "board",
  "plan-pass:progress": "local",
  "runner:changed": "local",
  "schedules:changed": "local",
};

let installedTransports: EventTransports | undefined;

/** Replace both transports; an entry point calls this, a component never does. */
export function setEventTransports(transports: EventTransports | undefined): void {
  installedTransports = transports;
}

/** Replace the transport for both sources at once: what fixture mode wants. */
export function setEventTransport(transport: EventTransport | undefined): void {
  installedTransports = transport ? { board: transport, local: transport } : undefined;
}

/** Tauri's `listen`, which both sources use when nothing is installed. */
export const listenEventTransport: EventTransport = <P>(
  event: string,
  onPayload: (payload: P) => void,
) => listen<P>(event, (e) => onPayload(e.payload));

/**
 * Subscribes through each distinct transport the event's source names: one for
 * `board` or `local`, and for `both` the board's and the local one unless they
 * are the same transport (compared by identity), so solo and fixture mode,
 * where one transport serves both, still deliver each payload once.
 */
async function subscribe<P>(
  event: string,
  onPayload: (payload: P) => void,
  options?: SubscribeOptions,
): Promise<UnlistenFn> {
  const source = EVENT_SOURCES[event];
  if (source === undefined) {
    throw new Error(`events.ts has no source for the event \`${event}\``);
  }
  const { board, local } = installedTransports ?? {
    board: listenEventTransport,
    local: listenEventTransport,
  };
  const transports: EventTransport[] = [];
  if (source === "board") {
    transports.push(board);
  } else if (source === "local") {
    if (local) transports.push(local);
  } else {
    transports.push(board);
    if (local && local !== board) transports.push(local);
  }

  const released: UnlistenFn[] = [];
  for (const transport of transports) {
    try {
      released.push(await transport<P>(event, onPayload, options));
    } catch (thrown) {
      // A failed subscribe leaves nothing behind.
      for (const unlisten of released) unlisten();
      throw thrown;
    }
  }
  // One subscription hands back the transport's own handle.
  if (released.length === 1) return released[0];
  return () => {
    for (const unlisten of released) unlisten();
  };
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
 * `teams:changed` (task 051) — a team itself changed: it was created or
 * deleted, someone joined, left or was removed, a role changed, or an
 * invitation was accepted or revoked. The payload is the team ids; an empty
 * array is {@link subscribeToTasksChanged}'s "re-read wholesale", which every
 * stream sends when it opens. That burst is what makes the switcher re-read
 * after a change to the person's own team set, because the server ends their
 * stream at once and the event itself may be lost with it.
 */
export function subscribeToTeamsChanged(
  onChanged: (teamIds: string[]) => void,
): Promise<UnlistenFn> {
  return subscribe<string[]>("teams:changed", onChanged);
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
 * `getRunTail` in `./commands` before subscribing here. The subscription
 * names the run it watches (task 049): a server's stream carries a run's tail
 * only if its URL named the run, and this wrapper drops any other run's
 * snapshot that solo's `listen` delivers.
 */
export function subscribeToRunsTail(
  runId: string,
  onTail: (tail: RunTail) => void,
): Promise<UnlistenFn> {
  // Solo's `listen` admits every run's tail, and the stream carries only the
  // runs it was asked for, so the filter keeps both transports to the one run.
  return subscribe<RunTail>(
    "runs:tail",
    (tail) => {
      if (tail.runId === runId) onTail(tail);
    },
    { runId },
  );
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

// ---------------------------------------------------------------------------
// The server's stream (task 049, ADR-0034 point 3)
// ---------------------------------------------------------------------------

/** The first reconnect delay, doubled per failed attempt up to the cap. A
 *  dropped connection on a laptop that slept must not hammer the server. */
export const RECONNECT_BASE_MS = 1000;
/** The longest a reconnect waits, so waking up never costs a minute. */
export const RECONNECT_CAP_MS = 30000;
/** The most runs one stream can carry a tail for: the server refuses a ninth
 *  with `400`, taking every board event with it. */
export const MAX_TAILS_PER_STREAM = 8;

/** How long attempt `attempt` (counted from 0) waits before reconnecting. */
export function reconnectDelay(attempt: number): number {
  return Math.min(RECONNECT_BASE_MS * 2 ** attempt, RECONNECT_CAP_MS);
}

/** A stream failure asking again cannot fix: reported, and not retried. */
export interface EventStreamFailure {
  error: RimaiaError;
}

class FatalStreamError extends Error {
  constructor(readonly error: RimaiaError) {
    super(error.message);
  }
}

const failureSubscribers = new Set<(failure: EventStreamFailure) => void>();

/**
 * Hears every fatal failure of an event stream: `401` (the credential), `400`
 * (a bad tail), `404` (no events route at all), `426` (version skew). Only
 * fatal ones: a retriable failure is retried with backoff and reported to no
 * one, because the stream recovers on its own and every reopen brings the
 * server's burst. The stream stays closed with its subscribers kept; nothing
 * reopens it but {@link reconnectEventStream}.
 */
export function subscribeToEventStreamFailure(
  onFailure: (failure: EventStreamFailure) => void,
): () => void {
  failureSubscribers.add(onFailure);
  return () => {
    failureSubscribers.delete(onFailure);
  };
}

/** The streams that have subscribers, for {@link reconnectEventStream}. */
const liveStreams = new Set<{ reopen: () => void }>();

/**
 * Reopens the stream with the current credential and headers when it has
 * subscribers, closing any stream still open; does nothing when it has none.
 * For after a sign-in and on a team switch.
 */
export function reconnectEventStream(): void {
  for (const stream of [...liveStreams]) stream.reopen();
}

/**
 * Server-sent events over the board's one stream per client (ADR-0034 point
 * 3): the first subscription opens `GET <baseUrl>/api/v1/events`, later ones
 * share it, and the last unlisten aborts it. A frame's `event` is the event
 * name and its `data` the JSON payload the Tauri forwarder would have sent.
 *
 * `@microsoft/fetch-event-source` is imported here and nowhere else (D34).
 * Its retry loop is used as it is, with the failure classes decided in
 * `onopen` (a `4xx` is fatal and a `5xx` or a wrong content type is not) and
 * obeyed in `onerror`. The request headers are rebuilt for **every** request
 * the library makes, its own retries included, so a sign-in between two
 * attempts is picked up.
 */
export function sseEventTransport(connection: {
  baseUrl: string;
  credential: Credential;
}): EventTransport {
  const baseUrl = connection.baseUrl.replace(/\/+$/, "");
  const { credential } = connection;

  const listeners = new Map<string, Set<(payload: unknown) => void>>();
  const tails = new Map<string, number>();
  let subscribers = 0;
  let controller: AbortController | null = null;
  // `open` while a stream is live or retrying, `fatal` after a failure asking
  // again cannot fix, `idle` with no subscribers.
  let state: "idle" | "open" | "fatal" = "idle";
  let attempt = 0;
  let reconnectQueued = false;

  const stream = { reopen };

  function streamUrl(): string {
    const query = [...tails.keys()].map((id) => `tail=${encodeURIComponent(id)}`).join("&");
    return `${baseUrl}/api/v1/events${query ? `?${query}` : ""}`;
  }

  function close() {
    controller?.abort();
    controller = null;
  }

  function open() {
    close();
    attempt = 0;
    state = "open";
    const mine = new AbortController();
    controller = mine;
    const current = () => controller === mine;

    fetchEventSource(streamUrl(), {
      signal: mine.signal,
      headers: transportHeaders(credential),
      credentials: credentialsMode(credential),
      // Read at call time, so a test's stub is the one reached, and rebuilt
      // headers ride on the library's own retries as well.
      fetch: (input, init) =>
        globalThis.fetch(input, {
          ...init,
          headers: { ...init?.headers, ...transportHeaders(credential) },
        }),
      async onopen(response) {
        if (!current()) return;
        const type = response.headers.get("content-type") ?? "";
        if (response.status === 200 && type.startsWith("text/event-stream")) {
          attempt = 0;
          return;
        }
        if (response.status >= 400 && response.status < 500) {
          throw new FatalStreamError(await errorFromResponse(response));
        }
        throw new Error(
          response.status === 200
            ? "The server answered the event stream with something that is not one."
            : `The server answered the event stream with HTTP ${response.status}.`,
        );
      },
      onmessage(message) {
        if (!current()) return;
        let payload: unknown;
        try {
          payload = JSON.parse(message.data);
        } catch {
          return;
        }
        for (const listener of [...(listeners.get(message.event) ?? [])]) {
          try {
            listener(payload);
          } catch (thrown) {
            // A subscriber's bug must not look like a stream failure, which
            // would reconnect and replay the opening burst into it again.
            console.error(`a ${message.event} subscriber threw`, thrown);
          }
        }
      },
      onclose() {
        // A server ends a stream on purpose (revalidation, a change of
        // teams) and expects the client to come back; the library reconnects
        // only if this throws.
        throw new Error("The server ended the event stream.");
      },
      onerror(error) {
        if (error instanceof FatalStreamError) throw error;
        return reconnectDelay(attempt++);
      },
    }).catch((thrown: unknown) => {
      if (!current() || !(thrown instanceof FatalStreamError)) return;
      state = "fatal";
      controller = null;
      for (const subscriber of [...failureSubscribers]) subscriber({ error: thrown.error });
    });
  }

  function reopen() {
    if (subscribers > 0) open();
  }

  // Changing the set of watched runs means reconnecting; the microtask lets a
  // view that mounts three run cards reconnect once.
  function reconnectForTails() {
    if (reconnectQueued) return;
    reconnectQueued = true;
    queueMicrotask(() => {
      reconnectQueued = false;
      if (subscribers > 0 && state === "open") open();
    });
  }

  return async <P>(
    event: string,
    onPayload: (payload: P) => void,
    options?: SubscribeOptions,
  ): Promise<UnlistenFn> => {
    const runId = options?.runId;
    const addsTail = runId !== undefined && !tails.has(runId);
    if (addsTail && tails.size >= MAX_TAILS_PER_STREAM) {
      throw {
        code: "invalid",
        message: `At most ${MAX_TAILS_PER_STREAM} run tails can be watched at once.`,
      } satisfies RimaiaError;
    }

    const listener = onPayload as (payload: unknown) => void;
    const forEvent = listeners.get(event) ?? new Set();
    forEvent.add(listener);
    listeners.set(event, forEvent);
    if (runId !== undefined) tails.set(runId, (tails.get(runId) ?? 0) + 1);
    subscribers += 1;

    if (subscribers === 1) {
      liveStreams.add(stream);
      open();
    } else if (addsTail) {
      reconnectForTails();
    }

    let released = false;
    return () => {
      if (released) return;
      released = true;
      forEvent.delete(listener);
      subscribers -= 1;
      let removesTail = false;
      if (runId !== undefined) {
        const remaining = (tails.get(runId) ?? 1) - 1;
        if (remaining > 0) {
          tails.set(runId, remaining);
        } else {
          tails.delete(runId);
          removesTail = true;
        }
      }
      if (subscribers === 0) {
        close();
        state = "idle";
        liveStreams.delete(stream);
      } else if (removesTail) {
        reconnectForTails();
      }
    };
  };
}
