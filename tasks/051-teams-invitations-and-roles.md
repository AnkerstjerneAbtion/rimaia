---
id: "051"
title: Teams, invitations and roles
milestone: v0.5
status: ready
depends_on: ["050"]
adrs: ["0029", "0037"]
size: M
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
  the way 038's `create_personal_team` does. It goes through the same internal helper, so
  the two cannot drift. A name that is empty after trimming is `invalid`. Names need not be
  unique: uniqueness would reveal that another team exists with that name.
- `list_team_members(ctx, team_id) -> Vec<TeamMember>`, readable by any member. A
  `TeamMember` has `user_id`, `login`, `avatar_url`, `role`, `joined_at` and
  `is_personal_owner`.
- `set_member_role(ctx, team_id, user_id, role)`, `remove_member(ctx, team_id, user_id)` and
  `leave_team(ctx, team_id)`. The first two are owner-only. Leaving is open to every member,
  owners included.
- `Team` gains, wherever 050's team list returns it, `role` (the caller's role),
  `personal` (a bool), `member_count` and `owner_count`. The account page needs the last two
  to warn before a deletion, without a preview command of its own. If 050's list already
  returns some of these fields, keep 050's names.

**3. Roles are checked in the write's own transaction.** One helper,
`teams::require_owner(&mut tx, ctx, team_id, action)`. It reads `team_memberships` for
`ctx.actor` inside the transaction that then writes. It never trusts `Caller.teams`, which
was read at the start of the request (D32 point 7), because a demotion committed in between
must win. The checks run in a fixed order. Scope first, so a non-member gets `not_found`.
Role second, so a member gets `invalid` with D32 point 3's sentence naming the role. The
refusal is exactly `only an owner of this team can <action>`, where `<action>` is a verb
phrase fixed per call site, for example `invite people`, `change the team's base
instructions` or `register a repository`.

The owner-only actions are ADR-0029 point 3's list, and **each existing write in that list
gains the check in this task**:

- **Repositories:** registering one, removing one, `update_repository`'s team-held fields,
  and 045's team-ceiling command. `register_repository` is still a local row until 054
  (D32 point 8), so the check sits in `repo::register`, which every door calls.
- **Team settings:** every write through 039's team-placement accessor. That covers
  `base_instructions`, the strategy catalogue, defaults and approval,
  `strategy_default.<repository_id>`, `max_turns`, `disallowed_tools`, and 021's
  review-loop keys, including the team's `review_instructions`. Putting the check in the
  accessor means a key added later is owner-only without anyone remembering to make it so.
  A task's own `review_instructions` override is a task edit and stays open to members.
- **Membership, invitations and deleting the team** (points 2, 4 and 6).

The solo caller is its team's owner (`Caller::solo`, D32 point 7), so no solo behaviour
changes.

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

**Removing a member, or a member leaving, withdraws their machines from the team's work** in
the same transaction:

- their tasks in that team are unassigned: `assignee_id` and `assigned_by` are set to NULL
  where `assignee_id` is theirs;
- every `pinned_runner_id` on that team's tasks that names one of their runners is cleared;
- their `runner_pool_teams` row for that team is deleted;
- every `runner_leases` row their runners hold on that team's tasks gets `expires_at` set to
  now, from `ctx.clock`.

Without this, a task pinned to an ex-member's runner (ADR-0031 point 4) could never be
claimed again, because that runner can no longer reach the team.

**5. Invitations** (`teams/invitations.rs`):

- `create_invitation(ctx, team_id, role) -> CreatedInvitation { invitation, token }`, owner
  only.
  - The token is `rmi_` followed by a 256-bit secret, generated and hashed by the same
    functions 047 uses for `rmd_`/`rmr_`/`rmp_` tokens (`rand`, and a hex SHA-256 over the
    whole token, D34). The prefix lets a secret scanner recognise a leaked link, and lets
    047's bearer extractor refuse an invitation pasted as an API token, by prefix and without
    a lookup.
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
only, runs in one transaction, in this order:

