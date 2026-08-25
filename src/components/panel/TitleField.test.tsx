import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { TitleField } from "./TitleField";

// Mocked at the Tauri seam, not `lib/commands.ts` — see
// `StorageSection.test.tsx`'s comment.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe("TitleField", () => {
  it("round-trips a renamed title to update_task on blur", () => {
    render(<TitleField taskId="task-1" initialTitle="Wire the board" />);

    const input = screen.getByLabelText("Task title");
    fireEvent.change(input, { target: { value: "  Wire the panel  " } });
    fireEvent.blur(input);

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { title: "Wire the panel" },
    });
  });

  it("saves a rename that was never blurred when the panel closes underneath it", () => {
    // The third of the three fields whose unmount backstop React 19's
    // detach-refs-before-cleanup order had quietly disabled — see
    // `PlanEditor.test.tsx`.
    const { unmount } = render(<TitleField taskId="task-1" initialTitle="Wire the board" />);

    fireEvent.change(screen.getByLabelText("Task title"), {
      target: { value: "Renamed, never blurred" },
    });
    unmount();

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { title: "Renamed, never blurred" },
    });
  });

  it("never saves a blank title, on blur or on unmount", async () => {
    const { unmount } = render(<TitleField taskId="task-1" initialTitle="Wire the board" />);

    const input = screen.getByLabelText("Task title");
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.blur(input);

    expect(await screen.findByText("a task's title must not be blank")).toBeInTheDocument();
    expect(input).toHaveValue("Wire the board");

    // The restore above writes `.value` directly, which fires no `change`
    // event — so this also proves the draft the unmount save reads was
    // restored with it, rather than still holding the blank.
    unmount();
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("sends nothing on unmount when the title was already saved", async () => {
    const { unmount } = render(<TitleField taskId="task-1" initialTitle="Wire the board" />);

    const input = screen.getByLabelText("Task title");
    fireEvent.change(input, { target: { value: "Renamed" } });
    fireEvent.blur(input);
    await waitFor(() => expect(mockInvoke).toHaveBeenCalledTimes(1));

    unmount();

    expect(mockInvoke).toHaveBeenCalledTimes(1);
  });
});
