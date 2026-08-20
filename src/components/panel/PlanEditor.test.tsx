import { Profiler } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { PlanEditor } from "./PlanEditor";

// Mocked at the Tauri seam, not `lib/commands.ts` — see
// `StorageSection.test.tsx`'s comment for why: this exercises the real
// `updateTask` -> `toRimaiaError` path instead of a stub of the wrapper.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
});

/** Waits for the in-flight save to settle. Every test that triggers a
 *  commit waits on this rather than asserting on `invoke` alone: the save
 *  really is asynchronous (`commit`'s `updateTask(...).then(...)`), and
 *  waiting for its own visible completion is what keeps that resolution's
 *  state update inside `act` instead of landing after the test returns. */
async function waitForSaved() {
  await waitFor(() => expect(screen.queryByText("Saving…")).not.toBeInTheDocument());
}

describe("PlanEditor", () => {
  it("round-trips an edited plan to update_task on blur", async () => {
    mockInvoke.mockResolvedValue({ id: "task-1", plan: "new plan text" });

    render(<PlanEditor taskId="task-1" initialValue="" />);

    const textarea = screen.getByLabelText("Plan");
    fireEvent.change(textarea, { target: { value: "new plan text" } });
    fireEvent.blur(textarea);

    // `invoke` is called synchronously inside the blur handler — only its
    // resolution is async — so there is nothing to await before asserting.
    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { plan: "new plan text" },
    });
    await waitForSaved();
  });

  it("does not call update_task on blur when nothing changed", () => {
    render(<PlanEditor taskId="task-1" initialValue="unchanged" />);

    const textarea = screen.getByLabelText("Plan");
    fireEvent.blur(textarea);

    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("renders Markdown when the preview toggle is used", async () => {
    mockInvoke.mockResolvedValue({ id: "task-1" });
    render(<PlanEditor taskId="task-1" initialValue="" />);

    const textarea = screen.getByLabelText("Plan");
    fireEvent.change(textarea, { target: { value: "# Heading\n\nSome **bold** text." } });

    fireEvent.click(screen.getByRole("button", { name: "Preview" }));

    expect(screen.getByRole("heading", { level: 1, name: "Heading" })).toBeInTheDocument();
    expect(screen.getByText("bold").tagName).toBe("STRONG");
    // The raw textarea is gone while in preview mode.
    expect(screen.queryByLabelText("Plan")).not.toBeInTheDocument();
    await waitForSaved();
  });

  it("commits the plan when switching to preview, not only on blur", async () => {
    mockInvoke.mockResolvedValue({ id: "task-1" });
    render(<PlanEditor taskId="task-1" initialValue="" />);

    fireEvent.change(screen.getByLabelText("Plan"), { target: { value: "typed, not blurred" } });
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { plan: "typed, not blurred" },
    });
    await waitForSaved();
  });

  it("saves an edit that was never blurred when the panel closes underneath it", async () => {
    // `Esc` (Board's own handler) and selecting a different card both unmount
    // this editor without a preceding blur, and blur is otherwise the only
    // save trigger — so an unmount that drops the text loses a plan the user
    // typed, silently, in the product's main writing surface. React 19
    // detaches every `ref` before it runs effect cleanups, so reading the
    // draft back off `textareaRef.current` at unmount reads `null`; the draft
    // has to live in a ref of its own to survive that.
    mockInvoke.mockResolvedValue({ id: "task-1" });
    const { unmount } = render(<PlanEditor taskId="task-1" initialValue="" />);

    fireEvent.change(screen.getByLabelText("Plan"), {
      target: { value: "400 lines of plan, never blurred" },
    });
    unmount();

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { plan: "400 lines of plan, never blurred" },
    });
  });

  it("sends nothing on unmount when the text was already saved", async () => {
    mockInvoke.mockResolvedValue({ id: "task-1" });
    const { unmount } = render(<PlanEditor taskId="task-1" initialValue="" />);

    const textarea = screen.getByLabelText("Plan");
    fireEvent.change(textarea, { target: { value: "typed and blurred" } });
    fireEvent.blur(textarea);
    await waitForSaved();
    expect(mockInvoke).toHaveBeenCalledTimes(1);

    unmount();

    expect(mockInvoke).toHaveBeenCalledTimes(1);
  });

  it("does not re-render while the plan is being typed", () => {
    // The backstop above must not be bought with a controlled textarea: a
    // 400-line plan re-rendered per keystroke is the thrash task 005's Notes
    // warn about. `Profiler` commits once per render of the subtree, so an
    // unchanged call count across a burst of input is the proof — the draft
    // is written to a ref, which React does not treat as a state change.
    mockInvoke.mockResolvedValue({ id: "task-1" });
    const onRender = vi.fn();
    render(
      <Profiler id="plan-editor" onRender={onRender}>
        <PlanEditor taskId="task-1" initialValue="" />
      </Profiler>,
    );

    const textarea = screen.getByLabelText("Plan");
    onRender.mockClear();
    for (let line = 0; line < 50; line += 1) {
      fireEvent.change(textarea, { target: { value: `line ${line}\n`.repeat(line + 1) } });
    }

    expect(onRender).not.toHaveBeenCalled();
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("switches back to the textarea, pre-filled with the last edited text", async () => {
    mockInvoke.mockResolvedValue({ id: "task-1" });
    render(<PlanEditor taskId="task-1" initialValue="" />);

    fireEvent.change(screen.getByLabelText("Plan"), { target: { value: "round trip me" } });
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    await waitForSaved();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    expect(screen.getByLabelText("Plan")).toHaveValue("round trip me");
  });
});
