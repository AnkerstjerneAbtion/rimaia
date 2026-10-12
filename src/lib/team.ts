import { setCurrentTeam } from "./commands";
import { reconnectEventStream } from "./events";
import type { TeamListing } from "../types";

/** Where the chosen team survives a reload (task 050 Scope 6). Not a secret
 *  and not a credential: the server decides what the caller may reach. */
export const TEAM_STORAGE_KEY = "rimaia.team";

function storage(): Storage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    // A browser with storage switched off still has a board to look at.
    return null;
  }
}

/**
 * The team a person falls back to when the one they had is gone: their
 * personal team, or the first they belong to when there is none. `teams` is
 * `list_teams`'s answer, personal first.
 */
export function fallbackTeam(teams: readonly TeamListing[]): TeamListing | undefined {
  return teams.find((team) => team.personal) ?? teams[0];
}

/**
 * The team to open on: the one stored, if `teams` still lists it, else the
 * fallback. `undefined` only for a person in no team at all.
 */
export function startingTeam(teams: readonly TeamListing[]): TeamListing | undefined {
  const stored = storage()?.getItem(TEAM_STORAGE_KEY) ?? null;
  return teams.find((team) => team.id === stored) ?? fallbackTeam(teams);
}

/**
 * Makes `teamId` the team every request and the event stream are about:
 * names it to the transport, remembers it, and reopens the stream so it
 * carries the new header. Remounting the content area is the caller's.
 */
export function chooseTeam(teamId: string): void {
  setCurrentTeam(teamId);
  try {
    storage()?.setItem(TEAM_STORAGE_KEY, teamId);
  } catch {
    // Not remembered, still chosen.
  }
  reconnectEventStream();
}
