import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import App from "./App";
import { ClientCapabilitiesProvider } from "./lib/capabilities";
import { installTransports, UPGRADE_RELOAD_KEY } from "./lib/client";
import { PROTOCOL_VERSION, giveUpOnTask, setCurrentTeam } from "./lib/commands";
import { TEAM_STORAGE_KEY } from "./lib/team";
import { BROWSER, connected } from "./test/capabilities";
import { mockHttp } from "./test/http";
import type { HttpMock } from "./test/http";
import { emptyDigest, taskSummary } from "./test/reviewFixtures";
import { ACME, PERSONAL, signedIn } from "./test/web";
import type { ClientCapabilities } from "./types";

// Mocked at the Tauri seam (`StorageSection.test.tsx` says why). The browser's
// board goes over the HTTP mock; `invoke` must hear nothing at all.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const mockInvoke = vi.mocked(invoke);
const UNAUTHENTICATED = { code: "unauthenticated", message: "Sign in to continue." };

let http: HttpMock;

function renderApp(capabilities: ClientCapabilities = BROWSER) {
  // The transports are the browser's whatever the capabilities say: a connected
  // desktop's HTTP half is the same code, and its sign-in is task 059's.
  installTransports("browser");
  return render(
    <ClientCapabilitiesProvider capabilities={capabilities}>
      <App />
    </ClientCapabilitiesProvider>,
  );
}

function last<T>(items: readonly T[]): T | undefined {
  return items[items.length - 1];
}

/** The board reads, with the header each carried. */
function boardReads() {
  return http.requests.filter((request) => request.name === "list_tasks");
}

beforeEach(() => {
  mockInvoke.mockReset();
  vi.mocked(listen).mockReset();
  http = mockHttp();
  window.localStorage.clear();
  window.sessionStorage.clear();
});

afterEach(() => {
  // Unmount first: a view that is still mounted when the transports change
  // reads its next answer from the wrong one.
  cleanup();
  installTransports("solo");
  setCurrentTeam(null);
  vi.unstubAllGlobals();
  window.history.replaceState({}, "", "/");
});

