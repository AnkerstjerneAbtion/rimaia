import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import App from "../App";
import { ClientCapabilitiesProvider } from "../lib/capabilities";
import { installTransports } from "../lib/client";
import { setCurrentTeam } from "../lib/commands";
import { INVITATION_STORAGE_KEY } from "../lib/invitation";
import { BROWSER } from "../test/capabilities";
import { mockHttp } from "../test/http";
import type { HttpMock } from "../test/http";
import { emptyDigest } from "../test/reviewFixtures";
import { ACME, PERSONAL, signedIn } from "../test/web";
import type { InvitationPreview, TeamListing } from "../types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const INVALID_LINK = "this invitation link is not valid any more: ask a team owner for a new one";

const PREVIEW: InvitationPreview = {
  teamName: "Analytical engines",
  role: "member",
  invitedByLogin: "charles-babbage",
  expiresAt: "2026-10-10T09:00:00Z",
};

const JOINED: TeamListing = {
  id: "team-engines",
  name: "Analytical engines",
  role: "member",
  personal: false,
  memberCount: 3,
  ownerCount: 1,
};

let http: HttpMock;

function renderApp() {
  installTransports("browser");
  return render(
    <ClientCapabilitiesProvider capabilities={BROWSER}>
      <App />
    </ClientCapabilitiesProvider>,
  );
}

function sent(name: string) {
  return http.requests.filter((request) => request.name === name);
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  http = mockHttp();
  signedIn(http);
  http.answer("preview_invitation", PREVIEW);
  window.localStorage.clear();
  window.sessionStorage.clear();
});

afterEach(() => {
  cleanup();
  installTransports("solo");
  setCurrentTeam(null);
  vi.unstubAllGlobals();
  window.history.replaceState({}, "", "/");
});

describe("the accept page", () => {
  it("takes the token off the address before any request, and previews it", async () => {
    window.history.replaceState({}, "", "/invite#rmi_the-token");

    renderApp();

    // Gone from the address bar on the first render, before a request.
    expect(window.location.hash).toBe("");
    expect(window.location.pathname).toBe("/invite");
    expect(await screen.findByRole("heading", { name: "Join Analytical engines" })).toBeInTheDocument();
    expect(screen.getByText("charles-babbage")).toBeInTheDocument();
    expect(screen.getByText(/^Member:/)).toBeInTheDocument();
    expect(sent("preview_invitation")[0].body).toEqual({ token: "rmi_the-token" });
    for (const request of http.requests) {
      expect(request.url).not.toContain("rmi_the-token");
    }
  });

  it("joins with the fragment's token and selects the new team", async () => {
    window.history.replaceState({}, "", "/invite#rmi_the-token");
    http.answer("accept_invitation", JOINED);
    http.answer("list_teams", [PERSONAL, ACME]);
    renderApp();
    await screen.findByRole("heading", { name: "Join Analytical engines" });
    http.answer("list_teams", [PERSONAL, ACME, JOINED]);

    fireEvent.click(screen.getByRole("button", { name: "Join team" }));

    await waitFor(() => expect(sent("accept_invitation").length).toBe(1));
    expect(sent("accept_invitation")[0].body).toEqual({ token: "rmi_the-token" });
    expect(window.sessionStorage.getItem(INVITATION_STORAGE_KEY)).toBeNull();
    await screen.findByRole("navigation", { name: "Main" });
    await waitFor(() => {
      const reads = sent("list_tasks");
      expect(reads[reads.length - 1]?.headers["rimaia-team"]).toBe(JOINED.id);
    });
    expect(window.location.pathname).toBe("/");
  });

  it("shows the service's sentence for a link that cannot be used, and nothing else", async () => {
    window.history.replaceState({}, "", "/invite#rmi_spent");
    http.refuse("preview_invitation", 404, { code: "not_found", message: INVALID_LINK });

    renderApp();

    expect(await screen.findByText(INVALID_LINK)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Join team" })).not.toBeInTheDocument();
    expect(screen.queryByText(/charles-babbage|Analytical engines/)).not.toBeInTheDocument();
    expect(window.sessionStorage.getItem(INVITATION_STORAGE_KEY)).toBeNull();
  });

  it("uses a token kept across sign-in once, and removes it", async () => {
    // What the page left behind before the sign-in, which lands on `/`.
    window.sessionStorage.setItem(INVITATION_STORAGE_KEY, "rmi_kept");
    http.answer("accept_invitation", JOINED);

    renderApp();
    await screen.findByRole("heading", { name: "Join Analytical engines" });
    expect(sent("preview_invitation")[0].body).toEqual({ token: "rmi_kept" });
    fireEvent.click(screen.getByRole("button", { name: "Join team" }));

    await waitFor(() => expect(window.sessionStorage.getItem(INVITATION_STORAGE_KEY)).toBeNull());
    await screen.findByRole("navigation", { name: "Main" });
    expect(sent("accept_invitation").map((request) => request.body)).toEqual([
      { token: "rmi_kept" },
    ]);
  });

  it("keeps the token through the sign-in screen", async () => {
    window.history.replaceState({}, "", "/invite#rmi_before-sign-in");
    http.refuse("get_account", 401, { code: "unauthenticated", message: "Sign in to continue." });

    renderApp();

    expect(await screen.findByRole("link", { name: "Sign in with GitHub" })).toBeInTheDocument();
    expect(window.location.hash).toBe("");
    expect(window.sessionStorage.getItem(INVITATION_STORAGE_KEY)).toBe("rmi_before-sign-in");
    expect(sent("preview_invitation")).toEqual([]);
  });

  it("never opens in solo", async () => {
    window.sessionStorage.setItem(INVITATION_STORAGE_KEY, "rmi_kept");
    vi.mocked(invoke).mockImplementation(async (command) => {
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
    expect(document.querySelector(".invite-view")).toBeNull();
    expect(vi.mocked(invoke).mock.calls.map(([command]) => command)).not.toContain(
      "preview_invitation",
    );
  });
});
