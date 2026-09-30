# 29. Teams, membership, and isolation between them

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

ADR-0027 introduces a hosted server. Rimaia will run **one** hosted instance, not one per
organisation, and teams are created on it:

- A person creates a team and invites people into it.
- One person may belong to several teams.
- A person who uses Rimaia on their own does it in a team of one.

That makes the instance **multi-tenant**. Several unrelated groups (potentially different
clients) share one server and one database. The failure that matters most is not a bug that
loses a task. It is one team seeing another team's plans, transcripts or repository names.
Plans describe unreleased work, and transcripts contain source code. A leak between teams
that are different clients is a data-protection incident.

Today nothing in the schema knows about groups of people. The only attribution is
`tasks.source`, recording which door a task came through (`ui`, `mcp` or `system`; ADR-0019).
There is no user, no owner and no boundary.

## Decision

### 1. A team is the unit of ownership and isolation

Every board entity belongs to exactly one team:

- **`team_id` is a direct column** on `repositories`, `tasks` and team `settings`.
- **Runs, links and dependency edges** inherit their team through their task.

`tasks` carries `team_id` directly even though it could be derived through its repository.
That is deliberate. The scoping filter is then on the table every board query already reads,
and a join that is left out cannot silently drop it.

A **team member** is a `team_memberships(team_id, user_id, role)` row.

### 2. Every user has a personal team

Signing up creates a personal team with its owner as its only member. Someone using Rimaia
alone needs nothing else, and there is no separate "personal" code path. A personal team
behaves like any other team, which is why it can later gain members with no data moving.

Solo mode (ADR-0027) is the same model: one implicit team and one implicit user, created by
migration with ordinary generated UUIDv4 ids (seam-contract D10) and recorded as the
installation's solo identity. Ids are generated, not fixed constants, so two solo boards can
later be imported into one server without colliding (ADR-0028). Solo never shows sign-in or
teams, but its services filter by team exactly as the server does. That is what makes "works in solo" evidence for
"works connected".

### 3. Two roles

| Role | Can |
| --- | --- |
| **Owner** | Everything a member can, plus: invite and remove members, change roles, register and remove repositories, change team settings, set the team-wide ceiling on unattended runs (ADR-0032), delete the team |
| **Member** | Create, edit, order, assign and archive tasks. Read every run and transcript in the team. Pair their own runners (ADR-0030) |

A team must always have at least one owner. The last owner cannot leave or be demoted.

Two roles are enough to separate "can change what the team is" from "can do the team's
work". Finer roles (read-only guests, per-repository permissions) are additive and wait until
a real team needs them.

### 4. Invitations are links

An owner creates an invitation: a single-use token that expires after seven days. The link
works for any signed-in account. It is not tied to an email address, because GitHub sign-in
(ADR-0030) does not reliably give the server a verified address. Owners can list and revoke
pending invitations. Accepting creates a membership with the role the invitation named.

### 5. Isolation is enforced in the service layer, and tested per door

The team scope travels on `ServiceContext`, the same way ADR-0019 put `MutationSource` there
and for the same reason: it is an ambient property of a request, not an argument each caller
remembers to pass. Every service that reads or writes board state filters by it.

ADR-0019 fixed that struct's shape and said "a later field is a later record". This is that
record for `scope`, and ADR-0030 is it for the acting user. As with `source`, there is no
default. The HTTP edge, the MCP edge, the runner-report edge and solo's shell each construct
a context with an explicit scope, and a missing one is a compile error.

Resolving *which* teams a request may touch happens once, at the edge:

- **HTTP API:** from the session or token (ADR-0030).
- **MCP:** from the token, and from the `team` argument where a tool takes one (ADR-0035).
- **Runner reports:** from the runner's lease (ADR-0031).

After that, a service function cannot be called without a scope.

Rules that follow:

- **An entity outside the caller's teams does not exist.** It is `not found`, never
  `forbidden`, so ids cannot be probed across teams. This is the rule ADR-0006 already applies
  to revoked run tokens.
- **Change events are filtered per subscriber.** ADR-0018's event carries only ids, but a
  task id arriving on another team's stream still leaks that the task exists and when it
  changed. Each event therefore also carries the team it belongs to (ADR-0034 point 3).
  Filtering then needs no lookup, and still works for a task that has just been deleted.
  The server's fan-out sends each subscriber only events for teams it may read.
- **Cross-team references are refused at write time.** A dependency edge, a repository and a
  task must share a team. This extends ADR-0008's refusal of cross-repository dependencies.
- **A task does not move between teams in this version.** Its runs were paid for with that
  team's members' subscriptions, and its transcripts were visible to that team. Moving the
  history would disclose it to a new audience, and leaving it behind would split one task's
  record in two. "Copy to team" creates a new task from the plan and nothing else.
- **There is a test for every surface.** Each HTTP route, MCP tool and runner report is
  exercised with a fixture of two teams, asserting that team B's ids are invisible to team A.
  Like `every_registered_tool_has_a_run_scope_decision` (ADR-0021), a registry test fails
  when a route or tool is added without a cross-team case.

### 6. Deleting a team deletes its data

Deleting a team removes its tasks, runs, transcripts, repositories-as-known-to-the-team and
invitations. It is owner-only and requires typing the team's name to confirm. Personal teams
are deleted with their user's account.

Deletion is immediate in the live database. The deleted data disappears from backups when
the backup retention period passes (ADR-0037), because point-in-time backups cannot be edited
selectively. This is the deletion path a data-processing agreement will require, and the
agreement states the backup period.

Runners are not told to delete anything. Their local worktrees and transcripts are their
owners' files, and each runner's own cleanup (ADR-0025, ADR-0033) applies to them.

## Consequences

- **Tenant isolation is now a correctness property on the same level as run-state
  transitions.** Like them, it goes on CLAUDE.md's list of modules that must have tests.
- **Every board query gains a filter**, and every board table a column. In solo mode the
  filter compares against one constant. Correct, trivially cheap, and it keeps the code path
  identical.
- **A person in several teams sees several boards.** The UI needs a team switcher, and the
  MCP surface needs a way to name a team (ADR-0035). A runner serves its user across all their
  teams (ADR-0031), so one spare machine can work for someone's personal team and their work
  team alike.
- **A leak is still possible through anything outside the service layer:** logs, error
  messages, analytics exports. Server logs are treated as containing team data (ADR-0037).

## Alternatives considered

- **One instance per organisation (single-tenant).** Stronger isolation, since there is no
  shared database to leak through, and simpler code. Rejected because the product needs teams
  to be cheap: a personal team for everyone, and a new team without a deployment. The instance
  can still be self-hosted per organisation for anyone who needs the stronger boundary. The
  image is the same (ADR-0037).
- **A database file per team.** Physical isolation inside one instance. Rejected: it turns
  one migration into N, makes cross-team queries (a user's teams, a runner serving several
  teams) into cross-database work, and complicates backups, all to enforce a rule that one
  scoped `ServiceContext` and a registry test can enforce.
- **Organisations above teams.** The usual SaaS shape (organisation → teams → members).
  Rejected for now: nothing yet needs billing or administration above a team, and adding a
  parent later is additive, while removing one is not.
- **Email invitations.** Familiar, and they need an email sender, deliverability and
  verified addresses to match against. A link does the same job with none of that. Email can
  deliver the link later.
