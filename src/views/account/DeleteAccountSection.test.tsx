import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { ClientCapabilitiesProvider } from "../../lib/capabilities";
import { installTransports } from "../../lib/client";
import { setCurrentTeam } from "../../lib/commands";
import { BROWSER } from "../../test/capabilities";
import { mockHttp } from "../../test/http";
import type { HttpMock } from "../../test/http";
import { ACME, PERSONAL, account } from "../../test/web";
import type { TeamListing, TeamMember } from "../../types";
import { DeleteAccountSection, blockingTeams } from "./DeleteAccountSection";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

/** A shared team Ada alone owns: it blocks deleting her account. */
const ENGINES: TeamListing = {
  id: "team-engines",
  name: "Analytical engines",
  role: "owner",
  personal: false,
  memberCount: 3,
  ownerCount: 1,
};

/** One she co-owns, which does not. */
const GUILD: TeamListing = {
  id: "team-guild",
  name: "Guild",
  role: "owner",
  personal: false,
  memberCount: 3,
  ownerCount: 2,
};

function member(userId: string, login: string, isPersonalOwner = false): TeamMember {
  return {
    userId,
    login,
    avatarUrl: null,
    role: isPersonalOwner ? "owner" : "member",
    joinedAt: "2026-09-20T09:00:00Z",
    isPersonalOwner,
  };
}

let http: HttpMock;

function renderSection(teams: TeamListing[] = [PERSONAL, ACME, ENGINES, GUILD]) {
  installTransports("browser");
  setCurrentTeam(PERSONAL.id);
  return render(
    <ClientCapabilitiesProvider capabilities={BROWSER}>
      <DeleteAccountSection account={account()} teams={teams} />
    </ClientCapabilitiesProvider>,
  );
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  http = mockHttp();
  http.answer("list_team_members", [
    member("user-ada", "ada-lovelace", true),
    member("user-charles", "charles-babbage"),
  ]);
});

afterEach(() => {
  cleanup();
  installTransports("solo");
  setCurrentTeam(null);
  vi.unstubAllGlobals();
});

describe("deleting an account", () => {
  it("names only the shared teams the person alone owns", () => {
    expect(blockingTeams([PERSONAL, ACME, ENGINES, GUILD])).toEqual([ENGINES]);
  });

  it("lists the blocking teams and the personal team's other members", async () => {
    renderSection();

    expect(screen.getByText("Analytical engines")).toBeInTheDocument();
    expect(screen.queryByText("Guild")).not.toBeInTheDocument();
    expect(screen.queryByText("Acme platform")).not.toBeInTheDocument();
    expect(await screen.findByText("charles-babbage")).toBeInTheDocument();
    expect(screen.queryByText("ada-lovelace")).not.toBeInTheDocument();
    const read = http.requests.find((request) => request.name === "list_team_members");
    expect(read?.body).toEqual({ teamId: PERSONAL.id });
  });

  it("enables Delete account only on the exact login", async () => {
    http.answer("delete_account", null);
    renderSection();
    const confirm = screen.getByLabelText("Type ada-lovelace to confirm");
    const remove = screen.getByRole("button", { name: "Delete account" });

    expect(remove).toBeDisabled();
    for (const typed of ["ada", "Ada-Lovelace", "ada-lovelace "]) {
      fireEvent.change(confirm, { target: { value: typed } });
      expect(remove).toBeDisabled();
    }
    fireEvent.change(confirm, { target: { value: "ada-lovelace" } });
    expect(remove).toBeEnabled();

    fireEvent.click(remove);
    await waitFor(() =>
      expect(http.requests.map((request) => request.name)).toContain("delete_account"),
    );
    const sent = http.requests.find((request) => request.name === "delete_account");
    expect(sent?.body).toEqual({ confirmLogin: "ada-lovelace" });
  });
});
