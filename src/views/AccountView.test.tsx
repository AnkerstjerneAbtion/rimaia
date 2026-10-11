import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import App from "../App";
import { ClientCapabilitiesProvider } from "../lib/capabilities";
import { installTransports } from "../lib/client";
import { setCurrentTeam } from "../lib/commands";
import { TeamsProvider } from "../lib/teams";
import { BROWSER } from "../test/capabilities";
import { mockHttp } from "../test/http";
import type { HttpMock } from "../test/http";
import { ACME, PERSONAL, apiToken, runner, session, signedIn } from "../test/web";
import { AccountView } from "./AccountView";
import { pairingLine } from "./account/AccountRunnersSection";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const NOW = new Date("2026-10-04T09:00:00Z");

let http: HttpMock;

/** The account page alone, under a browser with a team chosen. */
function renderAccount() {
  installTransports("browser");
  setCurrentTeam(PERSONAL.id);
  return render(
    <ClientCapabilitiesProvider capabilities={BROWSER}>
      <AccountView />
    </ClientCapabilitiesProvider>,
  );
}

/** The commands sent to the server, by name. */
function names(): string[] {
  return http.requests.map((request) => request.name);
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  http = mockHttp();
  signedIn(http);
  window.localStorage.clear();
});

afterEach(() => {
  cleanup();
  installTransports("solo");
  setCurrentTeam(null);
  vi.unstubAllGlobals();
});

describe("the account page", () => {
  it("lists who is signed in, their sessions, tokens and runners", async () => {
    http.answer("list_sessions", [
      session(),
      session({ id: "session-phone", current: false, userAgent: "curl/8.7.1" }),
    ]);
    http.answer("list_runners", [runner(), runner({ id: "runner-ci", label: "Build box", lastSeenAt: null })]);
    renderAccount();

    expect(await screen.findByText("ada-lovelace")).toBeInTheDocument();
    expect(await screen.findByText("Firefox on macOS")).toBeInTheDocument();
    expect(screen.getByText("this browser")).toBeInTheDocument();
    expect(screen.getByText("curl/8.7.1")).toBeInTheDocument();
    expect(await screen.findByText("Claude Code on my laptop")).toBeInTheDocument();
    expect(await screen.findByText("Studio Mac")).toBeInTheDocument();
    expect(screen.getByText("Build box")).toBeInTheDocument();
    expect(screen.getByText(/last seen never/)).toBeInTheDocument();
    expect(document.body.textContent).not.toContain("rmp_");
    expect(names().sort()).toEqual(
      ["get_account", "list_api_tokens", "list_runners", "list_sessions"].sort(),
    );
  });

  it("shows a new token's secret once", async () => {
    http.answer("create_personal_access_token", {
      token: "rmp_the-secret-value",
      summary: apiToken({ id: "token-new", label: "Claude Code" }),
    });
    renderAccount();
    await screen.findByText("Claude Code on my laptop");

    fireEvent.click(screen.getByRole("button", { name: "New personal access token" }));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Claude Code" } });
    fireEvent.click(screen.getByRole("button", { name: "Create token" }));

    const dialog = await screen.findByRole("dialog", { name: "New personal access token" });
    expect(within(dialog).getByText("rmp_the-secret-value")).toBeInTheDocument();
    expect(within(dialog).getByText("This is the only time it will be shown.")).toBeInTheDocument();
    const create = http.requests.find((request) => request.name === "create_personal_access_token");
    expect(create?.body).toEqual({ label: "Claude Code" });

    fireEvent.click(within(dialog).getByRole("button", { name: "Done" }));

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain("rmp_the-secret-value");
    expect(JSON.stringify({ ...window.localStorage })).not.toContain("rmp_the-secret-value");
    expect(JSON.stringify({ ...window.sessionStorage })).not.toContain("rmp_the-secret-value");
    // The re-read the creation caused is the list, which holds no secret.
    await waitFor(() =>
      expect(names().filter((name) => name === "list_api_tokens").length).toBeGreaterThan(1),
    );
    expect(document.body.textContent).not.toContain("rmp_the-secret-value");
  });

  it("sends a label and an expiry in days, and no teams when none are checked", async () => {
    http.answer("create_personal_access_token", { token: "rmp_x", summary: apiToken() });
    renderAccount();
    await screen.findByText("Claude Code on my laptop");
    fireEvent.click(screen.getByRole("button", { name: "New personal access token" }));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "CI" } });
    fireEvent.change(screen.getByLabelText("Expires after (days)"), { target: { value: "30" } });
    fireEvent.click(screen.getByRole("button", { name: "Create token" }));

    await screen.findByRole("button", { name: "Copy token" });

    const create = http.requests.find((request) => request.name === "create_personal_access_token");
    expect(create?.body).toEqual({ label: "CI", expiresInDays: 30 });
  });

  it("restricts a token to the teams that are checked", async () => {
    http.answer("create_personal_access_token", { token: "rmp_x", summary: apiToken() });
    installTransports("browser");
    setCurrentTeam(PERSONAL.id);
    render(
      <ClientCapabilitiesProvider capabilities={BROWSER}>
        <TeamsProvider value={{
          teams: [PERSONAL, ACME],
          current: PERSONAL,
          choose: () => {},
          reload: async () => {},
        }}>
          <AccountView />
        </TeamsProvider>
      </ClientCapabilitiesProvider>,
    );
    await screen.findByText("Claude Code on my laptop");

    fireEvent.click(screen.getByRole("button", { name: "New personal access token" }));
    fireEvent.change(screen.getByLabelText("Label"), { target: { value: "Acme only" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "Acme platform" }));
    fireEvent.click(screen.getByRole("button", { name: "Create token" }));

    await screen.findByRole("button", { name: "Copy token" });
    const create = http.requests.find((request) => request.name === "create_personal_access_token");
    expect(create?.body).toEqual({ label: "Acme only", teamIds: [ACME.id] });
  });

  it("revokes a token by its id", async () => {
    renderAccount();
    await screen.findByText("Claude Code on my laptop");

    fireEvent.click(screen.getByRole("button", { name: "Revoke the token Claude Code on my laptop" }));

    await waitFor(() => expect(names()).toContain("revoke_api_token"));
    expect(http.requests.find((request) => request.name === "revoke_api_token")?.body).toEqual({
      id: "token-claude",
    });
  });

  it("revokes another session by its id and stays signed in", async () => {
    http.answer("list_sessions", [
      session(),
      session({ id: "session-phone", current: false, userAgent: "curl/8.7.1" }),
    ]);
    renderAccount();
    await screen.findByText("curl/8.7.1");

    fireEvent.click(screen.getByRole("button", { name: "Revoke the session on curl/8.7.1" }));

    await waitFor(() => expect(names()).toContain("revoke_session"));
    expect(http.requests.find((request) => request.name === "revoke_session")?.body).toEqual({
      id: "session-phone",
    });
  });

  it("unpairs a runner only after the confirmation", async () => {
    renderAccount();
    await screen.findByText("Studio Mac");

    fireEvent.click(screen.getByRole("button", { name: "Unpair the runner Studio Mac" }));

    expect(names()).not.toContain("unpair_runner");
    const confirm = screen.getByRole("group", { name: "Unpair Studio Mac" });
    expect(within(confirm).getByText(/Its past runs keep its name/)).toBeInTheDocument();

    fireEvent.click(within(confirm).getByRole("button", { name: "Keep it" }));
    expect(names()).not.toContain("unpair_runner");

    fireEvent.click(screen.getByRole("button", { name: "Unpair the runner Studio Mac" }));
    fireEvent.click(screen.getByRole("button", { name: "Unpair Studio Mac" }));

    await waitFor(() => expect(names()).toContain("unpair_runner"));
    expect(http.requests.find((request) => request.name === "unpair_runner")?.body).toEqual({
      id: "runner-studio",
    });
  });

  it("shows the pairing line with this origin", async () => {
    vi.stubGlobal("location", { ...window.location, origin: "https://rimaia.example" });
    http.answer("create_pairing_code", { code: "ABCD-EFGH", expiresAt: "2026-10-04T09:10:00Z" });
    renderAccount();
    await screen.findByText("Studio Mac");

    fireEvent.click(screen.getByRole("button", { name: "Pair a runner" }));

    expect(
      await screen.findByText("rimaia-runner pair https://rimaia.example ABCD-EFGH"),
    ).toBeInTheDocument();
    expect(pairingLine("https://rimaia.example", "ABCD-EFGH")).toBe(
      "rimaia-runner pair https://rimaia.example ABCD-EFGH",
    );
  });

  it("renders relative times from the injected clock", async () => {
    // `now` is a prop of each section, so a test fixes it.
    const { SessionsSection } = await import("./account/SessionsSection");
    installTransports("browser");
    render(<SessionsSection now={NOW} />);

    expect(await screen.findByText(/last used 1m ago/)).toBeInTheDocument();
  });
});

