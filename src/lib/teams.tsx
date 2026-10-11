import { createContext, useContext } from "react";
import type { ReactNode } from "react";

import type { TeamListing } from "../types";

/** The teams a signed-in person can look at, and which one is on screen. */
export interface TeamsValue {
  readonly teams: readonly TeamListing[];
  readonly current: TeamListing;
  /** Switches the board to `teamId`: names it to the transport, reopens the
   *  stream and remounts the content area (`App` owns the last). */
  readonly choose: (teamId: string) => void;
}

const Teams = createContext<TeamsValue | null>(null);

export function TeamsProvider({
  value,
  children,
}: {
  readonly value: TeamsValue;
  readonly children: ReactNode;
}) {
  return <Teams.Provider value={value}>{children}</Teams.Provider>;
}

/** The team switcher's state, or `null` where there is none to switch: solo,
 *  and any screen shown before a team is chosen. */
export function useTeams(): TeamsValue | null {
  return useContext(Teams);
}
