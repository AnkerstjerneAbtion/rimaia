import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { ConcurrencySection } from "./ConcurrencySection";
import type { RunCapacity } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts` or `lib/events.ts` — see
// `StorageSection.test.tsx`'s comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function capacity(overrides: Partial<RunCapacity> = {}): RunCapacity {
  return { mode: "sequential", maxConcurrency: 2, ceiling: 8, ...overrides };
}

/** Every command this panel can send, answered; anything else is a failure
 *  rather than an `undefined` that silently renders as empty. */
function mockBackend(options: {
  readonly initial?: RunCapacity;
  readonly onSetMode?: (mode: string) => RunCapacity | Promise<never>;
  readonly onSetLimit?: (value: number) => RunCapacity | Promise<never>;
}) {
  const initial = options.initial ?? capacity();
  mockInvoke.mockImplementation(async (command, args) => {
    if (command === "get_run_capacity") return initial;
    if (command === "set_schedule_mode") {
      const mode = (args as { mode: string }).mode;
      return options.onSetMode ? options.onSetMode(mode) : { ...initial, mode };
    }
    if (command === "set_max_concurrency") {
      const value = (args as { value: number }).value;
      return options.onSetLimit
        ? options.onSetLimit(value)
        : { ...initial, maxConcurrency: value };
    }
    throw new Error(`unexpected command: ${String(command)}`);
  });
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("ConcurrencySection", () => {
  it("shows the stored mode and limit once get_run_capacity resolves", async () => {
    mockBackend({ initial: capacity({ mode: "parallel", maxConcurrency: 3 }) });

    render(<ConcurrencySection />);

    expect(await screen.findByRole("radio", { name: "Several at once" })).toBeChecked();
    expect(screen.getByLabelText("Runs at once")).toHaveValue(3);
  });

  it("writes the mode when the other option is chosen", async () => {
    mockBackend({});

    render(<ConcurrencySection />);
    fireEvent.click(await screen.findByRole("radio", { name: "Several at once" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_schedule_mode", { mode: "parallel" }),
    );
    expect(await screen.findByRole("radio", { name: "Several at once" })).toBeChecked();
  });

  it("puts the mode back and shows the message when the write is rejected", async () => {
    mockBackend({
      onSetMode: () => Promise.reject({ code: "internal", message: "the database is locked" }),
    });

    render(<ConcurrencySection />);
    fireEvent.click(await screen.findByRole("radio", { name: "Several at once" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("the database is locked");
    expect(screen.getByRole("radio", { name: "One at a time" })).toBeChecked();
  });

  it("keeps the stored limit visible in sequential mode, and says it is not in force", async () => {
    // The whole reason the panel shows the *stored* number rather than what
    // `resolve` would return: sequential runs exactly one, and a control that
    // rendered `1` here would make the setting look forgotten every time the
    // mode was switched.
    mockBackend({ initial: capacity({ mode: "sequential", maxConcurrency: 4 }) });

    render(<ConcurrencySection />);

    expect(await screen.findByLabelText("Runs at once")).toHaveValue(4);
    expect(screen.getByText(/Not in force while runs happen one at a time/)).toBeInTheDocument();
  });

  it("does not send a limit above the ceiling the backend reported", async () => {
    // Belt and braces on top of the service's own refusal: the form must not be
    // able to produce the request in the first place, and the bound comes off
    // the wire rather than being written into this file.
    mockBackend({ initial: capacity({ mode: "parallel", ceiling: 8 }) });

    render(<ConcurrencySection />);
    const input = await screen.findByLabelText("Runs at once");
    fireEvent.change(input, { target: { value: "40" } });

    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(mockInvoke).not.toHaveBeenCalledWith("set_max_concurrency", expect.anything());
  });

  it("saves the limit on submit and takes the answer as the new state", async () => {
    mockBackend({ initial: capacity({ mode: "parallel", maxConcurrency: 2 }) });

    render(<ConcurrencySection />);
    fireEvent.change(await screen.findByLabelText("Runs at once"), { target: { value: "5" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_max_concurrency", { value: 5 }),
    );
    // Disabled again, because the field now matches what was stored.
    await waitFor(() => expect(screen.getByRole("button", { name: "Save" })).toBeDisabled());
  });

  it("renders the service's refusal rather than swallowing it", async () => {
    mockBackend({
      initial: capacity({ mode: "parallel" }),
      onSetLimit: () =>
        Promise.reject({
          code: "invalid",
          message: "Rimaia will supervise between 1 and 8 runs at once, not 40.",
        }),
    });

    render(<ConcurrencySection />);
    fireEvent.change(await screen.findByLabelText("Runs at once"), { target: { value: "5" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "between 1 and 8 runs at once",
    );
  });

  it("re-reads when settings:changed fires, since both keys live in settings", async () => {
    mockBackend({});

    render(<ConcurrencySection />);
    await screen.findByLabelText("Runs at once");
    const before = mockInvoke.mock.calls.filter(([c]) => c === "get_run_capacity").length;

    const handler = mockListen.mock.calls.find(([event]) => event === "settings:changed")?.[1];
    expect(handler).toBeDefined();
    handler?.({ event: "settings:changed", id: 1, payload: undefined });

    await waitFor(() =>
      expect(
        mockInvoke.mock.calls.filter(([c]) => c === "get_run_capacity").length,
      ).toBeGreaterThan(before),
    );
  });
});
