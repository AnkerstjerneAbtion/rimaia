import { afterEach, describe, expect, it, vi } from "vitest";

import { listen } from "@tauri-apps/api/event";

import {
  setEventTransport,
  subscribeToPlanPassProgress,
  subscribeToRepositoriesChanged,
  subscribeToRunsChanged,
  subscribeToRunsTail,
  subscribeToSchedulesChanged,
  subscribeToSettingsChanged,
  subscribeToTasksChanged,
} from "./events";

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
      subscribeToRunsChanged(noop),
      subscribeToRunsTail(noop),
      subscribeToSchedulesChanged(noop),
      subscribeToPlanPassProgress(noop),
    ]);

    expect(seen).toEqual([
      "tasks:changed",
      "repositories:changed",
      "settings:changed",
      "runs:changed",
      "runs:tail",
      "schedules:changed",
      "plan-pass:progress",
    ]);
    expect(handles).toEqual(seen.map(() => unlisten));
    expect(mockListen).not.toHaveBeenCalled();
  });
});
