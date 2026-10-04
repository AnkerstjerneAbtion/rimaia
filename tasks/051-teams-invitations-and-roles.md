---
id: "051"
title: Teams, invitations and roles
milestone: v0.5
status: ready
depends_on: ["050"]
adrs: ["0021", "0029", "0030", "0031", "0032", "0037"]
size: L
---

# Teams, invitations and roles

## Goal

Make a team something people can form, join, leave and delete. After 050 a person can sign
in, see their personal team and switch between the teams they belong to, but no command
creates a team, admits anyone to one, or tells an owner from a member. This task adds
ADR-0029 points 3, 4 and 6 and ADR-0037 point 6's deletion path:

- **Creating a team.** The creator is its only owner.
- **Two roles, enforced.** Every owner-only action in ADR-0029 point 3's table refuses a
  member, including the repository and team-settings writes that already exist.
- **The last-owner rule.** A team always has at least one owner. The last owner cannot
  leave, be removed or be demoted.
- **Invitation links.** Single use, seven days, revocable, accepted by any signed-in account,
  granting the role the invitation names. Only the token's hash is stored.
- **Deleting a team.** Owner only, confirmed by typing the team's name, and ordered the way
  D28's Why requires.
- **Deleting an account.** It deletes the account's personal team with it.
- **Copying a task to another team.** The new task gets the plan and nothing else.

Every capability is a board command in D32's registry, so the HTTP route and the in-process
path come with it. Every one that is not irreversible is also an MCP tool (ADR-0021). The web
shell gains the screens for all of it, and the tenant-isolation suite gains a case for every
new command and tool.

## Why now

**Before 051, a server is a set of personal teams that cannot meet.** 047 gives every
signed-in person a personal team, and 050 gives them a switcher with one entry in it.
Everything M4 builds is about work crossing between people: assignment, consent, chaining
across machines, and transcripts reviewed by someone other than the runner's owner. Each
needs a team with more than one member to test against, and this task is where such a team
first exists.

**Roles must exist before a second person can join.** The 039 and 046 suites prove that team
A cannot see team B. They do not prove that a member cannot rewrite the team's base
instructions. That instruction goes into every prompt run on every member's machine
(ADR-0032 point 3). Until this task every member of a scope can do everything. That was
harmless while every team had exactly one member, and it stops being harmless the moment an
invitation is accepted. So the roles land in the same task as the invitation.

**Deletion must exist before a team outside Abtion uses the instance.** ADR-0037 point 6
makes deletion a precondition for acting as a data processor, not a follow-up.

## Scope

**1. The migration.** `src-tauri/migrations/20261003120400_invitations.sql`, with exactly
D28 part 6's DDL for this file: the `invitations` table and `idx_invitations_team`. Its
header comment follows the existing migrations' voice: why only the hash is stored, and why
accepted and revoked rows are kept. It is the only new migration.

**2. The team service.** A new `crates/core/src/teams/` module (`mod.rs`, `members.rs`,
`invitations.rs`, `delete.rs`, `copy.rs`). Every function takes `&ServiceContext`, as 039
requires. Where a function names a team, the team must be in `ctx.scope`, or the answer is
the one a never-issued id gets (`not_found`, ADR-0029 point 5).

- `create_team(ctx, name) -> Team`. Creates the team and an owner membership for
  `ctx.actor`, and seeds `team_settings.base_instructions` from `DEFAULT_BASE_INSTRUCTIONS`
  through the same internal helper 038's `create_personal_team` uses, so the two cannot
  drift. The name is stored trimmed, and must be 1–100 characters after trimming, as 047's
  labels are; otherwise `invalid`. Names need not be unique: uniqueness would reveal that
  another team exists with that name.
- `list_team_members(ctx, team_id) -> Vec<TeamMember>`, readable by any member. A
  `TeamMember` has `user_id`, `login`, `avatar_url`, `role`, `joined_at` and
  `is_personal_owner`.
- `set_member_role(ctx, team_id, user_id, role)`, `remove_member(ctx, team_id, user_id)` and
  `leave_team(ctx, team_id)`. The first two are owner-only. Leaving is open to every member,
  owners included.
- 050's `list_teams` entry gains `member_count` and `owner_count`, beside its `role` and
  `personal`. The account page needs them to warn before a deletion, without a preview
  command of its own.
- 045's `testing`-feature helper that adds a member keeps its signature and its callers.
  Its body now calls `create_invitation` and `accept_invitation`, as 045 asked.

**3. Roles are checked in the write's own transaction.** One helper,
`teams::require_owner(&mut tx, ctx, team_id, action)`. It reads `team_memberships` for
`ctx.actor` inside the transaction that then writes. It never trusts `Caller.teams`, which
was read at the start of the request (D32 point 7), because a demotion committed in between
must win. **Every transaction that calls it, or applies point 4's last-owner rule, is
`BEGIN IMMEDIATE`**, for the reason 043 gives for its lease transactions: under WAL, a
deferred transaction that reads and then writes can fail with `SQLITE_BUSY_SNAPSHOT`, which
reaches the person as a `database` error instead of the sentence.

The checks run in a fixed order. Scope first, so a non-member gets `not_found`. Role second,
so a member gets `invalid` with D32 point 3's sentence naming the role: exactly `only an
owner of this team can <action>`. `<action>` is a phrase fixed per call site, such as
`invite people` or `register a repository`. 039's team-placement accessor is shared by
every key, so it uses one phrase for all of them: `change the team's settings`.

The owner-only actions are ADR-0029 point 3's list, and **each existing write in that list
gains the check in this task**:

- **Repositories:** `register_repository`, `update_repository`, `remove_repository`, 045's
  `set_repository_unattended_ceiling` and 021's `set_repository_review_config`.
  `register_repository` is a local row until 054 (D32's appendix), so its check sits in
  `repo::register`, which every door calls, and 054's board half keeps it. 045's inline role
  check is replaced by `require_owner`, and its refusal becomes `only an owner of this team
  can change whether a repository allows unattended runs`.
- **Team settings:** every write through 039's team-placement accessor (D3's typed
  accessor). That covers `set_base_instructions`, `set_strategy_catalogue`,
  `set_strategy_defaults` (including `strategy_default.<repository_id>`),
  `set_strategy_approval`, 021's `set_review_settings`, and the `max_turns` and
  `disallowed_tools` keys. Putting the check in the accessor means a key added later is
  owner-only without anyone remembering to make it so.
