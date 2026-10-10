---
id: "039"
title: Every board service filters by team
milestone: v0.5
status: ready
depends_on: ["038"]
adrs: ["0029", "0021", "0028", "0030", "0035"]
size: L
---

# Every board service filters by team

## Goal

Make [ADR-0029](../docs/adr/0029-teams-membership-and-isolation.md) point 5 true of every
line of `rimaia-core` that reads or writes the board. Task 038 gives `ServiceContext` a
required team scope and puts `team_id` on `repositories` and `tasks`. This task makes every
service use that scope.

- Every read returns only rows from the context's teams.
- Every write refuses an id outside them, **exactly as it refuses an id that was never
  issued**: `not_found`, never `forbidden`, and with the same message once the id is
  substituted.
- Every reference that crosses a team is refused at write time.

Then prove it. A registry test calls every MCP tool as team A, against a board that also
holds team B, and does the same for every tool a run-scoped handle is granted. It fails when
someone adds a tool without a cross-team case, the same way
`every_registered_tool_has_a_run_scope_decision` fails when a tool has no run-scope decision
(ADR-0021 point 3). The Tauri-command half of the registry is 046's, where D32's
`api::registry` enumerates the commands for free (see Size).

**No behaviour changes in solo.** There is one team, and every existing test keeps passing.
The only changes allowed in those tests are the mechanical ones a signature change forces.

## Why now

038 and 039 ship in the same release (seam-contract D28 part 4), for two reasons.

- **038 copied the team settings into `team_settings` and moved no reader or writer.** After
  038, readers and writers both still use `settings`, and `team_settings` is a snapshot
  nobody writes. It goes stale the first time a setting changes, and stays stale until 039
  moves readers and writers together.
- **Every task after this one assumes a service cannot leak.** 040–045 move state and rules
  across the board/runner seam, and 046 puts the board on HTTP. Isolation added after the
  first route exists means an audit of every route. Added now, it is a property of the
  service layer that each route inherits.

