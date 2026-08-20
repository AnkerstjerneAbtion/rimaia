import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { ModelEffortOverrides } from "./ModelEffortOverrides";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

/** Waits for the in-flight save to settle — see `PlanEditor.test.tsx`'s
 *  copy of this helper for why. */
async function waitForSaved() {
  await waitFor(() => expect(screen.queryByText("Saving…")).not.toBeInTheDocument());
}

describe("ModelEffortOverrides", () => {
  it("sends nothing when left on the default", () => {
    render(<ModelEffortOverrides taskId="task-1" model={null} effort={null} />);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("sends the selected model, and null when switched back to default", async () => {
    render(<ModelEffortOverrides taskId="task-1" model={null} effort={null} />);

    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "opus" } });
    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { model: "opus" },
    });
    await waitForSaved();

    mockInvoke.mockClear();
    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "" } });
    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { model: null },
    });
    await waitForSaved();
  });

  it("sends the selected effort", async () => {
    render(<ModelEffortOverrides taskId="task-1" model={null} effort={null} />);

    fireEvent.change(screen.getByLabelText("Effort"), { target: { value: "high" } });

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { effort: "high" },
    });
    await waitForSaved();
  });

  it("does not resend the value it was already showing", () => {
    render(<ModelEffortOverrides taskId="task-1" model="sonnet" effort={null} />);

    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "sonnet" } });

    expect(mockInvoke).not.toHaveBeenCalled();
  });
});