- **Membership, invitations and deleting the team** (points 2, 4, 5 and 6).

Task edits stay open to members. That includes 021's `set_task_review` (the task's own
`review_instructions` and `review_config`), `accept_task_strategy` and
`clear_task_strategy`. The solo caller is its team's owner (`Caller::solo`, D32 point 7), so
no solo behaviour changes.

**4. The last-owner rule and the personal team.** `set_member_role`, `remove_member`,
`leave_team` and account deletion each apply their change first. Then, in the same
transaction, they count the team's owners and roll back if the count is zero. Writing
first and checking after means two owners demoting each other in concurrent requests cannot
both succeed, whatever order SQLite serializes them in. The refusal is `invalid`:
`<team name> needs at least one owner: make someone else an owner first`.

A personal team (`teams.personal_user_id`) always keeps its person as an owner. They cannot
leave it, be removed from it or be demoted in it, and `delete_team` refuses it. The first
three are `invalid`: `a personal team always keeps the person it belongs to as an owner`.
The refusal to delete is `invalid`: `a personal team is deleted with its account, not on its
own`. The reason: ADR-0029 point 2 gives every user a personal team, and point 6 deletes it
with their account and not otherwise. Other people may still be invited into a personal team
(ADR-0029 point 2: "it can later gain members with no data moving").

**Removing a member, or a member leaving, withdraws them from the team** in the same
transaction:

- their tasks in that team are unassigned: `assignee_id` and `assigned_by` are set to NULL
  where `assignee_id` is theirs;
- every `pinned_runner_id` on that team's tasks that names one of their runners is cleared.
  057 later replaces this write with its `release_pin`, which also advances the generation
  and requeues a waiting task;
- their runners' `runner_pool_teams` rows for that team are deleted;
- every `runner_leases` row their runners hold on that team's tasks gets `expires_at` set to
  now, from `ctx.clock`. The row is expired rather than deleted so that 053's sweep closes
  its run as `interrupted`; a deleted row would leave the run `running` with nothing to
  close it. **053's sweep must not pin a task to a runner whose owner is no longer a member
  of the task's team.** Without that condition the task would be pinned to a runner that
  can no longer reach it, and never claimed again. The seam entry (point 14) binds 053 to
  it;
- their `trusted_authors` rows in that team are deleted in both directions: whom they
  trusted, and who trusted them;
- their `api_tokens` restricted to that team alone are deleted, and a token restricted to it
  and other teams loses only this team's `api_token_teams` row, for point 6 step 2's reason.

A person who is invited back therefore starts with no trust and no restricted token in that
team. Both were decisions about a member, and re-joining is a new decision by whoever sent
the new invitation; old state coming back by itself would be a decision nobody made.

Every `UPDATE` above that touches `tasks` uses `RETURNING id`, and the ids of the tasks whose
leases expired are collected with them. After commit the service publishes
`ChangeEvent::tasks(team_id, ids)` beside the `Teams` event (point 8), and never with an
empty list. Without it a card keeps showing the old assignee (ADR-0018).

**5. Invitations** (`teams/invitations.rs`):

- `create_invitation(ctx, team_id, role) -> CreatedInvitation { invitation, token }`, owner
  only.
  - The token comes from a new `identity::secret::mint_invitation_token() -> Secret`: `rmi_`
    followed by `BASE64URL_NOPAD` of 32 CSPRNG bytes, as 047's `mint_token` builds the
    others, and stored as 047's `hash` of the whole string. **`TokenKind` does not change.**
    It spells `api_tokens.kind`'s `CHECK`, and 047's `Authenticate` maps each of its
    prefixes to a door; an `Invitation` variant would break the first or authenticate as
    the second. The prefix lets a secret scanner recognise a leaked link, and `Authenticate`
    already refuses an unknown prefix, so an invitation pasted as a bearer token is
    `unauthenticated` without a lookup.
  - `expires_at` is `created_at` plus seven days, from `ctx.clock`.
  - The token is returned once and never again. No read returns it, and no log line records
    it.
- `list_invitations(ctx, team_id) -> Vec<Invitation>`, owner only. It returns every row of
  the team, newest first, with a derived `status: InvitationStatus` of `Pending`,
  `Accepted`, `Expired` or `Revoked`. That is an enum, not a string (CLAUDE.md). An accepted
  row names who accepted it, which is the team's record of who let whom in.
- `revoke_invitation(ctx, invitation_id)`, owner of the invitation's team. It sets
  `revoked_at`. Revoking an accepted invitation is `invalid`: `this invitation was already
  accepted: remove the member instead`.
- `preview_invitation(ctx, token) -> InvitationPreview { team_name, role, invited_by_login,
  expires_at }`. The token is the capability, so this is the one read that names a team
  outside the caller's scope. It exists so the accept page can say what the person is
  joining before they join it.
- `accept_invitation(ctx, token) -> Team`. It runs in one transaction:
  1. a conditional `UPDATE … SET accepted_by, accepted_at WHERE token_hash = ? AND
     accepted_at IS NULL AND revoked_at IS NULL AND expires_at > ? RETURNING team_id,
     role`;
  2. the membership insert.

  Single use therefore holds with no lock of its own. A caller who is already a member is
  refused with `invalid`: `you are already a member of <team name>`. The invitation stays
  unused, and the caller's role does not change. An invitation must never be a way to
  change one's own role, and a member who is forwarded an owner's link should not use it up
  for the person it was meant for.

**Every unusable token gets one answer.** An unknown token, and one that is expired, revoked
or already accepted, are all `not_found`: `this invitation link is not valid any more: ask a
team owner for a new one`. `preview_invitation` and `accept_invitation` answer the same way.
The holder can do the same thing in every case, so telling the cases apart would help nobody
except someone probing for tokens.