Solo is the cheapest place to do it. The filter compares against one constant, and a missed
filter fails a test, not a customer (ADR-0029's consequences).

## Scope

**Every service filters by scope.** This covers every function in `tasks/`, `repo/`,
`runs/`, `archive/`, `worktree/`, `strategy/`, `analytics/`, `scheduler/`, `schedule/`,
`runner/` and `mcp/` that reads a board table, plus whatever 033–038 added:

- the review bundle (033);
- the review actions, `dependents_of` and the overnight digest (034);
- the findings (035);
- the board port's in-process adapter (036);
- the review loop's write-back (021).

The rules:

- **A lookup by id is scoped in its query.** Put `team_id` in the `WHERE` of `tasks` and
  `repositories`. `runs`, `task_links`, `task_dependencies`, `review_bundles` and
  `review_findings` inherit their team through `tasks` (ADR-0029 point 1), so their lookups
  join to it. Never fetch first and compare in Rust: a fetch followed by a check is two code
  paths, and only one of them gets tested.
- **The id determines the team when a call names an entity.** ADR-0035 point 2 applies here
  too. A task, run, link or finding id outside the scope is `Error::NotFound`, with the same
  message a missing id gets. An error variant specific to the scope, or a message naming a
  team, would be a new way to probe ids, and D8 says the error type does not grow.
- **An entity-less call aggregates, acts on the actor, or acts on one team.** Three kinds:
  - **Aggregating reads span every team in scope.** These are `get_analytics` and
    `get_run_cost_summary` ("over the caller's teams' runs", D32's appendix).
  - **User settings act on `ctx.actor`, whatever the scope.** `get_subscription_cost`
    reads the actor's `user_settings` row under any scope. `set_subscription_cost` writes
    it, but its change event still needs a team until 048 (below), so it calls
    `ctx.scope.sole()?` **before** it writes and is refused under a two-team context. Its
    comment names 048's `Audience::User` as the end of that refusal. 034's
    `review::mark_seen` is the same case and gets the same treatment (see "034's digest
    marker" below).
  - **Everything else acts on exactly one team:** `list_tasks`, `list_repositories`,
    `register_repository`, 034's `review::digest`, the team settings, machine-state writes,
    and the queue's selection. The team is `ctx.scope.sole()?`, 038's `TeamScope::sole() ->
    Result<&TeamId>`. A context that reaches more than one team is refused `invalid`. That
    is ADR-0035 point 2's refusal. 060 adds the `team` argument that avoids it on MCP, and
    050 adds the team switcher. 039 adds neither.
  - **039 refines `sole()`'s message**, which 038 left unspecified, to list the reachable
    teams by id, in the scope's sorted order: `this needs one team, and the request reaches
    2: <id-a>, <id-b>`. Ids, not names: `TeamScope` holds ids only, `sole()` is synchronous
    and reads nothing, and 038's placeholder names every personal team `Personal`, so a name
    would not tell two teams apart. The scope is the caller's own teams, so the ids reveal
    nothing to it. 038's `a_scope_of_two_teams_has_no_sole_team` keeps passing.
- **Cross-team references are refused at write time** (ADR-0029 point 5). Every case below
  uses a context that reaches *both* teams, so the id is visible and only the rule can
  refuse it:
  - **A dependency edge between teams.** It is already refused as cross-repository
    (`tasks/dependencies.rs:78`). Keep that message, and add a test that it holds when both
    tasks are visible.
  - **A task reassigned to another team's repository.** This extends D13's guard in
    `tasks::update_task`. A target repository outside the scope is `NotFound`, like a
    never-issued one. A target inside the scope but in another team than the task's is
    refused `invalid` with "a task cannot move to another team; copy it instead", checked
    before D13's worktree-and-runs guard because it holds whatever the task's history is.
    ADR-0029 says a task does not move between teams. 038's composite foreign key on
    `(repository_id, team_id)` is the store's backstop, and this refusal is the service's.
  - **A task created in a repository takes the repository's team.** 038 Scope 7 already
    does this, and 038's `a_task_takes_its_repositorys_team` and
    `a_change_event_names_the_team_of_the_row_it_announces` already prove it under a
    two-team context. 039 only keeps them passing.
- **The cycle walk stays unscoped, and its refusal never names what the scope cannot see.**
  `tasks/dependencies.rs::load_edges` reads the whole `task_dependencies` table, and its
  comment argues why: scoping it would hide a hand-written cross-repository edge from the
  walk, which is the row the walk exists to catch. That argument holds for a cross-team edge
  too, so the walk stays as it is and the comment gains "or cross-team". What changes is
  `cycle_error`: it reads titles with a scoped query, and a task on the path that the scope
  does not contain is written as `a task you cannot see`, with no id, title or team. The same
  applies to its vanished-row fallback, which today prints the id. The refusal still fires,
  because the cycle is real and saving the edge would close it. Only a hand-written row can
  put such a task on the path, since the service refuses a cross-team edge.
- **Repository registration is team-local.** `ensure_not_already_registered` compares only
  within the team. Refusing a directory because another team registered it would reveal that
  the other team exists. One directory registered by two teams on one machine is 054's to
  re-key (by remote), not 039's to forbid.

**The ~34 functions that take a raw `&SqlitePool` take `&ServiceContext` instead.** On `main`
@728a049 they are:

| File | Functions |
| --- | --- |
| `startup.rs` | `survey`, `tasks_left_running`, `missing_worktrees`, `missing_run_logs` |
| `db/settings.rs` | `get`, `base_instructions`, `run_environment`, `onboarding_dismissed`, `subscription_monthly_usd`, `doctor_dismissals` |
| `analytics/mod.rs` | `analytics`, `runs_in`, `planner_spend` |
| `scheduler/` | `capacity::{configured, schedule_mode, max_concurrency}`, `pause::active_until`, `state::queue_state` |
| `schedule/` | `window::active`, `mod::{enabled, rows, fetch}` |
| `strategy/` | `catalogue::catalogue`, `settings::{global_default, repository_default, approval, defaults_at}` |
| `runner/` | `process::{disallowed_tools, forbidden_operations, max_turns}`, `outcome::observed_run_cost` |
| elsewhere | `mcp/settings.rs::configured_port`, `worktree/cleanup.rs::auto_cleanup`, `tasks/service.rs::fetch_last_run` |

Whatever 033–038 added in the same shape joins this list. What each group does after the
conversion:

- **Board rows are filtered by scope.** This covers `startup::survey` and its three helpers,
  `analytics` and its two helpers, `observed_run_cost`, `fetch_last_run`, the per-repository
  half of `capacity::configured`, and the task reads behind `preview_schedule_preflight`.
  The shell now builds its `ServiceContext` before `survey` runs, and `survey` runs under it.
  Today `src-tauri/src/lib.rs` runs `survey` first (line 139) and builds the context after
  (line 163). Keep D29's kind rules where the same queries already have them.
- **Team settings are read from `team_settings`, for one team.** The keys are
  `base_instructions`, `strategy_catalogue`, `strategy_default`,
  `strategy_default.<repository_id>`, `strategy_approval`, `max_turns`, `disallowed_tools`
  and 021's review-loop keys. A read made for a task uses the task's team. So the prompt,
  the planner's catalogue and a run's forbidden operations all come from the team that owns
  the card. `repository_default` for another team's repository id is `NotFound`, because
  the repository is looked up first. `max_turns` and `disallowed_tools` read the team value
  only; the runner's stricter override is 042's.
- **The user setting is read from `user_settings`, for the context's actor.** That is
  `subscription_monthly_usd` (ADR-0030 point 8, D28 part 4).
- **Machine state is read unfiltered from the tables that still hold it.** This covers the
  runner keys in D28 part 4's table, still in `settings` until 040, and `schedules`, which
  has no team and moves whole in 041. These functions take the context so the structural
  test below needs no entry for them. Each carries a one-line doc comment naming 040 or 041
  as the task that moves it.

**The settings split, built on 038's placement.** 038 already declares where every key
lives: `db::settings::placement(key) -> Placement { Team, User, Runner }`, with `RUNNER_KEYS`
and `USER_KEYS` and team placement by exclusion (D28 part 4). 039 adds no second declaration;
it routes every read and write through that function.

- **`db::settings::get` and `db::settings::set` stop being public.** Between them they read
  and write any key in the legacy table, and eight modules call `set` today
  (`scheduler/{capacity,state,pause}.rs`, `schedule/window.rs`, `strategy/{catalogue,
  settings}.rs`, `mcp/settings.rs`, `worktree/cleanup.rs`). Both become private to
  `db::settings`, or go. So does 034's executor-generic `set_in(executor, key, value)`:
  it writes any key into the legacy table, and its only callers are the digest marker's two
  paths, which move to `set_user_in` below. No caller of `set_in` survives.
- **Three `pub(crate)` accessor pairs replace them**, one per placement. They are
  `pub(crate)`, not private, because their callers live in `scheduler/`, `strategy/`,
  `mcp/settings.rs`, `worktree/cleanup.rs` and `runner/process.rs`:
  - `get_team(ctx, team_id, key)` and **`set_team(ctx, team_id, key, value)`**, over
    `team_settings`. `team_id` must be in `ctx.scope`, or the call is `NotFound`. This is
    the one team-settings writer: 045 adds `revision`, `updated_by` and
    `written_during_run` to it, and 051 adds the owner check to it, so a key added later
    inherits both without anyone remembering to.
  - `get_user(ctx, key)` and `set_user(ctx, key, value)`, over `user_settings` for
    `ctx.actor`. Beside them, **`get_user_in(ctx, conn, key)` and `set_user_in(ctx, conn,
    key, value)`**, the same two statements run on a connection the caller's transaction
    holds, publishing nothing. They take `ctx` for the actor and nothing else, never its
    pool. They replace 034's `set_in` for the one key that needs a transactional write,
    `review_digest_seen_through`. `get_user` and `set_user` are those two run over the pool,
    `set_user` followed by its publish, so the key has one read and one write statement.
  - `get_runner(ctx, key)` and `set_runner(ctx, key, value)`, over the legacy `settings`
    table, with a doc comment naming 040 and 041. 041 deletes this pair.

  Each accessor checks `placement(key)` and returns `Error::internal` for a key of another
  placement. That is a wiring bug, never a user error. The per-repository key
  `strategy_default.<repository_id>` is placed `Team` by exclusion, like every key 038's
  lists do not name, so it needs nothing of its own.
- **Typed readers and writers go through the accessor their key's placement names.**
  `set_base_instructions(ctx, value)` is `set_team(ctx, ctx.scope.sole()?, …)`. A
  per-repository strategy default takes the repository's team, from the scoped lookup. After
  039, a team key or user key written through a service never touches the legacy `settings`
  row. Those rows are left in place, unread, for 065 to drop with the table.
- **Which team a settings event names.** 038 left comments at these sites saying the event
  names `ctx.scope.sole()?` "until 039". 039 resolves each one, and no "until 039" comment
  survives:
  - `set_team` publishes `ChangeEvent::settings(team_id)`, the written row's team;
  - `set_user` keeps `sole()`, with the comment rewritten to name 048's `Audience::User`;
  - a review verdict that advanced the digest marker publishes `Settings` naming the
    acted-on task's team, the same team its `Tasks` event names. The verdict names an
    entity, so it needs no `sole()`, and the comment names 048's `Audience::User`;
  - `set_runner` and the `schedules` writers keep `sole()`, with the comment rewritten to
    name 041, which moves the state, and 048, which moves the event off the team channel.

**034's digest marker.** `review_digest_seen_through` is placed **User** (D28 part 4, 034's
amendment), and 034 left its team scoping here (034 Notes). 034 writes it through `set_in`
into the legacy table, inside the review action's transaction. 039 keeps the transaction
and changes the store:

- **Both writers go through `set_user_in`, on the transaction they already hold.**
  `mark_seen(ctx, through)` reads the current value with `get_user_in` and writes with
  `set_user_in`, inside its one `BEGIN IMMEDIATE` transaction, as 034 specified. A review
  verdict that advances the marker does the same inside its own transaction, so the column
  move and the marker commit together or not at all. Neither writes the legacy `settings`
  row, which stays in place, unread, for 065. `review::digest` reads the window's start
  with `get_user`, the actor's row.
- **The marker is the actor's.** A verdict writes `ctx.actor`'s row, never the row of the
  user who triggered the run or owns the card. A teammate's review never moves another
  user's marker.
- **"Leaves no unarchived task in `in_review`" counts the acted-on task's team.** The count
  is a scoped query, `team_id = <the task's team>`, in the verdict's transaction. Another
  team's queue is neither counted nor revealed: a verdict that empties team A's queue
  advances the marker whatever team B holds, and a verdict that leaves a task of A's in
  `in_review` does not advance it, even under a context that reaches only A. This is the
  "caller's team's queue" 034 asked for.
- **The digest and `mark_seen` act on one team.** `review::digest` scopes its rows to
  `ctx.scope.sole()?`, so its entries are the team whose queue a verdict empties.
  `mark_seen` has no entity, and its `Settings` event needs a team until 048, so it calls
  `ctx.scope.sole()?` **before** it writes, exactly as `set_subscription_cost` does. Both are
  refused `invalid` under a two-team context.

**The shell holds no pool.** `src-tauri/src/` gives every core call `&state.context`,
never `&state.context.pool`. In `lib.rs`'s setup, the bare `pool` local is used only by
`db::connect`, `db::migrate` and `identity::ensure_solo`, which run before any context exists
(D28 part 3), and is then moved into `ServiceContext::new`. Everything after that, including
`startup::survey` and the `mcp::configured_port` read at `lib.rs:303`, takes the context.
This also removes:

- the shell-side `settings::get(pool, STRATEGY_CATALOGUE)` in `commands/strategy.rs:86`;
- the two raw reads in `notify.rs`.

A command body that reached the store itself would sit outside every test this task and 046
write.

**The two-team fixture.** `testing::teams::TwoTeams`, behind the `testing` feature. It is a
**server-shaped board**: no `solo_identity` row, and both teams made by 038's
`identity::create_personal_team`, never by hand-written `INSERT`s into `users`, `teams` or
`team_memberships`. The fixture must build teams the way the product does, and 051 builds on
it for boards with no solo identity. Each team gets:

- its own owner, the one `create_personal_team` makes;
- a repository in a real git repo in a `TempDir`. Team B's directory name contains
  `team-b-sentinel`, so a leaked path is caught by the same scan as a leaked title;
- tasks in all four columns and one archived;
- a dependency edge and a link;
- one run of each kind, with every capture column set, a review bundle and findings;
- every team settings key, set to a value that differs between the teams.

Every title, plan, repository name and **text** settings value in team B contains the
sentinel `team-b-sentinel`. Numeric and enum keys, such as `max_turns` and
`strategy_approval`, cannot hold it; they get values that differ between the teams, and the
tests that read them assert the exact value.

The fixture exposes three contexts:

- `a`: scoped to team A, acting as A's owner;
- `b`: scoped to team B, acting as B's owner;
- `both`: scoped to both teams, acting as **A's owner**.

A's owner is not a member of team B. No service writes a membership before 051, and the
fixture writes none by hand. 039's services check the scope, not membership: turning
membership into a scope is the edge's job, and that edge is 046's and 047's. `both` is the
context those edges would build for a user in two teams.

Runs, planners and credentials never reach a real binary or a real keychain. The fixture's
`RunnerConfig` points at `testing::cli::FakeCli`, which replays recorded fixture streams
(CLAUDE.md), and the credential store is `testing::credentials::MemoryStore`. `run_doctor`
gets `testing::doctor::temp_environment()`. A case that spawns (`start_task_run`,
`retry_task_now`, `start_queue`, `plan_task_strategy`, `plan_tasks_strategy`, and whatever
033–038 added) asserts what was spawned through `FakeCli::started()` and ends with
`assert_nothing_fell_through()`.

The fixture lives in `testing/` so 046's `crates/server/tests/commands.rs` can reuse it (D32
point 5).

**The registry test**, in `crates/core/tests/tenant_isolation.rs`. It has one case table,
keyed by tool name, with one entry per MCP tool in `Tool::ALL`. Every case runs as team A
over a real client against a bound loopback server, as
`a_real_client_at_a_scoped_url_is_refused_a_task_that_is_not_its_own` already does. Every
case gets four checks:

1. **Nothing leaks into the answer.** The serialized answer contains none of team B's ids and
   not the sentinel.
2. **A foreign id reads as a missing one.** A case that takes an id is called twice, once
   with team B's id and once with a freshly generated one. The two answers are equal once
   the id is substituted.
3. **A refused write changes nothing.** After a write case, a snapshot of team B's rows is
   unchanged. That covers `tasks`, `repositories`, `runs`, `task_links`,
   `task_dependencies`, `review_bundles`, `review_findings`, `team_settings` and B's owner's
   `user_settings`.
4. **No event names team B.** Every change event the case published names team A (038 put
   the team on `ChangeEvent`).

**Write each case as data, in the shape 046 lifts.** A case is a name, the row's effect, and
a function `fn(&TwoTeams) -> serde_json::Value` that builds the argument object, plus the
invoker that runs it. 039's invoker is the MCP client call by name. 046 moves the table into
`crates/core/src/testing/api.rs` and swaps the invoker for `api::dispatch`, without
rewriting a case. A case written as a direct call to a core function is a case 046 would
have to write again.

`record_review_findings` and `resolve_review_finding` are refused on the operator door
(`Tool::is_run_output`, D30 point 5). Their cases here assert that refusal, which is equal
for B's id and a never-issued one. Nothing is reached through that door, so these two cases
prove nothing about isolation. The run-scoped handle test below is what covers them.

**Two more doors.**

- **The run-scoped handle.** A handle's context is
  `ctx.with_scope(TeamScope::one(<the run's task's team>))`. `RunHandles::grant` records the
  task's team from the task's row when it mints the grant, and the `/mcp/run/{token}` route
  serves every call under that context. So a handle never sees more than one team, even on
  a runner that serves several. The test follows D30 point 5's grant table: for each
  `GrantKind` (`Strategy`, `Review`, `Fix`), it builds a handle for team A's run of the
  matching kind, then calls every tool that `Tool::run_access(tool, kind)` allows, using
  team B's ids:
  - B's task id for the task-taking tools (`get_task`, and under `Strategy`
    `set_task_strategy`, `update_task`, `add_task_link` and `remove_task_link`, with B's
    link id where a tool takes one);
  - under `Review`, `record_review_findings` naming B's task or run, whichever its arguments
    name (035 owns them);
  - under `Fix`, `resolve_review_finding` with one of B's finding ids.

  Each answer must equal the answer for a never-issued id. The two id-less tools that every
  grant allows, `list_repositories` and `get_base_instructions`, are called through a team A
  handle too, and must return only team A's data, with no sentinel and none of B's ids. The
  list of allowed tools is read from `run_access`, not written out, so a tool 035 or 045 adds
  to a grant gets a case or fails the test.
- **The board port** (036). 039 adds D31 point 13's 038/039 contract case to
  `testing/board_contract.rs`; 038 explicitly left it here. It iterates every `BoardPort`
  method that takes a `LeaseRef`, and calls each through the in-process adapter scoped to
  team A with a `LeaseRef` naming team B's task, in two spellings: with B's `team_id`, and
  with A's `team_id` on B's task. Both are `NotFound`, with the message a never-issued task
  gets. The adapter narrows its context to `LeaseRef.team_id` only after checking the scope
  contains it, and the scoped query refuses a mismatched pair. Methods that take no
  `LeaseRef` are scoped by the adapter's runner (D31 point 3), not by a lease.

**CLAUDE.md.** Add **tenant isolation** to the "must have tests" list. Add one sentence under
Conventions: every service reads and writes through its context's scope, and an MCP tool is
not done until it has a case in `crates/core/tests/tenant_isolation.rs` (046 extends this to
every command).

## Out of scope

- **The Tauri-command half of the registry test.** It is 046's, where `api::registry`
  replaces a parse of `lib.rs`'s `generate_handler!` blocks (see Size). 039 still converts
  every command body to "the shell holds no pool", and
  `a_foreign_id_is_answered_exactly_as_a_missing_one` covers the id-taking services those
  commands call.
- **The `team` argument on MCP tools, and `list_teams`.** Both are 060's (ADR-0035 point 2).
- **The team switcher and anything else in `src/`.** 039 changes who sees a row, not what a
  row looks like. If a DTO has to change, stop. The design has drifted.
- **Roles.** Owner-only refusals are 051's. Every member of a scope can do everything in 039.
- **Filtering change events per subscriber.** That is 048's. The shell still forwards
  everything, because solo has one team.
- **HTTP routes, `Caller` and the registry.** These are 046's (D32).
- **Moving machine state.** The runner keys, `schedules`, local paths and credentials are
  040's and 041's. 039 only puts them behind the context.
- **The runner's stricter override of `max_turns` and `disallowed_tools`.** That is 042's.
- **Per-runner reconcile** (043) and **deleting a team** (051).
- **Any migration.** D28 already put `team_id`, `idx_tasks_team` and the composite foreign
  key in 038's file. If a query needs an index D28 did not declare, stop and amend D28. Do
  not add a file (D4).
- **Any dependency** (D6, D34). The structural test scans text; it does not need `syn`.

## Acceptance criteria

- **Every MCP tool has a case.** `every_mcp_tool_has_a_cross_team_case` fails for a name in
  `Tool::ALL` without a case, and for a case naming nothing. A tool added in a scratch commit
  without a case reddens the build.
- **No tool leaks another team's rows.**
  `a_team_cannot_see_another_teams_ids_through_any_tool` runs points 1–4 of the registry
  test over every case, and passes. A failure names the case and the point that failed.
- `a_foreign_id_is_answered_exactly_as_a_missing_one` covers each id-taking service,
  including `get_task`, `move_task`, `set_task_dependencies`, `update_task_link`, `get_run`,
  `read_run_transcript_page`, `accept_task_strategy`, `plan_task_strategy`, 034's review
  actions and 035's findings. A foreign id gets `not_found`, the same as a missing one. None
  of these calls creates a worktree, lease, process or `runs` row.
- `a_dependency_on_another_teams_task_is_refused_even_when_both_are_visible`. Under the
  `both` context, the edge is refused with the cross-repository message, and
  `task_dependencies` is unchanged.
- `a_cycle_through_another_teams_task_is_refused_without_naming_it`. A raw cross-team edge
  is written into `task_dependencies` by hand. Saving the edge that closes the cycle is
  refused under the `a` context, the message names team A's titles and `a task you cannot
  see`, and it contains neither the sentinel nor any of team B's ids.
- `a_task_cannot_move_to_another_teams_repository`. Under the `both` context, the refusal
  is `invalid` with the message above, and the task row is byte-identical afterwards. Under
  the `a` context, the same call is answered exactly as for a never-issued repository id.
- 038's `a_task_takes_its_repositorys_team` and
  `a_change_event_names_the_team_of_the_row_it_announces` still pass.
- `a_context_reaching_two_teams_is_refused_an_entity_less_call`. `list_tasks`,
  `list_repositories`, `register_repository`, `set_base_instructions`,
  `set_subscription_cost`, `get_review_digest` and `mark_review_digest_seen` under the
  `both` context are refused `invalid`, and the message
  contains both teams' ids. `get_subscription_cost` answers with the actor's value, and
  analytics under the same context covers both teams. No refused call wrote anything.
- `registering_a_directory_another_team_registered_reveals_nothing`. Team A registers the
  directory that team B already registered, and it succeeds.
- `each_team_composes_its_own_base_instructions`. The prompt composed for team A's task
  equals, as an exact string, the prompt composed from team A's `base_instructions`, and
  contains no sentinel. `crates/core/tests/prompt.rs` is not modified by this task: it is
  identical to the commit 039 starts from, which carries 035's `rimaia-run` respelling.
- `a_run_is_forbidden_what_its_own_team_forbids`. `forbidden_operations` and `max_turns`
  for team A's task come from team A's `team_settings`, and neither changes when team B's
  values change.
- `every_settings_key_reads_from_the_store_d28_places_it_in`. It iterates the same key list
  038's `every_settings_key_has_the_placement_the_migration_gave_it` iterates, plus one
  `strategy_default.<repository_id>` for the fixture's repository, named explicitly because
  no constant spells it. If 038's list is local to its test, move it to
  `db::settings::ALL_KEYS` behind `#[cfg(any(test, feature = "testing"))]`, so both tests
  share one list. For each key, a distinct value is written into the legacy `settings`,
  into `team_settings` and into `user_settings`, and the typed reader returns the value
  from the store `placement(key)` names. An accessor handed a key of another placement
  returns `Error::internal`.
- `a_team_setting_written_after_the_split_never_touches_the_legacy_table`. After
  `set_base_instructions`, the `settings` row for `base_instructions` is unchanged, and a
  stale value in it is never read.
- `db::settings::get` and `db::settings::set` are not `pub`. The per-placement accessors
  are `pub(crate)`, and `set_team` is the only function that writes `team_settings`,
  removals included (`None` deletes the row), beside the seed row 038's
  `identity::create_personal_team` writes as the team comes into being.
  `each_split_settings_table_has_one_writer` holds both tables to their one writer.
  034's `set_in` is gone: `grep -rn "set_in(" crates/core/src` returns nothing, and
  `set_user_in` is the only statement that writes `user_settings`.
- `the_digest_marker_is_written_to_the_actors_user_settings_row`. Under the `a` context,
  `mark_seen` and a verdict that empties team A's queue each leave the new instant in A's
  owner's `user_settings` row. The legacy `settings` row for `review_digest_seen_through`
  is unchanged, B's owner's row is unchanged, and a stale value planted in the legacy row
  is never read by `review::digest`. The verdict's `Settings` event names team A.
- `the_review_that_empties_its_teams_queue_advances_the_marker_whatever_another_team_holds`.
  Team A has one task in `in_review` and team B has several. Approving A's task under the
  `a` context, and again on a fresh fixture under the `both` context, advances the marker
  to the faked clock's `now`, and B's tasks, rows and owner's marker are unchanged.
- `another_teams_empty_queue_does_not_advance_the_marker`. Team B's `in_review` is empty and
  team A has two tasks there. A verdict on one of A's leaves the marker where it was, under
  `a` and under `both`.
- `a_refused_verdict_on_another_teams_task_leaves_every_marker_where_it_was`. Under the `a`
  context, approving B's last `in_review` task is answered as a never-issued id would be,
  and neither owner's `user_settings` row changes. The verdict's marker write runs on the
  verdict's own transaction because `set_user_in` takes that connection; no test injects a
  failure between the two writes, since 034's actions offer no seam for one.
- `grep -rn "until 039" crates/core/src` returns nothing. Every team-settings event names
  the written row's team, and the remaining `sole()` sites at `set_user`, `set_runner` and
  the `schedules` writers each carry a comment naming 048, and 041 where it applies.
- `analytics_count_only_the_callers_teams_runs` and
  `the_observed_run_cost_is_the_median_of_the_callers_teams_runs`. Both use the fixture's
  runs, whose costs differ between the teams. Each figure is asserted exactly, and D29
  point 7's kind breakdown is unchanged.
- `the_startup_survey_reports_only_its_scopes_tasks`. It uses a real `TempDir` whose
  worktree and log files are missing for both teams.
- `the_queue_never_claims_another_teams_task`. It uses `crates/core/tests/scheduler.rs`'s
  harness, a faked clock, a real git repo and a fixture stream. With team B's task first in
  priority and team A's second, a queue scoped to team A claims only A's, and B's task stays
  `idle` in `ready`. No `sleep`.
- **The board port cannot reach another team's lease.** The contract case in
  `testing/board_contract.rs` iterates every `BoardPort` method that takes a `LeaseRef`,
  and each answers both spellings of a team B lease with `NotFound`, equal to a
  never-issued task's answer, through the in-process adapter.
- **A run-scoped handle cannot tell another team's entity from a missing one.**
  `a_run_scoped_handle_cannot_reach_another_teams_ids`. For every `GrantKind`, every tool
  `Tool::run_access` allows that grant, called through a handle for team A's run with team
  B's task, run or finding id, answers exactly as it does for a never-issued id.
  `a_run_scoped_handle_lists_only_its_own_team` asserts that `list_repositories` and
  `get_base_instructions` through a team A handle, under each grant, return only team A's
  data and no sentinel.
- **No service reaches the store without a scope.** `no_service_takes_a_pool_without_a_scope`
  scans the text of every `fn` signature in `crates/core/src/`, multi-line signatures
  included, through its opening brace. It skips `testing/` and every `#[cfg(test)]` module,
  such as the helpers in `startup.rs`'s tests and `db/mod.rs`'s `applied_versions`. It
  fails on either of:
  - a `fn` of any visibility that takes `SqlitePool` or `&SqlitePool`;
  - a `pub`, `pub(crate)` or `pub(super)` `fn` that takes `SqliteConnection`, a
    `Transaction`, or an `Executor` bound.

  Private helpers that take a connection are allowed: they are how a service shares one
  transaction, and they are reachable only from their module's functions that took the
  context. A pool is different, because it can start a transaction the service never sees.
  The exceptions are one named list in the test, `STORE_HANDLE_EXCEPTIONS`, each entry a
  path and a one-line reason:
  - `db::connect`: it makes the pool; no context can exist yet;
  - `db::migrate` and `db::apply_migrations`: they run before the context is built;
  - `identity::ensure_solo`: it returns the scope the context is built from;
  - `identity::create_personal_team`: it writes inside the caller's transaction, for
    `ensure_solo` and 047's sign-up (038);
  - `context::ServiceContext::new`: it takes the pool the context wraps;
  - `db::settings::get_user_in` and `db::settings::set_user_in`: they read and write the
    actor's row inside a review action's or `mark_seen`'s transaction; they take the context
    for the actor, and check the key's placement like every accessor.
  - the helpers that share a caller's transaction across modules, each with its own entry
    and reason: `ServiceContext::begin` and `begin_immediate`, `repo::team_of_repository`,
    `review::digest::advance_marker`, `tasks::dependencies::dependents_in`,
    `tasks::position::rebalance_column`, and `tasks::service::{move_within,
    fetch_task_row, team_of_task}`. They take `context::ScopedTx`, a transaction that
    carries the context's scope, and the test scans `ScopedTx` exactly as it scans
    `Transaction`, so the type is no way around this list.

  A later task that needs an exception appends an entry with its reason in the same commit.
  040's `runner_placed` takes a context and needs none.
- **The shell holds no pool.** `grep -rn "\.pool" src-tauri/src` returns nothing. In
  `lib.rs`'s setup, the bare `pool` local is used only by `db::connect`, `db::migrate` and
  `identity::ensure_solo`, and is then moved into `ServiceContext::new`.
- **No migration file was added**, and `.sqlx/` is regenerated with CLAUDE.md's recipe
  (D5; D33 says tasks before 040 use it unchanged).
- **CLAUDE.md is updated.** Its must-test list includes tenant isolation, and it has the
  Conventions sentence above.
- **Every CI check passes:** `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.

## Notes

**Read first:**

- **D28**, parts 3, 4 and 6 (038's section): where `team_id` is, the settings split, the
  solo identity and the composite foreign key.
- **D29**, all nine points. The `runs` readers this task scopes are the ones D29 made
  kind-aware, and a scoping join must not drop the kind rule.
- **D30** point 5: the grant table, `GrantKind`, and `Tool::is_run_output`. The handle test
  is written over it.
- **D31** points 3 and 13.
- **D32** points 5 and 7, and the appendix. The appendix is the best list of which
  commands carry team data.
- **D13**: the repository-change guard that the cross-team move refusal extends.
- **034**, Scope's marker bullets and Notes' "039" item: `set_in`, `mark_seen`, and the
  verdict that empties `in_review`. 039 changes where they write and what they count, not
  when the marker moves.
- Also D3, D5, D8, D10, D12, D16, D17 and D33.
- ADR-0028 point 2 (settings placement), ADR-0030 point 8 (the actor and user settings),
  and ADR-0035 point 2 (the entity-less refusal).

**What 038 provides.**

- `ServiceContext` with a required scope and actor. There is no default, and it is built
  explicitly in the shell's setup, `mcp::build`, `scheduler::build` and `testing::context`.
- `TeamScope` with `one`, `of`, `contains`, `teams` and `sole`.
- `identity::ensure_solo`, and `identity::create_personal_team`, which the fixture uses.
- `team_id` on `repositories` and `tasks`, and `idx_tasks_team`.
- `ChangeEvent` carrying its team.
- `team_settings` and `user_settings`, with the rows copied into them, and
  `db::settings::{placement, Placement, RUNNER_KEYS, USER_KEYS}`.

If any of these has a different name in 038's diff, follow 038. If one is missing, stop: it
is 038's to add, not 039's to improvise.

**What comes after.**

- 040's first-launch step copies the runner keys with `runner_placed`, which takes a context
  like every other core function, so it builds its context the way the shell does.
- 041 moves every machine-state function this task flagged with a doc comment, and deletes
  `get_runner`/`set_runner`.
- 042 adds the runner's override on top of the team value 039 reads.
- 045 extends `set_team` with the revision and authorship columns.
- 046 lifts this task's case table into `testing/api.rs`, adds the Tauri-command half over
  `api::registry`, and runs the same cases over HTTP (`every_board_command_has_a_case`,
  `a_team_cannot_see_another_teams_ids`).
- 048 filters events with the team 039 checks is correct, and ends `set_user`'s `sole()`.
- 051 adds the owner check to `set_team` and builds server-shaped boards on `TwoTeams`.
- 060 adds the `team` argument that ends the two-team refusal on MCP.

**Where to start.** `crates/core/src/context.rs`, `crates/core/src/db/settings.rs`,
`crates/core/src/tasks/service.rs` (19 queries), `tasks/links.rs`, `tasks/dependencies.rs`
(034's `dependents_of` and its executor-generic query, `load_edges` and `cycle_error`),
`review/{actions,digest}.rs` (034's marker paths), `repo/mod.rs`, `runs/mod.rs`,
`startup.rs`, `analytics/mod.rs`, `runner/outcome.rs`, `runner/process.rs`,
`strategy/settings.rs`, `strategy/catalogue.rs`,
`scheduler/{capacity,selection,claim,reconcile}.rs`, `schedule/{mod,window,preflight}.rs`,
`mcp/server.rs`, `mcp/scope.rs` (`RunHandles::grant`), `src-tauri/src/lib.rs`,
`src-tauri/src/notify.rs` and `src-tauri/src/commands/*.rs`. The test patterns to copy are
`crates/core/tests/mcp_scope.rs` (the registry test and the real loopback client) and
`crates/core/tests/mcp_tools.rs` (one refusal, two doors).

**One marker per user, one queue per team.** The marker is placed User, so a person in two
teams has one marker and two queues. A verdict that empties team A's queue moves that one
marker past team B's ended runs too, and B's digest, read later under a context for B,
loses them. 039 does not make this worse than D28 already decided: the digest and
`mark_seen` refuse a two-team context, so no single answer shows both teams' entries and
then drops half of them. Solo has one team and never meets it. If 050's team switcher or
060's `team` argument shows it matters, the fix is a marker keyed by user and team, which
is a D28 part 4 amendment and a 038-shaped placement, not something this task invents.
Say so in the PR.

**Why the test compares answers rather than asserting `not_found`.** "Not found" can leak
through the message, and a refusal that fires only for existing rows (a run-state check
reached before the scope check) can leak through its code. Comparing a foreign id's answer
with a never-issued id's answer catches both, and it also covers the run-scoped handle,
whose refusals are not `not_found` at all.

**Size, and the cut that is already made.** On `main` there are ~95 query macros, and
033–038 add more. Most need a `team_id` predicate or a join, which is roughly 1,000–1,500
lines. The settings split is ~400 lines. The MCP case table is about 50 tools at ~8 lines
each, around 400 lines, and the handle and board-port cases add ~200. Behaviour and
structural tests are ~600 lines. The digest marker's move to `user_settings` and its four
tests add ~150. That comes to 2,750–3,250 lines, at the upper edge of one session.

A Tauri-command half here would add ~100 cases and a parse of `lib.rs` that 046 deletes. So
it is not in this task: 046 writes it over `api::registry`, where enumerating the commands is
free, and 046 §11 already accepts it. Between 039 and 046 the commands are covered by "the
shell holds no pool" and by the service-level tests above, which call the same functions the
command bodies call. Say so in the PR. If the rest still runs over, do not cut a test: stop
and say so.
