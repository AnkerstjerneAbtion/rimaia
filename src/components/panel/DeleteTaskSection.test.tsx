import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { DeleteTaskSection } from "./DeleteTaskSection";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
});

describe("DeleteTaskSection", () => {
  it("does not call delete_task on the first click — it asks for confirmation", () => {
    render(<DeleteTaskSection taskId="task-1" title="Ship the thing" onDeleted={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Delete task" }));

    expect(mockInvoke).not.toHaveBeenCalled();
    expect(
      screen.getByRole("alertdialog", { name: 'Confirm delete "Ship the thing"' }),
    ).toBeInTheDocument();
  });

  it("calls delete_task and onDeleted only after the confirmation is accepted", async () => {
    mockInvoke.mockResolvedValue(undefined);
    const onDeleted = vi.fn();
    render(<DeleteTaskSection taskId="task-1" title="Ship the thing" onDeleted={onDeleted} />);

    fireEvent.click(screen.getByRole("button", { name: "Delete task" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete task" }));

    expect(mockInvoke).toHaveBeenCalledWith("delete_task", { id: "task-1" });
    await vi.waitFor(() => expect(onDeleted).toHaveBeenCalled());
  });

  it("cancelling the confirmation calls neither delete_task nor onDeleted", () => {
    const onDeleted = vi.fn();
    render(<DeleteTaskSection taskId="task-1" title="Ship the thing" onDeleted={onDeleted} />);

    fireEvent.click(screen.getByRole("button", { name: "Delete task" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(mockInvoke).not.toHaveBeenCalled();
    expect(onDeleted).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("shows the error and stays open when delete_task rejects", async () => {
    mockInvoke.mockRejectedValue({ code: "internal", message: "could not delete" });
    render(<DeleteTaskSection taskId="task-1" title="Ship the thing" onDeleted={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Delete task" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete task" }));

    expect(await screen.findByText("could not delete")).toBeInTheDocument();
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
  });
});