**The link is `<server origin>/invite#<token>`.** The token sits in the fragment, so a
browser never sends it to the server in a request line or a `Referer`. It therefore never
reaches an access log, which ADR-0037 point 6 treats as holding team data. The accept page
posts it in a board command's body, which `dispatch` never records (D32 point 2). The core
service returns the token and never a URL, because the origin belongs to the client: the
web shell uses its own origin, and a connected desktop uses its server URL. The MCP tool
returns the token and the path `/invite#<token>`.

There is no rate limit on accepting. A 256-bit token cannot be guessed, and 047's limiters
cover the endpoints where guessing is plausible.

**6. Deleting a team** (`teams/delete.rs`). `delete_team(ctx, team_id, confirm_name)`, owner
only, in one transaction. The public command first applies its own two refusals:

- `confirm_name` must equal the stored name exactly. Otherwise `invalid`: `type the team's
  name exactly to delete it`. The UI also enforces this, but the rule lives in the service,
  so every door applies it.
- A personal team is refused (point 4).

Then it calls the shared function `teams::delete::purge(&mut tx, ctx, team_id)`, which
account deletion calls too, and which does the rest in this order:

1. **Running work is refused.** A team with an unexpired lease on any of its tasks
   (`expires_at IS NULL OR expires_at > now`) is refused with `invalid`: `<n> of <team
   name>'s tasks are running: wait for them or cancel them first`. A deleted lease cannot
   fence the runner still working under it, and that runner may be any member's. An expired
   lease does not block: its task is deleted with the team, so nothing is left to close.
2. **Tokens restricted to this team.** Every `api_tokens` row whose `api_token_teams` rows
   name this team and no other is deleted. D28 reads "no rows" as "every team the token's
   user belongs to". A cascade that removed a restricted token's last row would therefore
   silently widen it to the user's whole account. A token restricted to several teams loses
   only this team's row.
3. **The ordered delete D28's Why requires.** `task_dependencies` edges between the team's
   tasks, then the team's tasks (cascading runs, links, bundles, findings, acceptances and
   leases), then its repositories, then the team (cascading memberships, `team_settings`,
   invitations, team-wide acceptances, trust rows, `runner_pool_teams` and the remaining
   `api_token_teams` rows). No raw `DELETE FROM teams` exists anywhere else. The store's
   `RESTRICT` references refuse any other order.

`purge` returns the deleted task and repository ids and the former members' user ids. After
commit they are published under the deleted team's id, with a `Teams` change (point 8). Its
tracing span records the team id and the row counts. It never records the team's name or any
plan (ADR-0037 point 6).

**7. Deleting an account.** `delete_account(ctx, confirm_login)`. It deletes `ctx.actor` and
nothing else. There is no id argument, so no door can name someone else. Its refusals read
`team_memberships` by `ctx.actor` in its own transaction, never through `ctx.scope`. It runs
in one transaction:

