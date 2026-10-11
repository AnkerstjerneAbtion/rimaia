import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { ClientCapabilitiesProvider } from "../lib/capabilities";
import { installTransports } from "../lib/client";
import {
  browserLocalTransport,
  httpCommandTransport,
  setCommandTransports,
  setCurrentTeam,
} from "../lib/commands";
import { setEventTransports, sseEventTransport } from "../lib/events";
import { setUrlOpener } from "../lib/open";
import { BROWSER } from "../test/capabilities";
import { mockHttp } from "../test/http";
import type { HttpMock } from "../test/http";
import { emptyDigest, repository, runDetail, taskDetail, taskSummary } from "../test/reviewFixtures";
import { PERSONAL, account, apiToken, runner, session } from "../test/web";
import { AccountView } from "./AccountView";
import { AnalyticsView } from "./AnalyticsView";
import { BoardView } from "./BoardView";
import { ReviewView } from "./ReviewView";
import { RunsView } from "./RunsView";
import { SettingsView } from "./SettingsView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const PR = "https://github.com/example/app/pull/task-1";
const STUDIO = { id: "runner-studio", label: "Studio Mac", unpaired: false };

let http: HttpMock;
/** Every command that reached the browser's local transport. */
let local: string[];
let open: ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  http = mockHttp();
  local = [];
  open = vi.fn();
  vi.stubGlobal("open", open);
  // The browser's own transports, with a spy on what it would refuse.
  const server = {
    baseUrl: window.location.origin,
    credential: { kind: "session", csrf: () => null } as const,
  };
  setCommandTransports({
    board: httpCommandTransport(server),
    local: async (command, args) => {
      local.push(command);
      return browserLocalTransport(command, args);
    },
  });
  setEventTransports({ board: sseEventTransport(server), local: null });
  setUrlOpener(async (url) => {
    globalThis.open(url, "_blank", "noopener,noreferrer");
  });
  setCurrentTeam(PERSONAL.id);

  const summary = taskSummary({ id: "task-1", title: "Add login", column: "in_review" });
  http.answer("list_tasks", (args: unknown) =>
    (args as { filter?: { runState?: string } }).filter?.runState === "running" ? [] : [summary],
  );
  http.answer("list_repositories", [repository()]);
  http.answer("get_task", taskDetail("task-1", { worktreeRunner: STUDIO }));
  http.answer("get_run", runDetail("task-1", { review: { source: "not_recorded" }, headSha: null }));
  http.answer("list_runs", [
    {
      ...runDetail("task-1"),
      taskTitle: "Add login",
      repositoryId: "repo-a",
      repositoryName: "rimaia-app",
    },
  ]);
  http.answer("get_review_digest", emptyDigest());
  http.answer("get_review_history", { loops: [] });
  http.answer("get_task_dependents", []);
  http.answer("list_review_findings", []);
  http.answer("list_runs_for_task", [runDetail("task-1")]);
  http.answer("get_account", account());
  http.answer("list_sessions", [session()]);
  http.answer("list_api_tokens", [apiToken()]);
  http.answer("list_runners", [runner()]);
});

afterEach(() => {
  cleanup();
  installTransports("solo");
  setCurrentTeam(null);
  vi.unstubAllGlobals();
});

function inBrowser(view: React.ReactNode) {
  return render(<ClientCapabilitiesProvider capabilities={BROWSER}>{view}</ClientCapabilitiesProvider>);
}

describe("a browser", () => {
  it("sends no local command from any view in browser mode", async () => {
    const user = userEvent.setup();

    // Board, with a task's detail open.
    const board = inBrowser(<BoardView />);
    fireEvent.click(await screen.findByRole("button", { name: /Add login/ }));
    await waitFor(() => expect(document.querySelector(".task-detail-panel")).not.toBeNull());
    expect(await screen.findByText("Worktree on Studio Mac")).toBeInTheDocument();
    board.unmount();

    // Runs, with a not_recorded run's detail open.
    const runs = inBrowser(<RunsView />);
    fireEvent.click(await screen.findByRole("button", { name: /Add login/ }));
    expect(await screen.findByText("No diff was recorded for this run.")).toBeInTheDocument();
    runs.unmount();

    // Review, on a queued task whose worktree is on another machine, with w and
    // o pressed: the first does nothing and the second opens the pull request.
    const review = inBrowser(<ReviewView now={() => new Date("2026-10-04T09:30:00Z")} />);
    await screen.findByRole("region", { name: "Overnight digest" });
    await user.keyboard("{Enter}");
    expect(await screen.findByText("Worktree on Studio Mac")).toBeInTheDocument();
    await user.keyboard("w");
    await user.keyboard("o");
    await waitFor(() => expect(open).toHaveBeenCalledTimes(1));
    review.unmount();

    inBrowser(<AnalyticsView />).unmount();
    inBrowser(<SettingsView />).unmount();
    const accountPage = inBrowser(<AccountView />);
    await screen.findByText("ada-lovelace");
    await waitFor(() => expect(http.requests.map((request) => request.name)).toContain("list_runners"));
    accountPage.unmount();

    // Nothing but the question only a client can answer reached the local
    // transport, and in a browser that is answered without a request.
    expect(local.filter((command) => command !== "get_client_capabilities")).toEqual([]);
    // `openUrl` calls `invoke` directly, outside the transport: this catches it.
    expect(vi.mocked(invoke)).not.toHaveBeenCalled();
    expect(open).toHaveBeenCalledTimes(1);
    expect(open).toHaveBeenCalledWith(PR, "_blank", "noopener,noreferrer");
    expect(document.body.textContent).not.toContain("only available in the desktop app");
  });
});