describe("signing out", () => {
  it("signs out by revoking the current session", async () => {
    http.answer("revoke_session", null);
    installTransports("browser");
    render(
      <ClientCapabilitiesProvider capabilities={BROWSER}>
        <App />
      </ClientCapabilitiesProvider>,
    );
    fireEvent.click(await screen.findByRole("button", { name: /^Account/ }));
    const signOut = await screen.findByRole("button", { name: "Sign out" });
    await waitFor(() => expect(names()).toContain("list_runners"));
    const before = http.requests.length;

    await act(async () => {
      fireEvent.click(signOut);
    });

    expect(await screen.findByRole("link", { name: "Sign in with GitHub" })).toBeInTheDocument();
    const after = http.requests.slice(before);
    // The one request is the revocation, with the current session's id; the
    // sign-in screen is up without any request being refused first.
    expect(after.map((request) => request.name)).toEqual(["revoke_session"]);
    expect(after[0].body).toEqual({ id: "session-current" });
  });

  it("is not offered to a client that is not signed in with a session", async () => {
    http.answer("get_account", {
      id: "user-ada",
      login: "ada-lovelace",
      avatarUrl: null,
      teams: [PERSONAL, ACME],
      current: { kind: "desktop", id: "token-desktop" },
    });
    renderAccount();

    await screen.findByText("ada-lovelace");

    expect(screen.queryByRole("button", { name: "Sign out" })).not.toBeInTheDocument();
  });
});
