import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { defaultLabelFromUrl, LinksEditor } from "./LinksEditor";
import type { TaskLink } from "../../types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

function link(id: string, label: string, url: string, position: number): TaskLink {
  return { id, taskId: "task-1", label, url, position };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe("defaultLabelFromUrl", () => {
  it("returns the URL's host", () => {
    expect(defaultLabelFromUrl("https://github.com/foo/bar/issues/3")).toBe("github.com");
  });

  it("falls back to the raw string for a URL it cannot parse", () => {
    expect(defaultLabelFromUrl("not a url")).toBe("not a url");
  });
});

// Every mutating action below asserts on `onChanged` having fired, not just
// on the `invoke` call — `onChanged` only runs after the mocked command's
// promise resolves, so waiting for it is both the real behaviour worth
// proving (a successful mutation refetches) and what lets that resolution's
// state update land inside `@testing-library/react`'s `act` instead of
// after the test function returns.

describe("LinksEditor", () => {
  it("adds a link with the typed label", async () => {
    const onChanged = vi.fn();
    render(<LinksEditor taskId="task-1" links={[]} loading={false} onChanged={onChanged} />);

    fireEvent.change(screen.getByLabelText("New link label"), { target: { value: "Asana" } });
    fireEvent.change(screen.getByLabelText("New link URL"), {
      target: { value: "https://app.asana.com/0/1/2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));

    expect(mockInvoke).toHaveBeenCalledWith("add_task_link", {
      taskId: "task-1",
      input: { label: "Asana", url: "https://app.asana.com/0/1/2" },
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("defaults the label to the URL's host when none is typed", async () => {
    const onChanged = vi.fn();
    render(<LinksEditor taskId="task-1" links={[]} loading={false} onChanged={onChanged} />);

    fireEvent.change(screen.getByLabelText("New link URL"), {
      target: { value: "https://github.com/org/repo" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add link" }));

    expect(mockInvoke).toHaveBeenCalledWith("add_task_link", {
      taskId: "task-1",
      input: { label: "github.com", url: "https://github.com/org/repo" },
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("removes a link", async () => {
    const onChanged = vi.fn();
    const links = [link("l1", "Docs", "https://example.com", 1)];
    render(<LinksEditor taskId="task-1" links={links} loading={false} onChanged={onChanged} />);

    fireEvent.click(screen.getByRole("button", { name: "Remove" }));

    expect(mockInvoke).toHaveBeenCalledWith("remove_task_link", { linkId: "l1" });
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("moving the second of three links up names the first and third as neighbours", async () => {
    const onChanged = vi.fn();
    const links = [
      link("l1", "One", "https://one.example", 1),
      link("l2", "Two", "https://two.example", 2),
      link("l3", "Three", "https://three.example", 3),
    ];
    render(<LinksEditor taskId="task-1" links={links} loading={false} onChanged={onChanged} />);

    fireEvent.click(screen.getByRole("button", { name: 'Move "Two" up' }));

    // Moving index 1 up swaps it with index 0: the new neighbours are
    // "nothing above" (index - 2 is out of range) and the link it swapped
    // with.
    expect(mockInvoke).toHaveBeenCalledWith("reorder_task_link", {
      linkId: "l2",
      beforeId: null,
      afterId: "l1",
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("moving the second of three links down names the third and nothing as neighbours", async () => {
    const onChanged = vi.fn();
    const links = [
      link("l1", "One", "https://one.example", 1),
      link("l2", "Two", "https://two.example", 2),
      link("l3", "Three", "https://three.example", 3),
    ];
    render(<LinksEditor taskId="task-1" links={links} loading={false} onChanged={onChanged} />);

    fireEvent.click(screen.getByRole("button", { name: 'Move "Two" down' }));

    expect(mockInvoke).toHaveBeenCalledWith("reorder_task_link", {
      linkId: "l2",
      beforeId: "l3",
      afterId: null,
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("disables moving the first link up and the last link down", () => {
    const links = [
      link("l1", "One", "https://one.example", 1),
      link("l2", "Two", "https://two.example", 2),
    ];
    render(<LinksEditor taskId="task-1" links={links} loading={false} onChanged={vi.fn()} />);

    expect(screen.getByRole("button", { name: 'Move "One" up' })).toBeDisabled();
    expect(screen.getByRole("button", { name: 'Move "Two" down' })).toBeDisabled();
  });

  it("edits a link's label and URL", async () => {
    const onChanged = vi.fn();
    const links = [link("l1", "Old label", "https://old.example", 1)];
    render(<LinksEditor taskId="task-1" links={links} loading={false} onChanged={onChanged} />);

    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByLabelText("Link label"), { target: { value: "New label" } });
    fireEvent.change(screen.getByLabelText("Link URL"), {
      target: { value: "https://new.example" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(mockInvoke).toHaveBeenCalledWith("update_task_link", {
      linkId: "l1",
      patch: { label: "New label", url: "https://new.example" },
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("shows a loading message rather than the empty-state copy while detail is unresolved", () => {
    render(<LinksEditor taskId="task-1" links={[]} loading onChanged={vi.fn()} />);
    expect(screen.getByText("Loading…")).toBeInTheDocument();
    expect(screen.queryByText("No links yet.")).not.toBeInTheDocument();
  });
});
