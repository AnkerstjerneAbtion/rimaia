import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { QueueControls } from "./QueueControls";

// Mocked at the Tauri seam, not `lib/commands.ts` — see
// `StorageSection.test.tsx`'s own comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe("QueueControls", () => {
  it("shows Start queue, and only that, for a queue never observed running", () => {
    render(<QueueControls state="paused" hasRunBefore={false} hasRunInFlight={false} />);

    expect(screen.getByRole("button", { name: "Start queue" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Resume queue" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Pause" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
  });

  it("shows Resume queue rather than Start once the queue has run before", () => {
    render(<QueueControls state="paused" hasRunBefore={true} hasRunInFlight={false} />);

    expect(screen.getByRole("button", { name: "Resume queue" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Start queue" })).toBeNull();
  });

  it("shows Pause and Stop while running", () => {
    render(<QueueControls state="running" hasRunBefore={true} hasRunInFlight={true} />);

    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Start queue" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Resume queue" })).toBeNull();
  });

  it("keeps Stop available after a pause whose current run is still finishing", () => {
    // The scenario the task's own wording exists to make legible: Pause
    // flips the switch to `paused` immediately, but the run it caught mid-way
    // keeps going until it ends on its own.
    render(<QueueControls state="paused" hasRunBefore={true} hasRunInFlight={true} />);

    expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pause" })).toBeNull();
  });

  it("hides Stop once the queue is paused with nothing left in flight", () => {
    render(<QueueControls state="paused" hasRunBefore={true} hasRunInFlight={false} />);

    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
  });

  it("always shows the Pause-vs-Stop distinction, regardless of state", () => {
    const { rerender } = render(
      <QueueControls state="paused" hasRunBefore={false} hasRunInFlight={false} />,
    );
    expect(
      screen.getByText(/Pause lets the current run finish and starts nothing new/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Stop also cancels the run in flight/)).toBeInTheDocument();

    rerender(<QueueControls state="running" hasRunBefore={true} hasRunInFlight={true} />);
    expect(
      screen.getByText(/Pause lets the current run finish and starts nothing new/),
    ).toBeInTheDocument();
  });

  it("calls start_queue when Start queue is clicked", async () => {
    render(<QueueControls state="paused" hasRunBefore={false} hasRunInFlight={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Start queue" }));

    expect(mockInvoke).toHaveBeenCalledWith("start_queue", undefined);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Start queue" })).toBeEnabled(),
    );
  });

  it("calls resume_queue, not start_queue, when Resume queue is clicked", async () => {
    render(<QueueControls state="paused" hasRunBefore={true} hasRunInFlight={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Resume queue" }));

    expect(mockInvoke).toHaveBeenCalledWith("resume_queue", undefined);
    expect(mockInvoke).not.toHaveBeenCalledWith("start_queue", undefined);
    // Lets the click's own `setPending(null)` land inside `act()` rather than
    // after this test has already returned.
    await waitFor(() => expect(screen.getByRole("button", { name: "Resume queue" })).toBeEnabled());
  });

  it("calls pause_queue when Pause is clicked", async () => {
    render(<QueueControls state="running" hasRunBefore={true} hasRunInFlight={true} />);

    fireEvent.click(screen.getByRole("button", { name: "Pause" }));

    expect(mockInvoke).toHaveBeenCalledWith("pause_queue", undefined);
    await waitFor(() => expect(screen.getByRole("button", { name: "Pause" })).toBeEnabled());
  });

  it("calls stop_queue when Stop is clicked", async () => {
    render(<QueueControls state="running" hasRunBefore={true} hasRunInFlight={true} />);

    fireEvent.click(screen.getByRole("button", { name: "Stop" }));

    expect(mockInvoke).toHaveBeenCalledWith("stop_queue", undefined);
    await waitFor(() => expect(screen.getByRole("button", { name: "Stop" })).toBeEnabled());
  });

  it("disables the button and shows a pending label while the request is in flight", async () => {
    let resolveInvoke: (() => void) | undefined;
    mockInvoke.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveInvoke = () => resolve(undefined);
        }),
    );
    render(<QueueControls state="paused" hasRunBefore={false} hasRunInFlight={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Start queue" }));

    expect(await screen.findByRole("button", { name: "Starting…" })).toBeDisabled();

    resolveInvoke?.();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Start queue" })).toBeEnabled(),
    );
  });

  it("shows the backend's own rejection message when the action fails", async () => {
    mockInvoke.mockRejectedValue({ code: "internal", message: "the queue could not be started" });
    render(<QueueControls state="paused" hasRunBefore={false} hasRunInFlight={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Start queue" }));

    expect(await screen.findByText("the queue could not be started")).toBeInTheDocument();
  });
});
