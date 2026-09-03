import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { SchedulesSection } from "./SchedulesSection";
import type { PreflightSummary, QueueEntry, ScheduleView } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts` or `lib/events.ts` — see
// `StorageSection.test.tsx`'s comment for why: `commands.ts` is the only module
// that imports `invoke`, so mocking here exercises the real call path including
// `toRimaiaError`, instead of stubbing the frontend's own wrappers.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

const ZONES = ["Europe/Copenhagen", "UTC"];

function nightly(overrides: Partial<ScheduleView> = {}): ScheduleView {
  return {
    id: "sched-1",
    name: "Nightly",
    mode: "sequential",
    cron: "0 22 * * *",
    startAt: null,
    maxConcurrency: 2,
    enabled: true,
    timezone: "Europe/Copenhagen",
    stopAt: "06:00",
    lastFiredAt: null,
    armedAt: "2026-09-02T12:00:00Z",
    nextFireAt: "2026-09-02T20:00:00Z",
    nextFireError: null,
    ...overrides,
  };
}

function entry(overrides: Partial<QueueEntry> = {}): QueueEntry {
  return {
    taskId: "task-1",
    title: "Alpha",
    repositoryId: "repo-1",
    queuePosition: 1,
    skip: null,
    resumeAfter: null,
    ...overrides,
  };
}

function preflight(overrides: Partial<PreflightSummary> = {}): PreflightSummary {
  return {
    scheduleId: "sched-1",
    scheduleName: "Nightly",
    nextFireAt: "2026-09-02T20:00:00Z",
    closesAt: "2026-09-03T04:00:00Z",
    mode: "sequential",
    maxConcurrency: 2,
    plan: [entry()],
    ...overrides,
  };
}

/** The backend, with `list_schedules` answering `schedules` and everything else
 *  falling through to a named failure — so a command the component calls but
 *  the test did not anticipate fails loudly rather than resolving `undefined`. */
function backend(schedules: ScheduleView[], extra: Record<string, unknown> = {}) {
  mockInvoke.mockImplementation(async (command) => {
    if (command === "list_schedules") return schedules;
    if (command === "list_timezones") return ZONES;
    if (command in extra) return extra[command];
    throw new Error(`unexpected command: ${String(command)}`);
  });
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
  vi.useRealTimers();
});

describe("SchedulesSection", () => {
  it("shows the next fire time absolutely and relatively, which is the whole point of the panel", async () => {
    // Task 013's Scope: the next fire time is shown for each "so a wrong cron
    // expression is caught in the evening rather than discovered in the
    // morning". Both forms, because they catch different mistakes — "22:00"
    // looks right when the timezone is wrong, and "in 6 hours" does not.
    //
    // Built as an offset from the real clock rather than pinned with fake
    // timers: `findBy*` polls on a real timer, so installing fake ones here
    // deadlocks the query rather than freezing time for it.
    const inSixHours = new Date(Date.now() + 6 * 60 * 60 * 1000);
    backend([nightly({ nextFireAt: inSixHours.toISOString() })]);

    render(<SchedulesSection />);

    const next = await screen.findByText(/^Next: /);
    expect(next).toHaveTextContent("in 6 hours");
    expect(next).toHaveTextContent(
      inSixHours.toLocaleString(undefined, {
        day: "numeric",
        month: "short",
        hour: "2-digit",
        minute: "2-digit",
      }),
    );
  });

  it("shows an overdue schedule as overdue rather than as tomorrow", async () => {
    // The one case worth seeing in the evening. The backend reports the
    // occurrence an overdue schedule *owes*, which is in the past, and a panel
    // that rendered tomorrow's 22:00 instead would hide exactly the failure
    // this row exists to catch.
    const anHourAgo = new Date(Date.now() - 60 * 60 * 1000);
    backend([nightly({ nextFireAt: anHourAgo.toISOString() })]);

    render(<SchedulesSection />);

    expect(await screen.findByText(/^Next: /)).toHaveTextContent("1 hour ago");
  });

  it("renders a broken cron expression as a reason rather than hiding the row", async () => {
    // The list is where a broken schedule is *fixed*, so one unreadable row
    // must not take the whole panel down with it.
    backend([
      nightly({
        id: "broken",
        name: "Typo",
        nextFireAt: null,
        nextFireError: '"every night" is not a cron expression Rimaia can read',
      }),
      nightly({ id: "fine", name: "Works" }),
    ]);

    render(<SchedulesSection />);

    expect(await screen.findByText(/is not a cron expression/)).toBeInTheDocument();
    expect(screen.getByText("Works")).toBeInTheDocument();
  });

  it("describes a nightly schedule as prose with its timezone beside it", async () => {
    backend([nightly()]);

    render(<SchedulesSection />);

    expect(
      await screen.findByText("Every day at 22:00 · Europe/Copenhagen"),
    ).toBeInTheDocument();
  });

  it("shows an unrecognised expression verbatim rather than paraphrasing it", async () => {
    // Paraphrasing an arbitrary cron expression would be a second cron
    // implementation in TypeScript, which task 013's Notes warn against — and
    // one that disagreed with the backend would be worse than no prose.
    backend([nightly({ cron: "*/15 9-17 * * 1-5" })]);

    render(<SchedulesSection />);

    expect(
      await screen.findByText("Repeats: */15 9-17 * * 1-5 · Europe/Copenhagen"),
    ).toBeInTheDocument();
  });

  it("shows the stop time and the mode, so a window's shape is readable at a glance", async () => {
    backend([nightly({ mode: "parallel", maxConcurrency: 3 })]);

    render(<SchedulesSection />);

    expect(await screen.findByText("Stops at 06:00 · 3 at once")).toBeInTheDocument();
  });

  it("toggles a schedule off without deleting it, in one click", async () => {
    // Task 013's fifth acceptance criterion, at the control that satisfies it.
    backend([nightly()], { set_schedule_enabled: nightly({ enabled: false }) });

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByLabelText("Enable Nightly"));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_schedule_enabled", {
        id: "sched-1",
        enabled: false,
      }),
    );
    expect(mockInvoke).not.toHaveBeenCalledWith("delete_schedule", expect.anything());
  });

  it("reports what a preview found, in a status region", async () => {
    backend([nightly()], {
      preview_schedule_preflight: preflight({
        plan: [entry(), entry({ taskId: "task-2", title: "Bravo", queuePosition: null, skip: "needs_attention" })],
      }),
    });

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Preview" }));

    const answer = await screen.findByRole("status");
    expect(answer).toHaveTextContent("Nightly will start 1 of 2 ready tasks");
    expect(answer).toHaveTextContent("Bravo (the last run did not succeed)");
  });

  it("renders the preview's own error without disturbing the list", async () => {
    backend([nightly()]);
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_schedules") return [nightly()];
      if (command === "list_timezones") return ZONES;
      if (command === "preview_schedule_preflight") {
        return Promise.reject({ code: "not_found", message: "no schedule with id sched-1" });
      }
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Preview" }));

    expect(await screen.findByText(/no schedule with id/)).toBeInTheDocument();
    expect(screen.getByText("Nightly")).toBeInTheDocument();
  });

  it("disables Preview while one is in flight", async () => {
    let resolve: ((value: PreflightSummary) => void) | null = null;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_schedules") return [nightly()];
      if (command === "list_timezones") return ZONES;
      if (command === "preview_schedule_preflight") {
        return new Promise<PreflightSummary>((done) => {
          resolve = done;
        });
      }
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Preview" }));

    expect(await screen.findByRole("button", { name: "Checking…" })).toBeDisabled();
    await waitFor(() => expect(resolve).not.toBeNull());
    resolve!(preflight());
    expect(await screen.findByRole("button", { name: "Preview" })).toBeEnabled();
  });

  it("creates a schedule from the form, sending the timezone the picker chose", async () => {
    backend([], { create_schedule: nightly() });

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Add a schedule" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Nightly" } });
    fireEvent.change(screen.getByLabelText("Repeats"), { target: { value: "0 22 * * *" } });
    fireEvent.change(screen.getByLabelText("Timezone"), {
      target: { value: "Europe/Copenhagen" },
    });
    fireEvent.change(screen.getByLabelText("Stops at"), { target: { value: "06:00" } });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("create_schedule", {
        input: {
          name: "Nightly",
          mode: "sequential",
          maxConcurrency: 2,
          timezone: "Europe/Copenhagen",
          cron: "0 22 * * *",
          startAt: null,
          stopAt: "06:00",
          enabled: true,
        },
      }),
    );
  });

  it("fills the timezone picker from the backend rather than from a bundled table", async () => {
    // The property that keeps a timezone npm package out of `package.json`: the
    // list the picker offers and the list the service accepts are one
    // `chrono-tz` table.
    backend([]);

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Add a schedule" }));

    const picker = screen.getByLabelText("Timezone");
    expect(picker).toHaveTextContent("Europe/Copenhagen");
    expect(picker).toHaveTextContent("UTC");
  });

  it("sends a one-off time as an instant and no cron expression", async () => {
    backend([], { create_schedule: nightly() });

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Add a schedule" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Tonight" } });
    fireEvent.click(screen.getByLabelText("Once"));
    fireEvent.change(screen.getByLabelText("Starts at"), {
      target: { value: "2026-09-02T18:30" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    await waitFor(() => expect(mockInvoke).toHaveBeenCalledWith("create_schedule", expect.anything()));
    const sent = mockInvoke.mock.calls.find(([command]) => command === "create_schedule")?.[1] as {
      input: { cron: string | null; startAt: string | null };
    };
    expect(sent.input.cron).toBeNull();
    expect(sent.input.startAt).toBe(new Date("2026-09-02T18:30").toISOString());
  });

  it("leaves the backend alone while a cron expression is half-typed", async () => {
    // "0 2" on the way to "0 22 * * *" must not be saved — the same argument
    // `McpSection` makes for its port field.
    backend([]);

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Add a schedule" }));
    const cron = screen.getByLabelText("Repeats");
    fireEvent.change(cron, { target: { value: "0 2" } });
    fireEvent.blur(cron);

    expect(mockInvoke).not.toHaveBeenCalledWith("create_schedule", expect.anything());
  });

  it("renders the backend's refusal and keeps the form open with what was typed", async () => {
    // The refusal is the service's, and the form is where it is acted on — so
    // it must not close and throw the input away.
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_schedules") return [];
      if (command === "list_timezones") return ZONES;
      if (command === "create_schedule") {
        return Promise.reject({
          code: "invalid",
          message: '"every night" is not a cron expression Rimaia can read',
        });
      }
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<SchedulesSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Add a schedule" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Broken" } });
    fireEvent.change(screen.getByLabelText("Repeats"), { target: { value: "every night" } });
    fireEvent.click(screen.getByRole("button", { name: "Create" }));

    expect(await screen.findByText(/is not a cron expression/)).toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toHaveValue("Broken");
  });

  it("re-reads when schedules:changed fires", async () => {
    // The queue writes `last_fired_at` when a schedule fires, and that is the
    // event which tells an open panel the row moved on.
    let reads = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_schedules") {
        reads += 1;
        return [nightly(reads === 1 ? {} : { name: "Renamed elsewhere" })];
      }
      if (command === "list_timezones") return ZONES;
      throw new Error(`unexpected command: ${String(command)}`);
    });
    let announce: (() => void) | null = null;
    mockListen.mockImplementation(async (event, handler) => {
      if (event === "schedules:changed") {
        announce = () => (handler as (payload: unknown) => void)({ payload: [] });
      }
      return vi.fn();
    });

    render(<SchedulesSection />);
    await screen.findByText("Nightly");

    await waitFor(() => expect(announce).not.toBeNull());
    announce!();

    expect(await screen.findByText("Renamed elsewhere")).toBeInTheDocument();
  });

  it("says the queue only starts by hand when there are no schedules at all", async () => {
    backend([]);

    render(<SchedulesSection />);

    expect(await screen.findByText(/only when you press Start/)).toBeInTheDocument();
  });

  it("renders the error banner when list_schedules rejects", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_timezones") return ZONES;
      return Promise.reject({ code: "database", message: "the database is locked" });
    });

    render(<SchedulesSection />);

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("the database is locked");
  });
});
