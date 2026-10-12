import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { ClientCapabilitiesProvider } from "../../lib/capabilities";
import { installTransports } from "../../lib/client";
import { setCurrentTeam } from "../../lib/commands";
import { TeamsProvider } from "../../lib/teams";
import { BROWSER } from "../../test/capabilities";
import { mockHttp } from "../../test/http";
import type { HttpMock } from "../../test/http";
import type { Invitation, TeamListing, TeamMember } from "../../types";
import { TeamSection } from "./TeamSection";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const OWNED: TeamListing = {
  id: "team-engines",
  name: "Engines",
  role: "owner",
  personal: false,
  memberCount: 2,
  ownerCount: 1,
};

const JOINED: TeamListing = {
  id: "team-acme",
  name: "Acme platform",
  role: "member",
  personal: false,
  memberCount: 2,
  ownerCount: 1,
};

function member(overrides: Partial<TeamMember> & Pick<TeamMember, "userId" | "login">): TeamMember {
  return {
    avatarUrl: null,
    role: "member",
    joinedAt: "2026-09-20T09:00:00Z",
    isPersonalOwner: false,
    ...overrides,
  };
}

const MEMBERS: TeamMember[] = [
  member({ userId: "user-ada", login: "ada-lovelace", role: "owner" }),
  member({ userId: "user-charles", login: "charles-babbage" }),
];

function pending(): Invitation {
  return {
    id: "invitation-1",
    teamId: OWNED.id,
    role: "member",
    status: "pending",
    createdBy: "user-ada",
    createdByLogin: "ada-lovelace",
    createdAt: "2026-10-03T09:00:00Z",
    expiresAt: "2026-10-10T09:00:00Z",
    acceptedBy: null,
    acceptedByLogin: null,
    acceptedAt: null,
    revokedAt: null,
  };
}

let http: HttpMock;

function renderSection(team: TeamListing) {
  installTransports("browser");
  setCurrentTeam(team.id);
  return render(
    <ClientCapabilitiesProvider capabilities={BROWSER}>
      <TeamsProvider
        value={{ teams: [team], current: team, choose: () => {}, reload: async () => {} }}
      >
        <TeamSection />
      </TeamsProvider>
    </ClientCapabilitiesProvider>,
  );
}

function sent(name: string) {
  return http.requests.filter((request) => request.name === name);
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  http = mockHttp();
  http.answer("list_team_members", MEMBERS);
  http.answer("list_invitations", [pending()]);
});

afterEach(() => {
  cleanup();
  installTransports("solo");
  setCurrentTeam(null);
  vi.unstubAllGlobals();
});

describe("the Team section", () => {
  it("gives an owner role controls, Remove and Create link", async () => {
    renderSection(OWNED);

    expect(await screen.findByText("charles-babbage")).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Role of charles-babbage" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove charles-babbage" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create link" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /^Revoke the member link/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Delete team" })).toBeInTheDocument();
  });

  it("gives a member none of them", async () => {
    renderSection(JOINED);

    expect(await screen.findByText("charles-babbage")).toBeInTheDocument();
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Remove/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Create link" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete team" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Leave team" })).toBeEnabled();
    expect(sent("list_invitations")).toEqual([]);
  });

  it("disables Leave team with the rule's sentence for the last owner", async () => {
    renderSection(OWNED);

    const leave = await screen.findByRole("button", { name: "Leave team" });

    expect(leave).toBeDisabled();
    expect(
      screen.getByText("Engines needs at least one owner: make someone else an owner first"),
    ).toBeInTheDocument();
  });

  it("disables Leave team in a personal team, with its sentence", async () => {
    renderSection({ ...OWNED, personal: true, name: "Personal" });

    expect(await screen.findByRole("button", { name: "Leave team" })).toBeDisabled();
    expect(
      screen.getByText("a personal team always keeps the person it belongs to as an owner"),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete team" })).not.toBeInTheDocument();
  });

  it("shows a created link once, and not after a re-render from list_invitations", async () => {
    http.answer("create_invitation", { invitation: pending(), token: "rmi_the-link-token" });
    const { unmount } = renderSection(OWNED);
    await screen.findByText("charles-babbage");

    fireEvent.change(screen.getByRole("combobox", { name: "Role" }), {
      target: { value: "owner" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create link" }));

    const shown = await screen.findByRole("group", { name: "New invitation link" });
    expect(
      within(shown).getByText(`${window.location.origin}/invite#rmi_the-link-token`),
    ).toBeInTheDocument();
    expect(
      within(shown).getByText("This link is shown once and works for seven days."),
    ).toBeInTheDocument();
    expect(sent("create_invitation")[0].body).toEqual({ teamId: OWNED.id, role: "owner" });
    expect(JSON.stringify({ ...window.localStorage })).not.toContain("rmi_the-link-token");
    expect(JSON.stringify({ ...window.sessionStorage })).not.toContain("rmi_the-link-token");

    unmount();
    const reads = sent("list_invitations").length;
    renderSection(OWNED);
    await waitFor(() => expect(sent("list_invitations").length).toBeGreaterThan(reads));
    await screen.findByRole("button", { name: /^Revoke the member link/ });

    expect(screen.queryByRole("group", { name: "New invitation link" })).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain("rmi_the-link-token");
  });

  it("enables Delete team only while the typed name matches exactly", async () => {
    http.answer("delete_team", null);
    renderSection(OWNED);
    const confirm = await screen.findByLabelText("Type Engines to confirm");
    const remove = screen.getByRole("button", { name: "Delete team" });

    expect(remove).toBeDisabled();
    for (const typed of ["engines", "Engines ", "Engine"]) {
      fireEvent.change(confirm, { target: { value: typed } });
      expect(remove).toBeDisabled();
    }
    fireEvent.change(confirm, { target: { value: "Engines" } });
    expect(remove).toBeEnabled();

    fireEvent.click(remove);
    await waitFor(() => expect(sent("delete_team").length).toBe(1));
    expect(sent("delete_team")[0].body).toEqual({ teamId: OWNED.id, confirmName: "Engines" });
  });

  it("renders nothing without a team switcher, as in solo", () => {
    const { container } = render(<TeamSection />);

    expect(container).toBeEmptyDOMElement();
  });
});
