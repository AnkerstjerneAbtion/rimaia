import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { listen } from "@tauri-apps/api/event";

import { installTransports } from "./client";
import { setCommandTransports } from "./commands";
import {
  EVENT_SOURCES,
  MAX_TAILS_PER_STREAM,
  RECONNECT_BASE_MS,
  RECONNECT_CAP_MS,
  reconnectDelay,
  reconnectEventStream,
  setEventTransports,
  subscribeToEventStreamFailure,
  setEventTransport,
  subscribeToPlanPassProgress,
  subscribeToRepositoriesChanged,
  subscribeToRunnerChanged,
  subscribeToRunsChanged,
  subscribeToRunsTail,
  subscribeToSchedulesChanged,
  subscribeToSettingsChanged,
  subscribeToTasksChanged,
  subscribeToTeamsChanged,
} from "./events";
import type { EventStreamFailure, UnlistenFn } from "./events";
import { mockHttp } from "../test/http";
import type { MockEventStream } from "../test/http";
import type { RunTail } from "../types";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockListen = vi.mocked(listen);

afterEach(() => {
  setEventTransport(undefined);
  mockListen.mockReset();
});

describe("the event transport", () => {
  it("hands a subscriber the payload, not the Tauri event envelope", async () => {
    mockListen.mockImplementation(async (_name, handler) => {
      handler({ event: "tasks:changed", id: 1, payload: ["t1"] });
      return () => {};
    });
    const onChanged = vi.fn();

    await subscribeToTasksChanged(onChanged);

    expect(mockListen).toHaveBeenCalledWith("tasks:changed", expect.any(Function));
    expect(onChanged).toHaveBeenCalledWith(["t1"]);
  });

  it("routes every subscribe wrapper through the installed event transport", async () => {
    const seen: string[] = [];
    const unlisten = () => {};
    setEventTransport(async (event) => {
      seen.push(event);
      return unlisten;
    });
    const noop = () => {};

    const handles = await Promise.all([
      subscribeToTasksChanged(noop),
      subscribeToRepositoriesChanged(noop),
      subscribeToSettingsChanged(noop),
      subscribeToRunnerChanged(noop),
      subscribeToRunsChanged(noop),
      subscribeToRunsTail("run-1", noop),
      subscribeToSchedulesChanged(noop),
      subscribeToPlanPassProgress(noop),
      subscribeToTeamsChanged(noop),
    ]);

    expect(seen).toEqual([
      "tasks:changed",
      "repositories:changed",
      "settings:changed",
      "runner:changed",
      "runs:changed",
      "runs:tail",
      "schedules:changed",
      "plan-pass:progress",
      "teams:changed",
    ]);
    expect(handles).toEqual(seen.map(() => unlisten));
    expect(mockListen).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// The server's stream (task 049)
// ---------------------------------------------------------------------------

describe("the event source table", () => {
  it("has a source row for every event a subscribe wrapper uses", async () => {
    const seen: string[] = [];
    setEventTransport(async (event) => {
      seen.push(event);
      return () => {};
    });
    const noop = () => {};
    await Promise.all([
      subscribeToTasksChanged(noop),
      subscribeToRepositoriesChanged(noop),
      subscribeToSettingsChanged(noop),
      subscribeToRunnerChanged(noop),
      subscribeToRunsChanged(noop),
      subscribeToRunsTail("run-1", noop),
      subscribeToSchedulesChanged(noop),
      subscribeToPlanPassProgress(noop),
      subscribeToTeamsChanged(noop),
    ]);

    expect(Object.keys(EVENT_SOURCES).sort()).toEqual([...seen].sort());
    expect(EVENT_SOURCES).toEqual({
      "tasks:changed": "both",
      "repositories:changed": "both",
      "runs:changed": "board",
      "settings:changed": "board",
      "teams:changed": "board",
      "runs:tail": "board",
      "plan-pass:progress": "local",
      "runner:changed": "local",
      "schedules:changed": "local",
    });
  });
});

describe("the teams subscription", () => {
  it("subscribes to teams:changed through the board transport once", async () => {
    const board = vi.fn(async () => () => {});
    const local = vi.fn(async () => () => {});
    setEventTransports({ board, local });
    const onChanged = vi.fn();

    await subscribeToTeamsChanged(onChanged);

    expect(board).toHaveBeenCalledTimes(1);
    expect(board).toHaveBeenCalledWith("teams:changed", expect.any(Function), undefined);
    expect(local).not.toHaveBeenCalled();
  });
});

describe("the reconnect delay", () => {
  it("doubles the reconnect delay from one second to a thirty second cap", () => {
    expect([0, 1, 2, 3, 4, 5, 6, 20].map(reconnectDelay)).toEqual([
      1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000,
    ]);
    expect(RECONNECT_BASE_MS).toBe(1000);
    expect(RECONNECT_CAP_MS).toBe(30000);
  });
});

describe("one transport serving both sources", () => {
  it("delivers each payload once when one transport serves both sources", async () => {
    const delivered: string[] = [];
    const registered: string[] = [];
    setEventTransport(async (event, onPayload) => {
      registered.push(event);
      (onPayload as (payload: unknown) => void)([event]);
      return () => {};
    });

    await subscribeToTasksChanged((ids) => delivered.push(...ids));
    await subscribeToRepositoriesChanged((ids) => delivered.push(...ids));

    expect(registered).toEqual(["tasks:changed", "repositories:changed"]);
    expect(delivered).toEqual(["tasks:changed", "repositories:changed"]);

    setEventTransport(undefined);
    mockListen.mockClear();
    mockListen.mockResolvedValue(() => {});
    await subscribeToTasksChanged(() => {});
    await subscribeToRepositoriesChanged(() => {});
    expect(mockListen.mock.calls.map(([event]) => event)).toEqual([
      "tasks:changed",
      "repositories:changed",
    ]);
  });
});

describe("the server's event stream", () => {
  const unlistens: UnlistenFn[] = [];
  let http: ReturnType<typeof mockHttp>;
  let stream: MockEventStream;
  let failures: EventStreamFailure[];
  let stopListening: () => void;

  const flush = () => vi.advanceTimersByTimeAsync(0);
  const setCsrf = (value: string) => {
    document.cookie = `rimaia_csrf=${value}`;
  };
  const tail = (runId: string): RunTail => ({
    runId,
    elapsedMs: 1,
    turns: 1,
    currentTool: null,
    lastAssistantText: null,
  });
  const urls = () => stream.opens.map((open) => open.url);
  async function keep(subscribing: Promise<UnlistenFn>): Promise<UnlistenFn> {
    const unlisten = await subscribing;
    unlistens.push(unlisten);
    return unlisten;
  }

  beforeEach(() => {
    vi.useFakeTimers();
    http = mockHttp();
    stream = http.events();
    failures = [];
    stopListening = subscribeToEventStreamFailure((failure) => failures.push(failure));
    setCsrf("csrf-1");
    installTransports("browser");
  });

  afterEach(() => {
    for (const unlisten of unlistens.splice(0)) unlisten();
    stopListening();
    setCommandTransports(undefined);
    document.cookie = "rimaia_csrf=; expires=Thu, 01 Jan 1970 00:00:00 GMT";
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("opens one event stream for every subscriber and aborts it after the last unlisten", async () => {
    const first = await keep(subscribeToTasksChanged(() => {}));
    const second = await keep(subscribeToRunsChanged(() => {}));
    await flush();

    expect(stream.opens).toHaveLength(1);
    expect(stream.opens[0].url).toBe(`${window.location.origin}/api/v1/events`);
    expect(stream.open).toBe(true);

    first();
    await flush();
    expect(stream.open).toBe(true);

    second();
    await flush();
    expect(stream.open).toBe(false);
    expect(stream.opens).toHaveLength(1);
  });

  it("delivers a frame's payload to every wrapper subscribed to its event name", async () => {
    const tasksA = vi.fn();
    const tasksB = vi.fn();
    const runs = vi.fn();
    await keep(subscribeToTasksChanged(tasksA));
    await keep(subscribeToTasksChanged(tasksB));
    await keep(subscribeToRunsChanged(runs));
    await flush();

    stream.send("tasks:changed", ["t1"]);
    await flush();

    expect(tasksA).toHaveBeenCalledWith(["t1"]);
    expect(tasksB).toHaveBeenCalledWith(["t1"]);
    expect(runs).not.toHaveBeenCalled();
  });

  it("delivers an empty id list after a reconnect as an empty id list", async () => {
    const onChanged = vi.fn();
    await keep(subscribeToTasksChanged(onChanged));
    await flush();

    stream.fail();
    await vi.advanceTimersByTimeAsync(reconnectDelay(0));
    stream.send("tasks:changed", []);
    await flush();

    expect(stream.opens).toHaveLength(2);
    expect(onChanged).toHaveBeenCalledTimes(1);
    expect(onChanged).toHaveBeenCalledWith([]);
  });

  it("sends the credential and Rimaia-Protocol on the event stream request", async () => {
    await keep(subscribeToTasksChanged(() => {}));
    await flush();

    const [open] = stream.opens;
    expect(open.headers["x-rimaia-csrf"]).toBe("csrf-1");
    expect(open.headers["rimaia-protocol"]).toBe("1.0");
    expect(open.headers).not.toHaveProperty("authorization");
    expect(open.credentials).toBe("same-origin");
  });

  it("stops reconnecting and reports unauthenticated when the stream answers 401", async () => {
    const refusal = { code: "unauthenticated", message: "Sign in to continue." };
    stream.failOpen(401, refusal);

    await keep(subscribeToTasksChanged(() => {}));
    await vi.advanceTimersByTimeAsync(RECONNECT_CAP_MS * 2);

    expect(stream.opens).toHaveLength(1);
    expect(failures).toEqual([{ error: refusal }]);
  });

  it.each([
    [400, "invalid", "A tail id is not a run id."],
    [404, "not_found", "No such route."],
    [426, "upgrade_required", "Reload to update."],
  ])(
    "stops reconnecting and reports the refusal when the stream answers %i",
    async (status, code, message) => {
      stream.failOpen(status, { code, message });

      await keep(subscribeToTasksChanged(() => {}));
      await vi.advanceTimersByTimeAsync(RECONNECT_CAP_MS * 2);

      expect(stream.opens).toHaveLength(1);
      expect(failures).toEqual([{ error: { code, message } }]);
    },
  );

  it("reconnects after a failed stream once the backoff delay has passed", async () => {
    await keep(subscribeToTasksChanged(() => {}));
    await flush();
    expect(stream.opens).toHaveLength(1);

    stream.fail();
    await vi.advanceTimersByTimeAsync(reconnectDelay(0) - 1);
    expect(stream.opens).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(1);
    expect(stream.opens).toHaveLength(2);
    expect(stream.open).toBe(true);
    expect(failures).toEqual([]);
  });

  it("reconnects after the server ends the stream cleanly", async () => {
    await keep(subscribeToTasksChanged(() => {}));
    await flush();

    stream.end();
    await vi.advanceTimersByTimeAsync(reconnectDelay(0) - 1);
    expect(stream.opens).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(1);
    expect(stream.opens).toHaveLength(2);
    expect(failures).toEqual([]);
  });

  it("retries a 5xx and a 200 that is not an event stream with backoff", async () => {
    stream.failOpen(503, "Service Unavailable", "text/plain");
    await keep(subscribeToTasksChanged(() => {}));
    await flush();
    expect(stream.opens).toHaveLength(1);

    stream.failOpen(200, "<html>a proxy page</html>", "text/html");
    await vi.advanceTimersByTimeAsync(reconnectDelay(0));
    expect(stream.opens).toHaveLength(2);

    await vi.advanceTimersByTimeAsync(reconnectDelay(1) - 1);
    expect(stream.opens).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(stream.opens).toHaveLength(3);
    expect(stream.open).toBe(true);
    expect(failures).toEqual([]);
  });

  it("resets the reconnect delay after a stream opens", async () => {
    await keep(subscribeToTasksChanged(() => {}));
    await flush();

    stream.fail();
    await vi.advanceTimersByTimeAsync(reconnectDelay(0));
    expect(stream.opens).toHaveLength(2);

    // The second stream opened, so a failure now starts over at one second
    // rather than doubling to two.
    stream.fail();
    await vi.advanceTimersByTimeAsync(reconnectDelay(0) - 1);
    expect(stream.opens).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(1);
    expect(stream.opens).toHaveLength(3);
  });

  it("keeps subscribers after a fatal failure and reopens on reconnectEventStream", async () => {
    const onChanged = vi.fn();
    stream.failOpen(401, { code: "unauthenticated", message: "Sign in to continue." });
    await keep(subscribeToTasksChanged(onChanged));
    await flush();
    expect(failures).toHaveLength(1);

    setCsrf("csrf-2");
    reconnectEventStream();
    await flush();

    expect(stream.opens).toHaveLength(2);
    expect(stream.opens[1].headers["x-rimaia-csrf"]).toBe("csrf-2");
    stream.send("tasks:changed", ["t9"]);
    await flush();
    expect(onChanged).toHaveBeenCalledWith(["t9"]);
  });

  it("does nothing on reconnectEventStream when there are no subscribers", async () => {
    reconnectEventStream();
    await flush();

    expect(stream.opens).toHaveLength(0);
  });

  it("reconnects with both run ids when a second tail subscriber arrives", async () => {
    const first = vi.fn();
    const second = vi.fn();
    await keep(subscribeToRunsTail("run-a", first));
    await keep(subscribeToRunsTail("run-b", second));
    await flush();

    const base = `${window.location.origin}/api/v1/events`;
    expect(urls()).toEqual([`${base}?tail=run-a`, `${base}?tail=run-a&tail=run-b`]);

    stream.send("runs:tail", tail("run-a"));
    stream.send("runs:tail", tail("run-b"));
    await flush();
    expect(first.mock.calls).toEqual([[tail("run-a")]]);
    expect(second.mock.calls).toEqual([[tail("run-b")]]);
  });

  it("reconnects without a run id once its last tail subscriber unlistens", async () => {
    await keep(subscribeToTasksChanged(() => {}));
    const unlistenTail = await keep(subscribeToRunsTail("run-a", () => {}));
    await flush();
    expect(urls()[urls().length - 1]).toContain("?tail=run-a");

    unlistenTail();
    await flush();

    expect(urls()[urls().length - 1]).toBe(`${window.location.origin}/api/v1/events`);
    expect(stream.open).toBe(true);
  });

  it("reconnects once for tail subscriptions made in the same tick", async () => {
    await Promise.all([
      subscribeToRunsTail("run-a", () => {}).then((u) => unlistens.push(u)),
      subscribeToRunsTail("run-b", () => {}).then((u) => unlistens.push(u)),
      subscribeToRunsTail("run-c", () => {}).then((u) => unlistens.push(u)),
    ]);
    await flush();

    expect(stream.opens).toHaveLength(2);
    expect(urls()[1]).toBe(
      `${window.location.origin}/api/v1/events?tail=run-a&tail=run-b&tail=run-c`,
    );
  });

  it("refuses a ninth watched run without reconnecting", async () => {
    const ids = Array.from({ length: MAX_TAILS_PER_STREAM }, (_, index) => `run-${index}`);
    await Promise.all(ids.map((id) => subscribeToRunsTail(id, () => {}).then((u) => unlistens.push(u))));
    await flush();
    const before = stream.opens.length;

    await expect(subscribeToRunsTail("run-ninth", () => {})).rejects.toEqual({
      code: "invalid",
      message: "At most 8 run tails can be watched at once.",
    });
    await flush();

    expect(stream.opens).toHaveLength(before);
    expect(urls()[urls().length - 1]).not.toContain("run-ninth");
  });

  it("treats a 400 on an open that named tails as fatal and does not retry without them", async () => {
    const refusal = { code: "invalid", message: "A tail id is not a run id." };
    stream.failOpen(400, refusal);

    await keep(subscribeToRunsTail("run-a", () => {}));
    await vi.advanceTimersByTimeAsync(RECONNECT_CAP_MS * 2);

    expect(stream.opens).toHaveLength(1);
    expect(failures).toEqual([{ error: refusal }]);
  });

  it("reports no failure for a stream that opens but never sends a watched run's tail", async () => {
    const onTail = vi.fn();
    const onChanged = vi.fn();
    const unlistenTail = await keep(subscribeToRunsTail("run-stale", onTail));
    await keep(subscribeToTasksChanged(onChanged));
    await flush();

    stream.send("tasks:changed", ["t1"]);
    await vi.advanceTimersByTimeAsync(RECONNECT_CAP_MS * 2);

    expect(onChanged).toHaveBeenCalledWith(["t1"]);
    expect(onTail).not.toHaveBeenCalled();
    expect(failures).toEqual([]);
    expect(stream.opens).toHaveLength(1);

    unlistenTail();
    await flush();
    expect(stream.opens).toHaveLength(2);
  });
});

describe("the event transports by mode", () => {
  const SERVER = "https://board.example";
  let http: ReturnType<typeof mockHttp>;
  let stream: MockEventStream;
  let emit: (event: string, payload: unknown) => void;
  let unlistens: UnlistenFn[];
  const releaseOfListen = vi.fn();
  const flush = () => vi.advanceTimersByTimeAsync(0);

  beforeEach(() => {
    vi.useFakeTimers();
    http = mockHttp();
    stream = http.events();
    unlistens = [];
    releaseOfListen.mockReset();
    const handlers = new Map<string, (event: { payload: unknown }) => void>();
    mockListen.mockImplementation(async (event, handler) => {
      handlers.set(event, handler as (event: { payload: unknown }) => void);
      return releaseOfListen;
    });
    emit = (event, payload) => handlers.get(event)?.({ payload });
  });

  afterEach(() => {
    for (const unlisten of unlistens.splice(0)) unlisten();
    setCommandTransports(undefined);
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("subscribes board events over SSE and local events through listen in connected mode", async () => {
    installTransports("connected", { serverUrl: SERVER, token: "rmd_t" });
    const runs = vi.fn();
    const schedules = vi.fn();
    const progress = vi.fn();
    unlistens.push(await subscribeToRunsChanged(runs));
    unlistens.push(await subscribeToSchedulesChanged(schedules));
    unlistens.push(await subscribeToPlanPassProgress(progress));
    await flush();

    stream.send("runs:changed", ["r1"]);
    stream.send("schedules:changed", ["s-from-the-stream"]);
    stream.send("plan-pass:progress", { from: "the stream" });
    emit("runs:changed", ["r-from-listen"]);
    emit("schedules:changed", ["s1"]);
    emit("plan-pass:progress", { done: 1 });
    await flush();

    expect(runs.mock.calls).toEqual([[["r1"]]]);
    expect(schedules.mock.calls).toEqual([[["s1"]]]);
    expect(progress.mock.calls).toEqual([[{ done: 1 }]]);
    expect(mockListen.mock.calls.map(([event]) => event).sort()).toEqual([
      "plan-pass:progress",
      "schedules:changed",
    ]);
    expect(stream.opens[0].headers.authorization).toBe("Bearer rmd_t");
  });

  it("delivers both sides of a both event in connected mode and releases both on unlisten", async () => {
    installTransports("connected", { serverUrl: SERVER, token: "rmd_t" });
    const onChanged = vi.fn();
    const unlisten = await subscribeToTasksChanged(onChanged);
    await flush();

    stream.send("tasks:changed", ["from-board"]);
    await flush();
    emit("tasks:changed", ["from-worktree"]);
    expect(onChanged.mock.calls).toEqual([[["from-board"]], [["from-worktree"]]]);

    unlisten();
    await flush();
    stream.send("tasks:changed", ["late"]);
    await flush();
    expect(onChanged).toHaveBeenCalledTimes(2);
    expect(releaseOfListen).toHaveBeenCalledTimes(1);
    expect(stream.open).toBe(false);
  });

  it("releases the first subscription when the second one fails", async () => {
    const release = vi.fn();
    setEventTransports({
      board: async () => release,
      local: async () => {
        throw { code: "internal", message: "no bridge" };
      },
    });

    await expect(subscribeToTasksChanged(() => {})).rejects.toEqual({
      code: "internal",
      message: "no bridge",
    });
    expect(release).toHaveBeenCalledTimes(1);
  });

  it("resolves a local-only subscription in the browser without subscribing to anything", async () => {
    installTransports("browser");

    const unlisten = await subscribeToSchedulesChanged(() => {});
    unlistens.push(unlisten);
    await flush();

    expect(typeof unlisten).toBe("function");
    expect(stream.opens).toHaveLength(0);
    expect(mockListen).not.toHaveBeenCalled();
  });

  it("opens the stream and nothing else for a tasks:changed subscription in the browser", async () => {
    installTransports("browser");

    unlistens.push(await subscribeToTasksChanged(() => {}));
    await flush();

    expect(stream.opens).toHaveLength(1);
    expect(mockListen).not.toHaveBeenCalled();
  });
});