1. **Refusals.**
   - `confirm_login` must equal the user's login exactly: `type your login exactly to delete
     your account`.
   - A user who is the only owner of any team other than their personal team is refused,
     with the teams named: `you are the only owner of <names>: make someone else an owner,
     or delete the team, first`. Deleting the account anyway would leave a team that nobody
     can administer or delete.
   - A user whose runner holds a `runner_leases` row on a task outside their personal team,
     **expired or not**, is refused: `your runner <label> is still holding work: wait for it
     to be released`. `runner_leases.runner_id` is `RESTRICT`, so the cascade from `users`
     to `runners` would fail on that row anyway, and this sentence is the one the user
     reads. An expired lease is not deleted here, because its task survives and its run
     would be left `running`. 053's sweep releases it within its interval and closes the
     run, after which the deletion goes through.
2. **The personal team goes first**, through point 6's `purge`, whose running-work refusal
   covers leases held by any member's runner on it. `teams.personal_user_id` is `RESTRICT`,
   which is why it cannot go after the user.
3. **The `users` row.** Before deleting it, the transaction selects, by team, the ids of
   other teams' tasks that name the user in any of D28's user columns on `tasks`, or are
   pinned to one of their runners. Then sessions, tokens, pairing codes, runners,
   memberships, `user_settings`, acceptances and trust rows cascade. Authorship columns
   become NULL, meaning "a former member" (D28, 045). After commit, one
   `ChangeEvent::tasks` per team with a non-empty list.

A personal team that other people were invited into is deleted with it, as ADR-0029 point 6
says. The account page shows who will lose access before the person types their login.

**8. Change events and open streams.** `Change` gains `Teams(Arc<[TeamId]>)`, with a
constructor `ChangeEvent::teams(team_id)` that builds `Audience::Team(team_id)`, as every
constructor 038 wrote does after 048 Scope 1. Creating a team, a membership or role change,
an accepted invitation, a revocation and a deletion each publish one after commit. Tests read
`event.audience`; there is no `team_id` field to read.

**The wire name lives in core, and only there** (048 Scope 3):

- `crates/core/src/api/events.rs`'s board function maps `Change::Teams(ids)` to the
  `WireEvent` `teams:changed`, whose payload is the array of team ids. It has no local
  source. 048 Scope 3's table gains that row.
- `board_reread()` gains `teams:changed []` as its fifth event, after `settings:changed
  null`, as 048's Notes expect. It therefore rides the opening burst and both lag bursts.
  That matters here more than for any other entity: a person whose team set changed has
  their stream ended by `revalidate` (below), and the `Teams` event published just before
  may be lost with it. The reconnect's burst is what makes the switcher re-read.
- **The shell gains no code.** 048 deleted `emit_change_event`'s match, and its forwarder
  emits whatever core's board function returns. Writing `"teams:changed"` anywhere under
  `src-tauri/src/` fails 048's step in `scripts/check-command-wiring.sh`. Solo never
  publishes a `Teams` change, because it refuses team management (point 10). It does emit
  the burst's `teams:changed []` on a lag, which nothing rendered in solo listens to.
- **The frontend.** `src/lib/events.ts` gains `subscribeToTeamsChanged` and an
  `EVENT_SOURCES` row, `teams:changed` → `board` (049 Scope 2). Nothing else imports
  `listen` (D7).

**A changed team set ends the person's streams at once** (ADR-0030 point 2). After commit,
the board handler calls 048's `StreamControl::revalidate(user_id)` on `BoardHost`, the way
048 wires 047's revocation paths:

- for the affected user, on `remove_member`, `leave_team`, `set_member_role` and
  `accept_invitation`;
- for the creator, on `create_team`. Admission reads `caller.teams` as the stream opened
  with it (048 Scope 6), so without this the creator's own stream never carries the new
  team's events;
- for every former member, on `delete_team`;
- for the deleted user and every other member of their personal team, on `delete_account`.

The service returns those user ids; it does not know `BoardHost`. No second mechanism is
added: 048 already ends a stream whose `caller.teams` changed.

**9. Copying a task to another team** (`teams/copy.rs`). `copy_task_to_team(ctx, task_id,
repository_id) -> Task`. The target team is the repository's team, so there is no team
argument to get wrong (039). Both the task and the repository must be in scope, or the
answer is `not_found`. The new task is created through `tasks::create_task`, so every
creation rule applies to it: 045's `created_by` and `plan_updated_by` are the copier, its
`plan_revision` is 1, and its source is the door's. Its fields:

- **Copied:** `title`, `plan` and `extra_instructions`. 045's `plan_revision` covers the plan
  and the extra instructions together, so D28 treats the two as one piece of content.
- **Set:** the column is `not_ready`, at the bottom. Plan text arriving from another team has
  been accepted by nobody in the new team, so it does not enter the run queue by itself.
- **Not copied:** everything else. That means runs, bundles, findings, transcripts,
  dependencies, links, branch, strategy, model, effort, `review_instructions`,
  `review_config`, assignee, archive state and `source` (ADR-0029 point 5).

The source task is unchanged. Copying within one team is allowed, and is simply a duplicate.

The picker needs another team's repositories, which `list_repositories` cannot give while the
request is narrowed to the current team. So `list_copy_targets(ctx) -> Vec<CopyTarget {
team_id, team_name, repositories: Vec<{ id, name }> }>` lists every team in scope with its
repositories, personal team first, then by name.

**10. A solo board refuses team management.** Each command in points 2, 5, 6, 7 and 9 that
writes, plus `preview_invitation`, is refused with `invalid`: `a solo board has one team:
teams need a Rimaia server`. The test is whether a `solo_identity` row exists, which is
D28's own discriminator. ADR-0030 point 7 says nothing in the identity record is reachable in
solo, and ADR-0029 point 2 says solo never shows teams. A second team on a solo board would
fall outside the shell's `TeamScope::one` and disappear. `list_team_members` and
`list_copy_targets` still answer in solo, with its one member and its one team.

**11. Doors.** Every command above is a board row in `crates/core/src/api/registry.rs`, with
its handler in a new `crates/core/src/api/board/teams.rs` (D32 point 1):

| Command | Effect | MCP tool |
| --- | --- | --- |
| `create_team` | Write | yes |
| `list_team_members` | Read | yes |
| `set_member_role` | Write | yes |
| `remove_member` | Write | yes |
| `leave_team` | Write | yes |
| `create_invitation` | Write | yes |
| `list_invitations` | Read | yes |
| `revoke_invitation` | Write | yes |
| `preview_invitation` | Read | yes |
| `accept_invitation` | Write | yes |
| `list_copy_targets` | Read | yes |
| `copy_task_to_team` | Write | yes |
| `delete_team` | Write | **no** |
| `delete_account` | Write | **no** |

- **Every row ignores `Rimaia-Team`** (050 Scope 3). Each names its team by id, acts on the
  person, or carries a token, so the header has nothing to narrow, and narrowing would break
  three of them: the copy's target repository, the picker's other teams, and the account
  page's read of the personal team's members are all outside the current team.
  **These fourteen rows are added to 050's `api::registry::ignores_team_header`**, beside
  the thirteen 050 put there (`list_teams`, `list_runners`, `unpair_runner`, 047's eight
  account rows, `get_subscription_cost` and `set_subscription_cost`), which stay. The
  function is not re-created and its existing rows are not touched; the server's `Caller`
  extractor already asks it before calling `narrow_to`. Their scope is then
  `Caller.teams` as `Authenticate` read it on this request: every membership on a board
  route, and a restricted token's intersection on `/mcp` (060). Ignoring the header never
  widens a credential.
- **HTTP.** The route for each row comes from the registry, with no route code of its own
  (D32 point 3).
- **The invitation page's route.** `crates/server` serves `index.html` for `GET /invite`
  exactly, with 050 Scope 1's `Cache-Control: no-cache`, `X-Content-Type-Options: nosniff`
  and `Content-Security-Policy: frame-ancestors 'none'`. It is the first deep link, and the
  only one: every other path, `/invite/<anything>` included, stays a `404`.
- **Frontend wrappers.** Each row gets a `board<T>` wrapper in `src/lib/commands.ts`, and
  `src/types.ts` gains `Role`, `TeamMember`, `Invitation`, `InvitationStatus`,
  `InvitationPreview`, `CreatedInvitation` and `CopyTarget`.
- **MCP tools.** Tools use MCP's snake_case arguments (D16.1). Every new tool is
  `RunAccess::Refused` in the run-scope table (`crates/core/src/mcp/scope.rs`, as it stands
  at 050's tip). A run must never invite, admit, remove or copy people or work (ADR-0012).
- **The two deletions have no tool.** They follow ADR-0021 point 5's `delete_task`
  reasoning: each is irreversible and could be done by an agent by mistake. Parity does not
  overturn a decision about destructiveness.
- **Listing the caller's teams is not added here.** 050 added the command behind the
  switcher, and this task only widens its entry. ADR-0035 point 2's `list_teams` tool is
  060's.

**12. The interface.** It is rendered wherever 050 renders the team switcher, and never in
solo. Components use the calm vocabulary of ADR-0024.

- **The switcher** gains **New team…**, a name field that creates the team and switches to
  it.
- **Settings gains a Team section** (`src/views/settings/TeamSection.tsx`):
  - the member list, with each member's role;
  - for owners: a role control and **Remove** on each member;
  - **Leave team** for everyone, disabled with the rule's sentence when the rule would
    refuse it;
  - for owners, invitations: a role choice and **Create link**. The link appears once, with
    a copy button and the sentence "This link is shown once and works for seven days".
    Pending invitations are listed with **Revoke**, and the others are folded away as a
    history;
  - for owners of a non-personal team, a **Delete team** area. Its button is enabled only
    while the typed text equals the team's name. The area states what goes (every task,
    run, transcript and repository record) and what stays (each runner's local worktrees,
    per ADR-0029 point 6).
- **The accept page** (`src/views/InviteView.tsx`). `App.tsx` routes on
  `location.pathname === '/invite'`. It moves the fragment's token into `sessionStorage`
  and drops the fragment from the address bar with `history.replaceState`, before any
  request. A visitor who is not signed in gets 050's sign-in screen; 047's callback always
  lands on `/`, and the app, finding a kept token, opens `InviteView` there. The page calls
  `preview_invitation` and shows the team, the role, the inviter and the expiry with
  **Join team**. The token is removed from storage the moment it is used or refused. An
  invalid link shows the service's sentence and nothing else. After joining, the switcher
  selects the new team.
- **The account page (050)** gains `src/views/account/DeleteAccountSection.tsx`. It lists
  the teams that block deletion (`owner_count` of 1, not personal) and the members of the
  personal team who will lose it. It enables **Delete account** only while the typed text
  equals the login.
- **The task panel** gains **Copy to team…** (`src/components/panel/CopyToTeamSection.tsx`)
  when the caller belongs to more than one team. It offers a team, then a repository in that
  team, both from `list_copy_targets`, and says the copy brings the plan and nothing else.
- **On `not_found` for the selected team**, which is what a removed member or a deleted
  team's member gets next, the switcher reloads the list and selects the personal team.
- **Fixture rows for all fourteen commands** in `src/dev/fixtures/`, writes included,
  because 028's coverage test fails on any wrapper `commands.ts` sends without a row. The
  reads answer from the seed. The writes answer without changing it (028 Scope 2):
  `create_team` and `accept_invitation` return a `Team` that the seed's `list_teams` does
  not gain, `create_invitation` returns a fixed token, and the rest return their success
  value. A row that answers with a refusal instead uses a real code and the service's exact
  sentence (028). `list_teams`' existing row gains `member_count` and `owner_count`
  (point 2), with one team whose `owner_count` is 1 so the account page's blocking list has
  something to show.
- **Screenshots.** Scenarios in `screenshots/views.shot.ts` for the Team section as owner and
  as member, the accept page, and the delete confirmations (028).

**13. The isolation suites.** Every new command gets a case in the cross-team registry
tests: 039's `crates/core/tests/tenant_isolation.rs` and 046's
`crates/server/tests/commands.rs`, however 046 left them keyed. Every new tool gets a case in
the MCP half.

- **The token-bearing commands**, `preview_invitation` and `accept_invitation`, have no team
  id to substitute, and a valid team-B token rightly reaches team B. 046's `Foreign` gains
  `Capability(fn(&TwoTeams) -> Vec<Value>)`: arguments carrying team B's expired, revoked
  and accepted tokens. Run as team A, each answer must contain nothing of team B (check 1)
  and equal the answer for a never-issued token (check 2), and team B's rows must be
  unchanged (check 3).
- **The id-less rows**, `create_team`, `list_copy_targets` and `delete_account`, are
  `EntityLess` and get checks 1, 3 and 4.
- **Check 3's snapshot** gains `teams`, `team_memberships`, `invitations` and
  `api_token_teams`.

**14. Records.**

- CLAUDE.md's "must have tests" list gains **roles, the last-owner rule and invitation
  redemption**, beside the tenant isolation that 039 added.
- The decisions this task makes that no ADR or seam entry records are appended to
  `docs/seam-contract.md` as the next free `D` entry, "Task 051's cross-cutting choices", in
  the four-part shape D26 and D27 use:
  - the `rmi_` prefix outside `TokenKind`, and the fragment link;
  - `/invite` as the one deep link, and the token kept in `sessionStorage` across sign-in;
  - one answer for every unusable token;
  - `BEGIN IMMEDIATE` and write-then-count for the last-owner rule;
  - the personal-team rules;
  - what removal withdraws, including trust and restricted tokens, and the task events it
    publishes;
  - **the obligation on 053**: its expiry sweep does not pin a task to a runner whose owner
    is not a member of the task's team;
  - the fourteen rows added to 050's `ignores_team_header`, and why ignoring the header
    never widens a credential;
  - the running-work refusal inside `purge`, and expired leases blocking account deletion;
  - the restricted-token revocation on team deletion;
  - the solo refusal;
  - `Change::Teams`, its wire name `teams:changed` in core's board function and in
    `board_reread()`, its `EVENT_SOURCES` row, and the `revalidate` call sites;
  - which fields a copy carries.

  Its Binds line names 053, 054, 056, 060 and 061.
- The entry is added to the "How to use this" table's rows for 051, 053 and 060, creating
  each row if it is absent. 051's own row lists what it reads: D3 · D4 · D6 · D7 · D8 ·
  D10 · D16 · D28 · D32 · D33 · D34, the entries 045, 047, 048 and 050 appended, and the
  new entry.

## Out of scope

- **Renaming a team, transferring a personal team, and finer roles** (guests,
  per-repository permissions). All are additive (ADR-0029 point 3).
- **Email delivery of invitations.** ADR-0029 rejects it for now. The link is copied by hand.
- **Moving a task between teams.** Refused by 039. A copy is the only path (ADR-0029
  point 5).
- **The `team` argument on MCP tools, `list_teams`, and the hosted `/mcp` door.** All are
  060's (ADR-0035 point 2).
- **Assignment and consent UI**, including what an unassigned task looks like after a
  removal. That is 061's.
- **Deleting transcript files and review patches held outside the database.** They do not
  exist on a server until 056, and 056 extends `purge` to delete them.
- **Backups.** Deleted data leaves the Litestream replica when its retention passes
  (ADR-0037 point 6, task 062). Nothing here touches the replica.
- **Telling runners to delete anything.** Their worktrees are their owners' files
  (ADR-0029 point 6).
- **Any dependency** beyond D34's list, and **any other migration** (D4, D28's amendment).

## Acceptance criteria

- `src-tauri/migrations/20261003120400_invitations.sql` exists under exactly that name with
  D28's DDL for it, and it is the only new migration. Both offline caches are regenerated
  with D33 point 3's recipe and committed.
  `SQLX_OFFLINE=true cargo check --workspace --all-targets` passes.
- These core tests exist under these names and pass. Each runs against the in-memory test
  pool or real files in a `TempDir`, with a faked clock, no `sleep`, and a server-shaped
  board (no `solo_identity`) built through 038's team-creation service, 039's `TwoTeams`
  and 045's member helper, never through hand-written `INSERT`s into `teams` or
  `team_memberships`:
  - `a_new_team_has_its_creator_as_its_only_owner`, including its seeded
    `base_instructions` and its trimmed name. An empty name and a 101-character one are
    `invalid`. Sign-up's personal team stays covered by 047's
    `a_first_sign_in_creates_a_user_a_personal_team_and_an_owner_membership`;
  - `an_invitation_admits_any_signed_in_account_once`: a second account using the same token
    gets the unusable-token answer, and `team_memberships` holds one new row;
  - `the_member_gets_the_role_the_invitation_names`, for both roles;
  - `an_invitation_expires_seven_days_after_it_was_made`: accepted at `expires_at` minus one
    second, and refused at exactly `expires_at`, by advancing the fake clock;
  - `a_revoked_invitation_admits_nobody`;
  - `every_unusable_invitation_is_answered_the_same_way`: unknown, expired, revoked and
    accepted tokens give byte-equal errors from both `preview_invitation` and
    `accept_invitation`;
  - `only_the_hash_of_an_invitation_token_is_stored`: no column of any row contains the
    token or its secret part, and `token_hash` is 047's `hash` of the token;
  - `accepting_while_already_a_member_changes_nothing_and_keeps_the_invitation`: the role is
    unchanged, and the invitation is still `Pending` afterwards;
  - `a_member_cannot_do_what_only_an_owner_can`: a case table with one named row each for
    `update_repository`, `remove_repository`, `set_repository_unattended_ceiling`,
    `set_repository_review_config`, `set_base_instructions`, `set_strategy_catalogue`,
    `set_strategy_defaults`, `set_strategy_approval`, `set_review_settings`,
    `set_member_role`, `remove_member`, `create_invitation`, `list_invitations`,
    `revoke_invitation` and `delete_team`, dispatched through `api::dispatch` with 046's
    `FixedCaller` as a member, plus `register_repository` through `repo::register` with a
    member-scoped `ServiceContext`. Each answer is `invalid` with the exact sentence for
    that action. The team's rows are unchanged, and no change event is published. The same
    call as an owner succeeds. `set_task_review` as a member succeeds;
  - `a_demotion_takes_effect_on_the_next_request`: demoted between two calls, the second
    owner-only call is refused;
  - `the_last_owner_cannot_leave_be_removed_or_be_demoted`, with the exact sentence and no
    row changed in each case;
  - `an_owner_can_leave_once_another_owner_exists`;
  - `a_personal_team_keeps_the_person_it_belongs_to`: removal, demotion, leaving and
    `delete_team` are each refused with their exact sentence, even after a second owner is
    invited in;
  - `removing_a_member_withdraws_their_runners_from_the_teams_work`: their tasks are
    unassigned, their pins are cleared, their pool row is gone, and their leases on the
    team's tasks expire at the fake clock's now. One `tasks` event names exactly the changed
    tasks. Their assignments, pins and leases in another team are untouched;
  - `a_member_invited_back_starts_without_trust_or_restricted_tokens`: trust rows in both
    directions are gone, a token restricted to the team alone is gone, and a token
    restricted to it and another team keeps only the other;
  - `deleting_a_team_needs_its_exact_name`, where a name that differs only in case is
    refused;
  - `deleting_a_team_removes_everything_it_owned`. The team is filled with tasks in every
    column, a dependency edge, links, runs of every kind with bundles and findings,
    acceptances, trust rows, invitations and every team setting. Afterwards no row in any
    board table references the team or its tasks, `pragma_foreign_key_check` is empty, and
    the other team's rows are byte-identical to a snapshot taken before;
  - `deleting_a_team_does_not_widen_a_token_restricted_to_it`: a token restricted to that
    team alone is gone, and a token restricted to it and another team keeps only the other;
  - `a_team_with_running_work_cannot_be_deleted`, while an expired lease does not block;
  - `deleting_an_account_deletes_its_personal_team_and_leaves_its_other_teams`: the other
    team's tasks remain, with `created_by` NULL, one `tasks` event names them, and the
    user's memberships, sessions, tokens and runners are gone;
  - `an_account_that_is_a_teams_only_owner_cannot_be_deleted`, naming the team;
  - `an_account_whose_runner_holds_work_cannot_be_deleted`, for an unexpired and for an
    expired lease on another team's task;
  - `an_account_whose_personal_team_has_running_work_cannot_be_deleted`, where the lease is
    held by another member's runner, and the lease row is unchanged;
  - `copying_a_task_to_another_team_brings_the_plan_and_nothing_else`. It asserts every
    column of the new row: title, plan and extra instructions are equal; the column is
    `not_ready`; the author is the copier; and every other field is its creation default.
    The source task, its runs and its edges are unchanged;
  - `a_task_cannot_be_copied_into_a_team_the_caller_is_not_in`: the answer equals the one a
    never-issued repository id gets;
  - `a_solo_board_refuses_team_management`: every command point 10 names is refused with
    the exact sentence, on a board built by `ensure_solo`, and writes nothing;
  - `team_changes_are_announced_to_the_team`: each write in point 8 publishes one
    `Change::Teams` whose `audience` is `Audience::Team(<its team>)`, a removal also
    publishes its `tasks` event, and a deletion also publishes the deleted task and
    repository ids;
  - 048's `every_change_maps_to_the_name_the_frontend_listens_for` is extended, not
    duplicated: `Change::Teams([t])` maps to exactly `teams:changed` with payload `["<t>"]`,
    and `board_reread()` is exactly five events, `teams:changed []` last;
  - `an_invitation_token_is_not_a_bearer_token`: an `rmi_` token presented to
    `Authenticate` is `unauthenticated` with 047's one message, and no table is read.
- **Doors.**
  - Each command in point 11's table is a board row, and `./scripts/check-command-wiring.sh`
    passes, including 048's step that refuses a wire event name as a literal under
    `src-tauri/src/`.
  - `the_team_header_is_ignored_by_exactly_the_person_scoped_rows`, a core test:
    `ignores_team_header` is true for exactly the twenty-seven names, 050's thirteen and
    this task's fourteen, and false for every other registry row. 050's
    `person_scoped_commands_ignore_the_team_header` still passes unchanged.
  - `a_new_stream_opens_with_one_wholesale_reread_of_each_board_entity` (048) passes with
    its expectation updated to the five events of `board_reread()`.
  - `creating_a_team_ends_the_creators_stream`: over 048's SSE endpoint, the creator's
    open stream without `Rimaia-Team` ends after `create_team`, and its reopening admits
    the new team's events.
  - `every_board_command_has_a_case`, `a_team_cannot_see_another_teams_ids` and
    `both_transports_answer_every_case_identically` pass with the new rows.
  - `a_removed_member_is_refused_on_their_next_request` passes over HTTP, with a real
    session for the removed member.
  - `a_task_is_copied_across_teams_whatever_team_the_request_names`: over HTTP, with
    `Rimaia-Team: <source team>`, the copy into the other team succeeds.
  - `the_invitation_page_is_the_app`: `GET /invite` returns `index.html` with 050's three
    headers, and `GET /invite/anything` is `404`.
  - `a_removed_member_stops_receiving_the_teams_events` and
    `a_deleted_teams_members_stop_receiving_its_events` pass over 048's SSE endpoint.
- **MCP.**
  - Each tool in point 11's table is registered, and has `RunAccess::Refused`.
  - `every_registered_tool_has_a_run_scope_decision` passes.
  - `team_management_is_refused_to_a_run` calls every new tool through a run-scoped handle
    and gets the scope refusal.
  - `the_operator_manages_a_team_over_mcp` creates a team, invites, previews, accepts,
    changes a role and copies a task. It builds two loopback MCP servers through the
    testing harness, one per actor's context, on a board without `solo_identity`, with a
    real client against each.
  - `Tool::from_name("delete_team")` and `Tool::from_name("delete_account")` are `None`.
- **The isolation suites have a case for every new command and tool**, as point 13 says.
- **Frontend tests** exist and pass. They use 049's HTTP test mock, or the invoke mock where
  the component is transport-agnostic:
  - `TeamSection.test.tsx`: an owner sees role controls, Remove and Create link. A member
    sees none of them. The created link is shown once and absent after a re-render from
    `list_invitations`. The Delete team button is disabled until the typed name matches
    exactly;
  - `InviteView.test.tsx`: the preview is shown, and Join calls `accept_invitation` with the
    fragment's token. The fragment is gone from `location` after the first render. An
    invalid link shows the service's sentence. A token kept across sign-in is used once and
    removed from `sessionStorage`;
  - `DeleteAccountSection.test.tsx`: blocking teams and the personal team's other members are
    listed, and the button is enabled only on the exact login;
  - `CopyToTeamSection.test.tsx`: it is absent with one team, and offers only the chosen
    team's repositories from `list_copy_targets`;
  - `events.test.ts` (extended): `EVENT_SOURCES` maps `teams:changed` to `board`, and
    `subscribeToTeamsChanged` subscribes through the board transport once;
  - 028's `fixtures.test.ts` passes with the fourteen new wrappers: "has an answer or an
    explicit refusal for every command commands.ts sends" finds a row for each, and "never
    reaches invoke or listen in fixture mode" calls each once;
  - none of the above renders in solo.
- `npm run screenshot` produces the scenarios in point 12. The PR notes that the images were
  inspected.
- CLAUDE.md's must-test list and the new seam entry (point 14) are committed.
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`, `cargo test
  -p rimaia-core` and the other crates' test steps as CI runs them, `cargo fmt --all
  --check`, the clippy steps as CI runs them, `cargo check --workspace --all-targets`, and
  `./scripts/check-command-wiring.sh`.
- **Needs a person, and the PR body carries it as a checklist.** Against a local
  `rimaia-server` with two GitHub accounts in two browsers:
  - the owner creates a team and sends a member link;
  - the second account accepts it after signing in from a signed-out tab;
  - the member is refused an owner action with the sentence;
  - the owner removes the member, whose open tab falls back to the personal team;
  - the owner deletes the team by typing its name.

## Notes

**Read first.**

- **Seam-contract D28:** 038's DDL for `teams`, `team_memberships`, `users` and
  `solo_identity`; this task's file under "`20261003120400_invitations.sql` — task 051"; the
  `RESTRICT` references and the ordered deletion in its Why; 045's authorship columns
  (`SET NULL`) and `trusted_authors`; and 047's `api_tokens` and `api_token_teams`, whose
  "no rows means every team" is the reason for point 6 step 2. The D4 amendment beside it
  names the file.
- **D32:** points 1–3 (rows, handlers, `dispatch`, no 403), point 7 (`Caller`, `TeamGrant`,
  `Caller::solo`, `FixedCaller`), point 9 (no MCP pairing is recorded), the appendix, and its
  Binds line: "051: role refusals are `invalid`, and team commands are board rows".
- **The entries 045, 047, 048 and 050 appended**, whatever numbers they took: 045's role
  check and member helper, 047's secrets and `Authenticate`, 048's `Audience`, wire table,
  `board_reread()` and `StreamControl`, 049's `EVENT_SOURCES`, and 050's `Rimaia-Team`
  header, `ignores_team_header` and person-scoped commands.
- **D3**, the typed settings accessor that 039 made per-placement and point 3 gates.
- **D33:** point 3's two-cache recipe, which binds 051.
- **D34:** the token crates are already admitted by 047.
- **Also:** D8 (no new `ErrorCode`: every refusal here is `invalid` or `not_found`), D10,
  D16.1, D7, and D4 and D6 as prohibitions.
- **ADRs:** ADR-0029 in full; ADR-0037 point 6; ADR-0030 points 2, 3 and 7; ADR-0021
  point 5; ADR-0031 point 4 (pins); ADR-0032 points 1 and 3 (why the base instructions are
  owner-only).

**Files to start from.** Created by earlier tasks in this chain:

- `crates/core/src/identity/` (038: `create_personal_team`, `Role`; 047: sign-up,
  `secret.rs`, sessions);
- `crates/core/src/api/registry.rs`, `api/caller.rs` (050's `narrow_to`) and
  `crates/core/src/api/board/` (046);
- `crates/core/src/testing/teams.rs` (039's `TwoTeams`), `testing/api.rs` (046's
  `BoardCase` and `Foreign`) and 045's member helper;
- `crates/core/tests/tenant_isolation.rs` (039) and `crates/server/tests/commands.rs` (046).

On `main` today: `crates/core/src/repo/mod.rs` (`register`, `update`, `remove`),
`crates/core/src/db/settings.rs` (039 turned it into per-placement accessors),
`crates/core/src/tasks/service.rs` (`create_task`, and `delete_task`'s refusal voice),
`crates/core/src/events.rs`, `crates/core/src/mcp/scope.rs`, `mcp/server.rs`,
`mcp/requests.rs` and `mcp/responses.rs`, `crates/core/tests/mcp_scope.rs` (the run-scope
refusal tests to copy), `src/lib/commands.ts`, `src/lib/events.ts`, `src/types.ts`,
`src/dev/fixtures/`, `src/views/settings/` and `src/components/panel/`. 048 created
`crates/core/src/api/events.rs`, where `teams:changed` goes; `src-tauri/src/lib.rs` is not
touched for it.
050's switcher, `App.tsx` routing and `src/views/account/` are wherever 050 put them.
Follow its diff.

**Migration.** `src-tauri/migrations/20261003120400_invitations.sql`. It sorts after 047's
`20261003120300_identity.sql` and before 054's `20261003120500_repositories_by_remote.sql`,
and it is frozen once this task lands on the branch.

**What the chain provides.**

- **038:** teams, users, memberships, `Role`, and the team-creation helper.
- **039:** scope filtering in every service, `TwoTeams`, and the team-placement settings
  accessor that point 3 gates.
- **043:** `runner_leases`, pins, and the `BEGIN IMMEDIATE` rule.
- **045:** assignment, authorship, acceptances, the trust list, `runner_pool_teams`, the
  team-ceiling command with the first role check, and the member helper.
- **046:** the registry, `dispatch`, `Caller`, `FixedCaller`, `Foreign`, and the HTTP
  contract tests.
- **047:** sign-up with a personal team, sessions, `secret.rs`, `api_token_teams`, and a
  sign-in callback that always lands on `/`.
- **048:** the SSE fan-out and `StreamControl::revalidate`.
- **049:** transports, capabilities, and the HTTP test mock.
- **050:** the switcher, `list_teams`, the `Rimaia-Team` header and `narrow_to`, the bundle
  routes and their headers, and the account page.

If a name differs in those diffs, follow the diff. If something this task relies on is
missing, stop and ask. It is not 051's to improvise.

**What the next tasks expect.**

- **052:** the runner caller's teams come from memberships, so a removed member's runner
  stops reaching the team.
- **053:** the pin condition in point 4, which the seam entry binds it to, with a test.
- **054:** `repo::register`'s owner check, kept in the board half.
- **056:** extends `purge` to transcript files and stored patches.
- **060:** adds the `team` argument and `list_teams`. The twelve tools here take the team by
  id or a token, or list every team in scope, and need nothing more. 060 leaves
  `create_invitation`, `accept_invitation` and `set_member_role` off the hosted endpoint
  (ADR-0030 point 3); they keep their tools on the embedded one.
- **061:** shows unassigned tasks after a removal, from the `tasks` event point 4 publishes.
- **062:** counts storage per team, and a deleted team drops out of those counts.

**Size.** L, at the upper edge of one session. Roughly:

| Part | Lines |
| --- | --- |
| Services, and the role gate on existing writes | ~1,000 |
| Registry rows, handlers, MCP tools and the `/invite` route | ~500 |
| Core and server tests | ~1,100 |
| Isolation cases | ~250 |
| Interface, its fixture rows and its tests | ~1,100 |
| Total | ~3,950 |

If it runs over, cut here: **ship points 1–11, 13 and 14 with every core, door and MCP
test, and move point 12 (the interface, its frontend tests and screenshots) into the first
commit of 061.** Amend 061's task file in the same commit and say so in the PR. **The
fourteen fixture rows stay in 051 even then**, because point 11's wrappers would otherwise
fail CI: 028's coverage test reads every wrapper `commands.ts` sends. So does point 8's
`teams:changed` subscription and its `EVENT_SOURCES` row, which is not part of point 12.
Neither is more than a few dozen lines. The event wiring in core, the fixture rows and the
header test add roughly 100 lines to the estimate, and do not move the task off L. The
services are complete and reachable over HTTP and MCP without the screens. The screens
cannot exist without the services. The `/invite` route stays in 051 either way: it is a
server door.