1. **Refusals.**
   - `confirm_name` must equal the team's name exactly. Otherwise `invalid`: `type the
     team's name exactly to delete it`. The UI also enforces this, but the rule lives in
     the service, so every door applies it.
   - A personal team is refused (point 4).
   - A team with an unexpired lease on any of its tasks (`expires_at IS NULL OR expires_at
     > now`) is refused with `invalid`: `<n> of this team's tasks are running: wait for them
     or cancel them first`. A deleted lease cannot fence the runner still working under it.
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

After commit it publishes the deleted task and repository ids and a `Teams` change (point 8),
all under the deleted team's id. Its tracing span records the team id and the row counts. It
never records the team's name or any plan (ADR-0037 point 6).

A shared internal function does steps 2 and 3, and account deletion calls it too. The public
command is the only path that applies step 1's confirmation and personal-team refusal.

**7. Deleting an account.** `delete_account(ctx, confirm_login)`. It deletes `ctx.actor` and
nothing else. There is no id argument, so no door can name someone else. It runs in one
transaction:

1. **Refusals.**
   - `confirm_login` must equal the user's login exactly: `type your login exactly to delete
     your account`.
   - A user who is the only owner of any team other than their personal team is refused,
     with the teams named: `you are the only owner of <names>: make someone else an owner,
     or delete the team, first`. Deleting the account anyway would leave a team that nobody
     can administer or delete.
   - A user whose runner holds any `runner_leases` row is refused: `your runner <label> is
     still holding work: wait for it to be released`. `runner_leases.runner_id` is
     `RESTRICT`, so the cascade from `users` to `runners` would fail on that row anyway, and
     this sentence is the one the user actually reads.
2. **The personal team goes first**, through point 6's shared function.
   `teams.personal_user_id` is `RESTRICT`, which is why it cannot go after the user.
3. **The `users` row.** Sessions, tokens, pairing codes, runners, memberships,
   `user_settings`, acceptances and trust rows cascade. Authorship columns on other teams'
   tasks become NULL, meaning "a former member" (D28, 045).

A personal team that other people were invited into is deleted with it, as ADR-0029 point 6
says. The account page shows who will lose access before the person types their login.

**8. Change events.** `Change` gains `Teams(Arc<[TeamId]>)`, with a constructor
`ChangeEvent::teams(team_id)`. The event's `team_id` is the team, like every other event.
Creating a team, a membership or role change, an accepted invitation, a revocation and a
deletion each publish one after commit. The shell's `emit_change_event` maps it to a
`teams:changed` Tauri event, which solo never emits, because solo refuses team management
(point 10). 048's SSE mapping gains the same name. `src/lib/events.ts` gains the matching
subscription. Nothing else imports `listen` (D7).

**A removed member's streams stop at once.** ADR-0030 point 2 requires removal to take
effect on the member's next request. If 048's fan-out resolved a subscriber's teams once,
when the stream connected, this task makes it re-resolve on a `Teams` event for a team the
subscriber holds. After that, a removed member receives nothing more from that team.

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
  dependencies, links, branch, strategy, model, effort, `review_instructions`, assignee,
  archive state and `source` (ADR-0029 point 5).

The source task is unchanged. Copying within one team is allowed, and is simply a duplicate.

**10. A solo board refuses team management.** Each command in points 2, 5, 6, 7 and 9 that
writes, plus `preview_invitation`, is refused with `invalid`: `a solo board has one team:
teams need a Rimaia server`. The test is whether a `solo_identity` row exists, which is
D28's own discriminator. ADR-0030 point 7 says nothing in the identity record is reachable in
solo, and ADR-0029 point 2 says solo never shows teams. A second team on a solo board would
fall outside the shell's `TeamScope::one` and disappear. `list_team_members` still answers
in solo, with its one member.

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
| `copy_task_to_team` | Write | yes |
| `delete_team` | Write | **no** |
| `delete_account` | Write | **no** |

- **HTTP.** The route for each row comes from the registry, with no route code of its own
  (D32 point 3).
- **Frontend wrappers.** Each row gets a `board<T>` wrapper in `src/lib/commands.ts`, and
  `src/types.ts` gains `Role`, `TeamMember`, `Invitation`, `InvitationStatus`,
  `InvitationPreview` and `CreatedInvitation`.
- **MCP tools.** Tools use MCP's snake_case arguments (D16.1). Every new tool is
  `RunAccess::Refused` in the run-scope table (`crates/core/src/mcp/scope.rs`, as it stands
  after 035 and 055). A run must never invite, admit, remove or copy people or work
  (ADR-0012).
- **The two deletions have no tool.** They follow ADR-0021 point 5's `delete_task`
  reasoning: each is irreversible and could be done by an agent by mistake. Parity does not
  overturn a decision about destructiveness.
- **Listing the caller's teams is not added here.** 050 added the command behind the
  switcher, and this task only widens its `Team`. ADR-0035 point 2's `list_teams` tool is
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
- **The accept page** (`src/views/InviteView.tsx`) is served for `/invite`. It reads the
  fragment, calls `preview_invitation`, and shows the team, the role, the inviter and the
  expiry with **Join team**. For a visitor who is not signed in, the token is kept in
  `sessionStorage` across 050's sign-in redirect, because the OAuth round trip drops the
  fragment. It is removed from storage the moment it is used or refused. An invalid link
  shows the service's sentence and nothing else.
- **The account page (050)** gains **Delete account**. It lists the teams that block
  deletion (`owner_count` of 1, not personal) and the members of the personal team who will
  lose it. It enables the button only while the typed text equals the login.
- **The task panel** gains **Copy to team…** (`src/components/panel/CopyToTeamSection.tsx`)
  when the caller belongs to more than one team. It offers a team, then a repository in that
  team, and says the copy brings the plan and nothing else.
- **On `not_found` for the selected team**, which is what a removed member or a deleted
  team's member gets next, the switcher reloads the list and selects the personal team.
- **Screenshots.** Fixture rows in `src/dev/fixtures/` for every new read, and scenarios in
  `screenshots/views.shot.ts` for the Team section as owner and as member, the accept page,
  and the delete confirmations (028).

**13. The isolation suites.** Every new command gets a case in the cross-team registry
tests: 039's `crates/core/tests/tenant_isolation.rs` and 046's
`crates/server/tests/commands.rs`, however 046 left them keyed. Every new tool gets a case in
the MCP half. The token-bearing commands, `preview_invitation` and `accept_invitation`, have
no team id to substitute. Their point-2 check compares a never-issued token with an expired,
a revoked and an accepted one, and all four answers must be equal.

**14. Records.**

- CLAUDE.md's "must have tests" list gains **roles, the last-owner rule and invitation
  redemption**, beside the tenant isolation that 039 added.
- The decisions this task makes that no ADR or seam entry records are appended to
  `docs/seam-contract.md` as the next free `D` entry, "Task 051's cross-cutting choices", in
  the four-part shape D26 and D27 use:
  - the `rmi_` prefix and the fragment link;
  - one answer for every unusable token;
  - write-then-count for the last-owner rule;
  - the personal-team rules;
  - what removal withdraws;
  - the restricted-token revocation on team deletion;
  - the solo refusal;
  - `Change::Teams`;
  - which fields a copy carries.
- The entry is also added to the "How to use this" table's rows for 051, 053 and 060.

## Out of scope

- **Renaming a team, transferring a personal team, and finer roles** (guests,
  per-repository permissions). All are additive (ADR-0029 point 3).
- **Email delivery of invitations.** ADR-0029 rejects it for now. The link is copied by hand.
- **Moving a task between teams.** Refused by 039. A copy is the only path (ADR-0029
  point 5).
- **The `team` argument on MCP tools, and `list_teams`.** Both are 060's (ADR-0035 point 2).
- **Assignment and consent UI**, including what an unassigned task looks like after a
  removal. That is 061's.
- **Deleting transcript files and review patches held outside the database.** They do not
  exist on a server until 056, and 056 extends point 6's shared function to delete them.
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
  board (no `solo_identity`) built through 038's team-creation service and 039's
  `TwoTeams`, never through hand-written `INSERT`s into `teams` or `team_memberships`:
  - `a_new_team_has_its_creator_as_its_only_owner`, including its seeded `base_instructions`;
  - `signing_up_creates_a_personal_team_with_its_owner_alone`, through 047's sign-up
    service. If 047 already has this test, it is kept and not duplicated;
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
    token or its secret part, and `token_hash` is the token's hex SHA-256;
  - `accepting_while_already_a_member_changes_nothing_and_keeps_the_invitation`: the role is
    unchanged, and the invitation is still `Pending` afterwards;
  - `a_member_cannot_do_what_only_an_owner_can`: a table over every owner-only command
    (point 3's list, including the existing repository and team-settings writes),
    dispatched through `api::dispatch` with 046's `FixedCaller` as a member. Each answer is
    `invalid` with the exact sentence for that action. The team's rows are unchanged, and no
    change event is published. The same command as an owner succeeds;
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
    team's tasks expire at the fake clock's now. Their assignments, pins and leases in
    another team are untouched;
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
    team's tasks remain, with `created_by` NULL, and the user's memberships, sessions,
    tokens and runners are gone;
  - `an_account_that_is_a_teams_only_owner_cannot_be_deleted`, naming the team;
  - `an_account_whose_runner_holds_work_cannot_be_deleted`;
  - `copying_a_task_to_another_team_brings_the_plan_and_nothing_else`. It asserts every
    column of the new row: title, plan and extra instructions are equal; the column is
    `not_ready`; the author is the copier; and every other field is its creation default.
    The source task, its runs and its edges are unchanged;
  - `a_task_cannot_be_copied_into_a_team_the_caller_is_not_in`: the answer equals the one a
    never-issued repository id gets;
  - `a_solo_board_refuses_team_management`: every command point 10 names is refused with
    the exact sentence, on a board built by `ensure_solo`, and writes nothing;
  - `team_changes_are_announced_to_the_team`: each write in point 8 publishes one
    `Change::Teams` naming its team. A deletion also publishes the deleted task and
    repository ids.
- **Doors.**
  - Each command in point 11's table is a board row, and `./scripts/check-command-wiring.sh`
    passes.
  - `every_board_command_has_a_case`, `a_team_cannot_see_another_teams_ids` and
    `both_transports_answer_every_case_identically` pass with the new rows.
  - `a_removed_member_is_refused_on_their_next_request` passes over HTTP, with a real
    session for the removed member.
  - `a_removed_member_stops_receiving_the_teams_events` passes over 048's SSE endpoint.
- **MCP.**
  - Each tool in point 11's table is registered, and has `RunAccess::Refused`.
  - `every_registered_tool_has_a_run_scope_decision` passes.
  - `team_management_is_refused_to_a_run` calls every new tool through a run-scoped handle
    and gets the scope refusal.
  - `the_operator_manages_a_team_over_mcp` creates a team, invites, previews, accepts,
    changes a role and copies a task, over a real loopback client against a server-shaped
    board.
  - `Tool::from_name("delete_team")` and `Tool::from_name("delete_account")` are `None`.
- **The isolation suites have a case for every new command and tool.** The token cases
  follow point 13.
- **Frontend tests** exist and pass. They use 049's HTTP test mock, or the invoke mock where
  the component is transport-agnostic:
  - `TeamSection.test.tsx`: an owner sees role controls, Remove and Create link. A member
    sees none of them. The created link is shown once and absent after a re-render from
    `list_invitations`. The Delete team button is disabled until the typed name matches
    exactly;
  - `InviteView.test.tsx`: the preview is shown, and Join calls `accept_invitation` with the
    fragment's token. An invalid link shows the service's sentence. A token kept across
    sign-in is used once and removed from `sessionStorage`;
  - the account page's delete test: blocking teams are listed, and the button is enabled only
    on the exact login;
  - `CopyToTeamSection.test.tsx`: it is absent with one team, and offers only the chosen
    team's repositories;
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
  (`SET NULL`); and 047's `api_tokens` and `api_token_teams`, whose "no rows means every
  team" is the reason for point 6 step 2. The D4 amendment beside it names the file.
- **D32:** points 1–3 (rows, handlers, `dispatch`, no 403), point 7 (`Caller`, `TeamGrant`,
  `Caller::solo`, `FixedCaller`), point 9 (no MCP pairing is recorded), and its Binds line:
  "051: role refusals are `invalid`, and team commands are board rows".
- **D33:** point 3's two-cache recipe, which binds 051.
- **D34:** the token crates are already admitted by 047.
- **Also:** D8 (no new `ErrorCode`: every refusal here is `invalid` or `not_found`), D10,
  D16.1, D7, and D4 and D6 as prohibitions.
- **ADRs:** ADR-0029 in full; ADR-0037 point 6; ADR-0030 points 2, 3 and 7; ADR-0021
  point 5; ADR-0031 point 4 (pins); ADR-0032 points 1 and 3 (why the base instructions are
  owner-only).

**Files to start from.** Created by earlier tasks in this chain:

- `crates/core/src/identity/` (038: `create_personal_team`, `Role`; 047: sign-up, tokens,
  sessions);
- `crates/core/src/api/registry.rs` and `crates/core/src/api/board/` (046);
- `crates/core/src/testing/teams.rs` (039's `TwoTeams`);
- `crates/core/tests/tenant_isolation.rs` (039) and `crates/server/tests/commands.rs` (046).

On `main` today: `crates/core/src/repo/mod.rs` (`register`, `update`, `remove`),
`crates/core/src/db/settings.rs` (039 turned it into per-placement accessors),
`crates/core/src/tasks/service.rs` (`create_task`, and `delete_task`'s refusal voice),
`crates/core/src/events.rs`, `crates/core/src/mcp/scope.rs`, `mcp/server.rs`,
`mcp/requests.rs` and `mcp/responses.rs`, `crates/core/tests/mcp_scope.rs` (the run-scope
refusal tests to copy), `src-tauri/src/lib.rs` (`emit_change_event`), `src/lib/commands.ts`,
`src/lib/events.ts`, `src/types.ts`, `src/views/settings/` and `src/components/panel/`.
050's switcher and account page are wherever 050 put them. Follow its diff.

**Migration.** `src-tauri/migrations/20261003120400_invitations.sql`. It sorts after 047's
`20261003120300_identity.sql` and before 054's `20261003120500_repositories_by_remote.sql`,
and it is frozen once this task lands on the branch.

**What the chain provides.**

- **038:** teams, users, memberships, `Role`, and the team-creation helper.
- **039:** scope filtering in every service, `TwoTeams`, and the team-placement settings
  accessor that point 3 gates.
- **043:** `runner_leases` and pins.
- **045:** assignment, authorship, acceptances, the trust list, `runner_pool_teams`, and the
  team-ceiling command.
- **046:** the registry, `dispatch`, `Caller`, `FixedCaller`, and the HTTP contract tests.
- **047:** sign-up with a personal team, sessions, hashed tokens, `api_token_teams`, and the
  token generator.
- **048:** the SSE fan-out.
- **049:** transports, capabilities, and the HTTP test mock.
- **050:** the switcher, sign-in with a return path, and the account page.

If a name differs in those diffs, follow the diff. If something this task relies on is
missing, stop and ask. It is not 051's to improvise.

**What the next tasks expect.**

- **052:** the runner caller's teams come from memberships, so a removed member's runner
  stops reaching the team.
- **053:** its expiry sweep must not pin a task to a runner whose owner is no longer a
  member of the task's team. Point 4 expires those leases and relies on that. 053 adds the
  condition, with a test.
- **056:** extends point 6's shared deletion to transcript files and stored patches.
- **060:** adds the `team` argument and `list_teams`. The eleven tools here take the team by
  id and need nothing more.
- **061:** shows unassigned tasks after a removal.
- **062:** counts storage per team, and a deleted team drops out of those counts.

**Size.** This task is at the upper edge of one session. Roughly:

| Part | Lines |
| --- | --- |
| Services, and the role gate on existing writes | ~900 |
| Registry rows, handlers and MCP tools | ~450 |
| Core tests | ~1,000 |
| Isolation cases | ~250 |
| Interface and its tests | ~1,000 |
| Total | ~3,600 |

If it runs over, cut here: **ship points 1–11, 13 and 14 with every core, door and MCP
test, and move point 12 (the interface, its frontend tests and screenshots) into the first
commit of 061.** Amend 061's task file in the same commit and say so in the PR. The services
are complete and reachable over HTTP and MCP without the screens. The screens cannot exist
without the services.
