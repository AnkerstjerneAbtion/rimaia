import { act, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { ReviewHistorySection } from "./ReviewHistorySection";
import { phase, twoLoopHistory } from "../../test/reviewFixtures";
import type { ReviewHistory } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts` or `lib/events.ts`.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

type Handler = (event: { payload: unknown }) => void;
let handlers: Record<string, Handler>;

function answering(history: () => ReviewHistory) {
  mockInvoke.mockImplementation(async (command, args) => {
    if (command === "get_review_history") {
      expect(args).toEqual({ taskId: "task-1" });
      return history();
    }
    throw new Error(`unexpected command: ${command}`);
  });
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  handlers = {};
  mockListen.mockImplementation(async (name, callback) => {
    handlers[name as string] = callback as Handler;
    return vi.fn();
  });
});

describe("ReviewHistorySection", () => {
  it("states what remains in the current loop from core's counts", async () => {
    answering(twoLoopHistory);
    render(<ReviewHistorySection taskId="task-1" />);

    expect(
      await screen.findByText("Reviewed after 1 fix · 2 blocking findings open, 1 advisory"),
    ).toBeInTheDocument();
  });

  it("names every phase of the newest loop as kind and number", async () => {
    answering(twoLoopHistory);
    render(<ReviewHistorySection taskId="task-1" />);
    await screen.findByText("Review · #5");

    expect(screen.getByText("Review · #7")).toBeInTheDocument();
    expect(screen.getByText("Fix · #6")).toBeInTheDocument();
  });

  it("shows what became of each finding, with a rejection's reason on the line", async () => {
    answering(twoLoopHistory);
    render(<ReviewHistorySection taskId="task-1" />);
    await screen.findByText("Review · #5");

    expect(screen.getAllByText("Fixed in #6")).toHaveLength(2);
    expect(
      screen.getByText("Rejected in #6 — The caller always passes a timeout."),
    ).toBeInTheDocument();
    // A finding 021 carried over as already rejected shows its stored
    // resolution verbatim, with no run to name.
    expect(
      screen.getByText(
        "Rejected earlier as f-default: The caller always passes a timeout.",
      ),
    ).toBeInTheDocument();
  });

  it("marks open, advisory and ping-pong findings apart by their words", async () => {
    answering(twoLoopHistory);
    render(<ReviewHistorySection taskId="task-1" />);
    await screen.findByText("Review · #7");

    expect(screen.getAllByText("Open")).toHaveLength(3);
    expect(screen.getAllByText("Advisory")).toHaveLength(1);
    const cameBack = screen.getByText("Came back after a fix").closest("li");
    expect(cameBack).not.toBeNull();
    expect(within(cameBack as HTMLElement).getByText("Unchecked index")).toBeInTheDocument();
    const brandNew = screen.getByText("New after a fix").closest("li");
    expect(within(brandNew as HTMLElement).getByText("Race on logout")).toBeInTheDocument();
    const nit = screen.getByText("Advisory").closest("li");
    expect(within(nit as HTMLElement).getByText("Rename the helper")).toBeInTheDocument();
  });

  it("shows a finding's location as copyable monospace file:line and its body on expansion", async () => {
    answering(twoLoopHistory);
    render(<ReviewHistorySection taskId="task-1" />);
    await screen.findByText("Review · #7");

    const location = screen.getAllByRole("button", { name: "Copy src/login.ts:12" })[0];
    expect(within(location).getByText("src/login.ts:12").tagName).toBe("CODE");
    const details = screen.getAllByText("Details")[0].closest("details") as HTMLDetailsElement;
    expect(details.open).toBe(false);
  });

  it("collapses an earlier loop by default", async () => {
    answering(twoLoopHistory);
    render(<ReviewHistorySection taskId="task-1" />);
    await screen.findByText("Review · #7");

    const earlier = screen.getByText("Earlier loop · Implementation · #1").closest("details");
    expect(earlier).not.toBeNull();
    expect((earlier as HTMLDetailsElement).open).toBe(false);
    expect(within(earlier as HTMLElement).getByText("Review · #2")).toBeInTheDocument();
  });

  it("renders nothing for an empty history", async () => {
    answering(() => ({ loops: [] }));
    const { container } = render(<ReviewHistorySection taskId="task-1" />);

    await waitFor(() => expect(mockInvoke).toHaveBeenCalled());
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing for a loop that was only ever implemented", async () => {
    answering(() => ({
      loops: [
        {
          earlier: false,
          implementation: phase("implementation", [1]),
          rounds: [],
          fixesSpent: 0,
          verdict: { verdict: "none" },
          openBlocking: 0,
          openAdvisory: 0,
        },
      ],
    }));
    const { container } = render(<ReviewHistorySection taskId="task-1" />);

    await waitFor(() => expect(mockInvoke).toHaveBeenCalled());
    expect(container).toBeEmptyDOMElement();
  });

  it("uses the card's words for a loop that was not reviewed", async () => {
    const history = twoLoopHistory();
    history.loops[1].verdict = { verdict: "unreviewed", reason: "review_failed" };
    answering(() => history);
    render(<ReviewHistorySection taskId="task-1" />);

    expect(
      await screen.findByText("Not reviewed — the review run failed"),
    ).toBeInTheDocument();
  });

  // The section's reason to exist is that nobody has to reopen the card: a
  // finding recorded or resolved, or a run ending, has to reach the screen.
  it.each([
    ["tasks:changed", ["task-1"]],
    ["tasks:changed", []],
    ["runs:changed", ["run-8"]],
    ["settings:changed", null],
    ["repositories:changed", ["repo-1"]],
  ])("reads the history again on %s", async (event, payload) => {
    let reads = 0;
    answering(() => {
      reads += 1;
      return reads === 1 ? { loops: [] } : twoLoopHistory();
    });
    const { container } = render(<ReviewHistorySection taskId="task-1" />);
    await waitFor(() => expect(reads).toBe(1));
    await waitFor(() => expect(handlers[event]).toBeDefined());
    expect(container).toBeEmptyDOMElement();

    act(() => handlers[event]({ payload }));

    expect(await screen.findByText("Review · #7")).toBeInTheDocument();
    expect(reads).toBe(2);
  });

  it("ignores a tasks:changed for another task", async () => {
    let reads = 0;
    answering(() => {
      reads += 1;
      return { loops: [] };
    });
    render(<ReviewHistorySection taskId="task-1" />);
    await waitFor(() => expect(handlers["tasks:changed"]).toBeDefined());

    act(() => handlers["tasks:changed"]({ payload: ["task-2"] }));

    await waitFor(() => expect(reads).toBe(1));
  });
});