describe("signing in", () => {
  it("shows sign-in when there is no session", async () => {
    http.refuse("get_account", 401, UNAUTHENTICATED);

    renderApp();

    const link = await screen.findByRole("link", { name: "Sign in with GitHub" });
    expect(link.getAttribute("href")).toBe("/auth/github");
    expect(screen.getByRole("heading", { name: "Rimaia" })).toBeInTheDocument();
    // Nothing else was asked of a server that said who is not welcome.
    expect(http.requests.map((request) => request.name)).toEqual(["get_account"]);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("does not render the browser's link under connected capabilities", async () => {
    http.refuse("get_account", 401, UNAUTHENTICATED);

    renderApp(connected("runner-studio"));

    await screen.findByRole("heading", { name: "Rimaia" });
    expect(screen.queryByRole("link", { name: "Sign in with GitHub" })).not.toBeInTheDocument();
  });

  it("returns to sign-in when any later command is unauthenticated", async () => {
    signedIn(http);
    renderApp();
    await screen.findByRole("navigation", { name: "Main" });
    await waitFor(() => expect(boardReads().length).toBeGreaterThan(0));

    http.refuse("give_up_on_task", 401, UNAUTHENTICATED);
    await act(async () => {
      await giveUpOnTask("task-1").catch(() => undefined);
    });

    expect(await screen.findByRole("link", { name: "Sign in with GitHub" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Main" })).not.toBeInTheDocument();
  });

  it("returns to sign-in when the event stream is refused unauthenticated", async () => {
    signedIn(http);
    renderApp();
    await screen.findByRole("navigation", { name: "Main" });
    await waitFor(() => expect(http.events().open).toBe(true));

    // The library reconnects after its first backoff, on a real timer.
    http.events().failOpen(401, UNAUTHENTICATED);
    await act(async () => http.events().end());

    expect(
      await screen.findByRole("link", { name: "Sign in with GitHub" }, { timeout: 4000 }),
    ).toBeInTheDocument();
  }, 10_000);

  it("explains a failed sign-in, and removes the parameter", async () => {
    const sentences: Array<[string, string]> = [
      ["denied", "GitHub did not grant access, so you are not signed in."],
      ["expired", "That sign-in took too long and has expired. Sign in again."],
      ["failed", "Signing in did not work. Try again in a minute."],
    ];
    for (const [reason, sentence] of sentences) {
      http.refuse("get_account", 401, UNAUTHENTICATED);
      window.history.replaceState({}, "", `/?sign_in=${reason}`);
      const replace = vi.spyOn(window.history, "replaceState");

      const { unmount } = renderApp();

      expect(await screen.findByText(sentence)).toBeInTheDocument();
      expect(replace).toHaveBeenCalled();
      expect(window.location.search).toBe("");
      replace.mockRestore();
      unmount();
    }
  });

  it("renders the plain screen for a reason it does not know, and never echoes it", async () => {
    http.refuse("get_account", 401, UNAUTHENTICATED);
    window.history.replaceState({}, "", "/?sign_in=%3Cb%3Ex%3C%2Fb%3E");

    const { container } = renderApp();

    await screen.findByRole("link", { name: "Sign in with GitHub" });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(container.querySelector("b")).toBeNull();
    expect(document.body.textContent).not.toContain("<b>");
    expect(window.location.search).toBe("");
  });
});

describe("a page older than its server", () => {
  const SKEW = { code: "upgrade_required", message: "This client is too old." };
  let reload: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    reload = vi.fn();
    vi.stubGlobal("location", { ...window.location, origin: window.location.origin, reload });
  });

  it("reloads once per bundle on upgrade_required", async () => {
    http.refuse("give_up_on_task", 426, SKEW);
    installTransports("browser");

    await giveUpOnTask("task-1").catch(() => undefined);

    expect(window.sessionStorage.getItem(UPGRADE_RELOAD_KEY)).toBe(PROTOCOL_VERSION);
    expect(reload).toHaveBeenCalledTimes(1);

    // As the reloaded page, with the same literal: reads succeed, and a write
    // that is still refused does not reload a second time.
    http.answer("list_tasks", []);
    await expect(
      import("./lib/commands").then(({ listTasks }) => listTasks({})),
    ).resolves.toEqual([]);
    await giveUpOnTask("task-1").catch(() => undefined);
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("says so in one banner when reloading did not fix it", async () => {
    window.sessionStorage.setItem(UPGRADE_RELOAD_KEY, PROTOCOL_VERSION);
    signedIn(http);
    http.refuse("give_up_on_task", 426, SKEW);
    renderApp();
    await screen.findByRole("navigation", { name: "Main" });

    await act(async () => {
      await giveUpOnTask("task-1").catch(() => undefined);
    });

    expect(
      await screen.findByText(
        "This page is older than the server, and reloading did not fix it. Try again in a minute.",
      ),
    ).toBeInTheDocument();
    expect(reload).not.toHaveBeenCalled();
  });

  it("reloads when a different bundle's value is stored", async () => {
    window.sessionStorage.setItem(UPGRADE_RELOAD_KEY, "0.9");
    http.refuse("give_up_on_task", 426, SKEW);
    installTransports("browser");

    await giveUpOnTask("task-1").catch(() => undefined);

    expect(reload).toHaveBeenCalledTimes(1);
    expect(window.sessionStorage.getItem(UPGRADE_RELOAD_KEY)).toBe(PROTOCOL_VERSION);
  });

  it("never reloads on upgrade_required in a desktop mode", async () => {
    http.refuse("give_up_on_task", 426, SKEW);
    installTransports("connected", { serverUrl: "https://rimaia.example", token: "rmd_token" });

    await giveUpOnTask("task-1").catch(() => undefined);

    expect(reload).not.toHaveBeenCalled();
    expect(window.sessionStorage.getItem(UPGRADE_RELOAD_KEY)).toBeNull();
  });
});

describe("the team switcher", () => {
  it("sends the chosen team on every board request and on the events stream", async () => {
    signedIn(http);
    renderApp();
    await waitFor(() => expect(boardReads().length).toBeGreaterThan(0));
    await waitFor(() => expect(http.events().opens.length).toBeGreaterThan(0));
    expect(last(boardReads())?.headers["rimaia-team"]).toBe(PERSONAL.id);
    expect(last(http.events().opens)?.headers["rimaia-team"]).toBe(PERSONAL.id);

    fireEvent.click(screen.getByRole("button", { name: /^Team/ }));
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /Acme platform/ }));

    await waitFor(() => expect(last(boardReads())?.headers["rimaia-team"]).toBe(ACME.id));
    await waitFor(() => expect(last(http.events().opens)?.headers["rimaia-team"]).toBe(ACME.id));
    expect(window.localStorage.getItem(TEAM_STORAGE_KEY)).toBe(ACME.id);
  });

  it("sends no board read before a team is chosen", async () => {
    signedIn(http);
    renderApp();
    await waitFor(() => expect(boardReads().length).toBeGreaterThan(0));

    const names = http.requests.map((request) => request.name);
    expect(names.slice(0, 2)).toEqual(["get_account", "list_teams"]);
    expect(http.requests[0].headers["rimaia-team"]).toBeUndefined();
    expect(http.requests[1].headers["rimaia-team"]).toBeUndefined();
    // Everything after the choice names the team.
    for (const request of http.requests.slice(2)) {
      expect(request.headers["rimaia-team"], request.name).toBe(PERSONAL.id);
    }
  });

  it("falls back to the personal team when the chosen team is gone, at startup", async () => {
    window.localStorage.setItem(TEAM_STORAGE_KEY, "team-that-was-deleted");
    signedIn(http, [ACME, PERSONAL]);

    renderApp();

    await waitFor(() => expect(boardReads().length).toBeGreaterThan(0));
    expect(boardReads()[0].headers["rimaia-team"]).toBe(PERSONAL.id);
    expect(window.localStorage.getItem(TEAM_STORAGE_KEY)).toBe(PERSONAL.id);
  });

  it("falls back to the first team when there is no personal one", async () => {
    signedIn(http, [ACME]);

    renderApp();

    await waitFor(() => expect(boardReads().length).toBeGreaterThan(0));
    expect(boardReads()[0].headers["rimaia-team"]).toBe(ACME.id);
  });

  it("falls back to the personal team after the stream is refused not_found", async () => {
    window.localStorage.setItem(TEAM_STORAGE_KEY, ACME.id);
    signedIn(http);
    renderApp();
    await waitFor(() => expect(last(http.events().opens)?.headers["rimaia-team"]).toBe(ACME.id));

    // The person was removed from Acme: the list no longer has it, and the
    // stream that names it is refused.
    http.answer("list_teams", [PERSONAL]);
    http.events().failOpen(404, { code: "not_found", message: "There is no team with that id." });
    await act(async () => http.events().end());

    // After the library's first backoff, on a real timer.
    const slow = { timeout: 4000 };
    await waitFor(
      () => expect(last(boardReads())?.headers["rimaia-team"]).toBe(PERSONAL.id),
      slow,
    );
    await waitFor(
      () => expect(last(http.events().opens)?.headers["rimaia-team"]).toBe(PERSONAL.id),
      slow,
    );
    expect(window.localStorage.getItem(TEAM_STORAGE_KEY)).toBe(PERSONAL.id);
  }, 10_000);

  it("closes an open task panel when the team changes", async () => {
    signedIn(http);
    http.answer("list_tasks", [taskSummary({ id: "task-1", title: "Add login", column: "ready" })]);
    http.answer("get_task", {
      ...taskSummary({ id: "task-1", title: "Add login", column: "ready" }),
      links: [],
      worktreeRunner: null,
      dependsOn: [],
      lastRun: null,
      reviewInstructions: null,
      reviewConfig: {},
      reviewLoop: null,
    });
    renderApp();
    fireEvent.click(await screen.findByRole("button", { name: /Add login/ }));
    await waitFor(() => expect(document.querySelector(".task-detail-panel")).not.toBeNull());

    fireEvent.click(screen.getByRole("button", { name: /^Team/ }));
    fireEvent.click(await screen.findByRole("menuitemradio", { name: /Acme platform/ }));

    await waitFor(() => expect(document.querySelector(".task-detail-panel")).toBeNull());
  });

  it("shows the team name with no menu when there is one team", async () => {
    signedIn(http, [PERSONAL]);
    renderApp();

    await screen.findByRole("navigation", { name: "Main" });

    const nav = screen.getByRole("navigation", { name: "Main" });
    expect(within(nav).getByText("Personal")).toBeInTheDocument();
    expect(within(nav).queryByRole("button", { name: /^Team/ })).not.toBeInTheDocument();
  });
});

describe("solo", () => {
  it("renders no sign-in, account entry or switcher in solo mode", async () => {
    mockInvoke.mockImplementation(async (command) => {
      switch (command) {
        case "get_app_info":
          return {
            appVersion: "0.1.0",
            dataDir: "/data",
            dbFile: "/data/rimaia.db",
            logsDir: "/data/logs",
            onboardingDismissed: true,
          };
        case "run_doctor":
          return { results: [], dismissals: [] };
        case "get_review_digest":
          return emptyDigest();
        case "get_queue_status":
          return { state: "paused", runningTaskIds: [], plan: [] };
        default:
          return [];
      }
    });
    vi.mocked(listen).mockResolvedValue(vi.fn());

    render(<App />);

    await screen.findByRole("navigation", { name: "Main" });
    const nav = screen.getByRole("navigation", { name: "Main" });
    expect(within(nav).queryByRole("button", { name: /^Account/ })).not.toBeInTheDocument();
    expect(within(nav).queryByText("Team")).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Sign in with GitHub" })).not.toBeInTheDocument();
    const sent = mockInvoke.mock.calls.map(([command]) => command);
    expect(sent).not.toContain("get_account");
    expect(sent).not.toContain("list_teams");
    expect(sent).not.toContain("get_client_capabilities");
  });
});

describe("what a browser never asks of a machine", () => {
  it("opens no welcome screen, reads no app info and shows no doctor banner or version", async () => {
    signedIn(http);

    renderApp();
    await screen.findByRole("navigation", { name: "Main" });

    expect(document.querySelector(".welcome-view")).toBeNull();
    expect(document.querySelector(".doctor-banner")).toBeNull();
    expect(document.querySelector(".sidebar-version")).toBeNull();
    expect(mockInvoke).not.toHaveBeenCalled();
    expect(http.requests.map((request) => request.name)).not.toContain("get_app_info");
    expect(http.requests.map((request) => request.name)).not.toContain("run_doctor");
  });
});
