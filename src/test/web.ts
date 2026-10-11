import type {
  Account,
  ApiTokenSummary,
  RunnerListing,
  SessionSummary,
  TeamListing,
} from "../types";
import type { HttpMock } from "./http";
import { emptyDigest, repository, taskSummary } from "./reviewFixtures";

/** Plain data for the web shell's tests: a person in a personal and a shared
 *  team, with a session, a token and runners to list. */

export const PERSONAL: TeamListing = {
  id: "team-personal",
  name: "Personal",
  role: "owner",
  personal: true,
};

export const ACME: TeamListing = {
  id: "team-acme",
  name: "Acme platform",
  role: "member",
  personal: false,
};

export function account(overrides: Partial<Account> = {}): Account {
  return {
    id: "user-ada",
    login: "ada-lovelace",
    avatarUrl: null,
    teams: [PERSONAL, ACME].map(({ id, name, role }) => ({ id, name, role })),
    current: { kind: "session", id: "session-current" },
    ...overrides,
  };
}

export function session(overrides: Partial<SessionSummary> = {}): SessionSummary {
  return {
    id: "session-current",
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:134.0) Gecko/20100101 Firefox/134.0",
    createdAt: "2026-10-01T09:00:00Z",
    lastUsedAt: "2026-10-04T08:59:00Z",
    current: true,
    ...overrides,
  };
}

export function apiToken(overrides: Partial<ApiTokenSummary> = {}): ApiTokenSummary {
  return {
    id: "token-claude",
    kind: "personal",
    label: "Claude Code on my laptop",
    runnerId: null,
    teamIds: [],
    createdAt: "2026-09-28T09:00:00Z",
    lastUsedAt: "2026-10-04T07:00:00Z",
    lastUsedFrom: "claude-code/2.1.234",
    expiresAt: null,
    current: false,
    ...overrides,
  };
}

export function runner(overrides: Partial<RunnerListing> = {}): RunnerListing {
  return {
    id: "runner-studio",
    label: "Studio Mac",
    provider: "claude-code",
    appVersion: "0.5.2",
    eligibility: "assigned",
    pairedAt: "2026-09-14T09:00:00Z",
    lastSeenAt: "2026-10-04T08:56:00Z",
    ...overrides,
  };
}

/** Answers what a signed-in browser reads on the way in, for `teams`, and an
 *  empty board for whichever team is named. */
export function signedIn(http: HttpMock, teams: TeamListing[] = [PERSONAL, ACME]): void {
  http.answer("get_account", account());
  http.answer("list_teams", teams);
  http.answer("list_tasks", []);
  http.answer("list_repositories", [repository()]);
  http.answer("get_review_digest", emptyDigest());
  http.answer("list_sessions", [session()]);
  http.answer("list_api_tokens", [apiToken()]);
  http.answer("list_runners", [runner()]);
}

export { taskSummary };
