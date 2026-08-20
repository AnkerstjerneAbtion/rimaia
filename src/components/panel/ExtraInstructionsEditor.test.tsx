import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { ExtraInstructionsEditor } from "./ExtraInstructionsEditor";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe("ExtraInstructionsEditor", () => {
  it("round-trips an edit to update_task on blur", async () => {
    render(<ExtraInstructionsEditor taskId="task-1" initialValue="" />);

    const textarea = screen.getByLabelText("Extra instructions");
    fireEvent.change(textarea, { target: { value: "Never touch the migrations." } });
    fireEvent.blur(textarea);

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { extraInstructions: "Never touch the migrations." },
    });
    // Waits for the save to settle so its state update lands inside `act`
    // rather than after the test returns — see `PlanEditor.test.tsx`.
    await waitFor(() => expect(screen.queryByText("Saving…")).not.toBeInTheDocument());
  });

  it("does not call update_task when nothing changed", () => {
    render(<ExtraInstructionsEditor taskId="task-1" initialValue="already saved" />);

    fireEvent.blur(screen.getByLabelText("Extra instructions"));

    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("normalizes a cleared field to null rather than an empty string", async () => {
    // The schema has one column (`plan`) where this is enforced by comment
    // and convention, not a CHECK constraint - `extra_instructions` is the
    // other, and a blank commit here would otherwise be a second way to
    // spell "none" that task 006's prompt composer would have to know about.
    render(<ExtraInstructionsEditor taskId="task-1" initialValue="Old note" />);

    const textarea = screen.getByLabelText("Extra instructions");
    fireEvent.change(textarea, { target: { value: "   " } });
    fireEvent.blur(textarea);

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { extraInstructions: null },
    });
    await waitFor(() => expect(screen.queryByText("Saving…")).not.toBeInTheDocument());
  });

  it("saves an edit that was never blurred when the panel closes underneath it", () => {
    // Same silent data loss `PlanEditor` had, for the same reason: React 19
    // nulls every `ref` before effect cleanups run, so the unmount backstop
    // read `null` and saved nothing. See `PlanEditor.test.tsx`.
    const { unmount } = render(<ExtraInstructionsEditor taskId="task-1" initialValue="" />);

    fireEvent.change(screen.getByLabelText("Extra instructions"), {
      target: { value: "Never touch the migrations." },
    });
    unmount();

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { extraInstructions: "Never touch the migrations." },
    });
  });

  it("normalizes a cleared field to null on the unmount save too", () => {
    const { unmount } = render(
      <ExtraInstructionsEditor taskId="task-1" initialValue="Old note" />,
    );

    fireEvent.change(screen.getByLabelText("Extra instructions"), { target: { value: "  " } });
    unmount();

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { extraInstructions: null },
    });
  });

  it("states that the text is appended after the plan", () => {
    render(<ExtraInstructionsEditor taskId="task-1" initialValue="" />);
    expect(screen.getByText(/appended after the plan/i)).toBeInTheDocument();
  });
});
