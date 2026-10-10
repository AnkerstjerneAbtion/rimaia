---
id: "060"
title: Hosted MCP
milestone: v0.5
status: ready
depends_on: ["055", "059", "068"]
adrs: ["0035", "0021", "0030", "0034", "0029"]
size: L
---

# Hosted MCP

## Goal

Serve the operator MCP surface from `rimaia-server` at `https://<server>/mcp`, so a planning
session on any member's machine, or in a cloud session with no Rimaia installed, writes plans
onto a shared board with the same tools it uses against the desktop today
([ADR-0035](../docs/adr/0035-mcp-when-the-board-is-remote.md)). A user registers it once:

```
claude mcp add --transport http rimaia https://<server>/mcp \
  --header "Authorization: Bearer rmp_…"
```

Five things change on the surface an agent reads, and each is ADR-0035's:

- **Every request carries a personal access token** (point 1, ADR-0030 point 6). No other
  credential opens `/mcp`.
- **Tools that list or create take an optional `team`** (point 2). When it is omitted, the
  request names no entity, and the token reaches more than one team, the call is refused
  with the teams it could mean.
- **A new `list_teams` tool** says which teams the token reaches (point 2).
- **`create_task`, `update_task` and `list_tasks` gain `assignee`** (point 3).
- **`plan_task_strategy` and `plan_tasks_strategy` become requests on the hosted server**
  (point 6). They keep their names and arguments, record a strategy request that an
  eligible runner claims with purpose `strategy`, and return once the request is recorded.
  In solo, and on a connected desktop's loopback endpoint, they still start the planner
  locally.

**Local tools have no hosted form** (point 6). The hosted endpoint serves 041's board router,
less the three tools that admit people, plus the two request forms, and nothing from the
local router. ADR-0021's parity rule is then checked per command kind (ADR-0034 point 1):
every board command reaches MCP on the hosted endpoint or has a recorded reason, and every
local command's tool reaches MCP only on a loopback endpoint.

## Why now

Everything `/mcp` stands on is in place, and the one thing that would make it dangerous has
just been removed:

- **The caller exists.** 046 put `Caller` on every board route, and its table in D32 point 7
  already assigns `/mcp` to this task, accepting `Door::Mcp` only. 047's `SessionsAndTokens`
  answers an `rmp_` token with `Door::Mcp { token_id: Some(_) }` and applies the token's team
  restriction to `Caller.teams`.
- **The board is scoped.** 039 made every board service honour its context's scope and
  refuse an entity-less call under a two-team scope. That refusal is exactly what the `team`
  argument lets an agent avoid. 050 added `Caller::narrow_to` and the `list_teams` command,
  and 051 added the team tools.
- **Assignment and consent exist.** 045 wrote `assign_task`, the authorship columns and the
  eligibility rules a strategy request is claimed under. 043 created
  `tasks.strategy_requested_at` and `strategy_requested_by` and left them for this task.
- **Roles are enforced in the services.** 051's `require_owner` and 039's team-placement
  accessor refuse a member on every owner-only write, whichever door called. 051's table
  tests that through `api::dispatch`, and 068 added `set_transcript_retention` to it. Until
  now no member could reach MCP at all, because the embedded endpoint serves the solo owner
  only, so 068 left the member-over-MCP case to this task.
- **The router is already split.** 041 put the tools that inspect, reconfigure or spawn on
  one machine in a local router, and built the server so that `local: None` serves the board
  router alone.
- **A connected desktop already relays to `/mcp`.** 059's loopback answers local tools
  itself and forwards every other call to `<origin>/mcp`, which answers `404` until now.
- **055 closed the laundering path.** With `run_environment = inherit`, a run loads the
  operator's Claude Code configuration, which after this task holds a live `rmp_` token for
  the hosted server. D30's `--disallowedTools mcp__rimaia` denies the registration named
  `rimaia` and no other. 055 strips Rimaia tokens from a run's environment and denies MCP
  servers by URL (ADR-0032 point 6). Serving `/mcp` before 055 would have handed every
  inheriting run a credential to its owner's boards.

Remote planning is the handoff ADR-0006 built, extended to a team.

## Scope

Read ADR-0035 in full, D32 points 7 to 9 with the appendix and its amendment on the planning
commands, D31 points 2 to 4, D30, and the entries 045, 048, 050 and 051 added, before
starting. Where this file refines them, the refinement goes into this task's seam entry
(point 10).

**1. The route.** New: `crates/server/src/mcp.rs`, mounted at `/mcp` beside 046's
`/api/v1` router.

- **The handler takes `Caller` as its first extractor**, as every board route does, and
  refuses every door except `Door::Mcp { token_id: Some(_) }` as `Error::unauthenticated()`.
  An `rmd_` or `rmr_` token, a session cookie, a request carrying both a cookie and a bearer
  token, and a request with no credential are all `401` with `WWW-Authenticate: Bearer` and
  047's single message, `Sign in to continue.` The body is the D8 `{ code, message }` shape,
  because D32 point 3's status table applies to every route the server mounts.
- **A token that reaches no team is `unauthenticated` too.** An `rmp_` token restricted to
  teams its user has since left produces a `Caller` with no grants. There is nothing it can
  do, and the remedy is a new token. It is answered before a context is built, because
  `TeamScope::of` refuses an empty set.
- **Per request, a fresh `StreamableHttpService`** over a `RimaiaServer` built from
  `host.context.for_caller(&caller)`, with `local: None` and the hosted planning router of
  point 5. This is the shape `crates/core/src/mcp/mod.rs`'s `dispatch` already uses for
  `/mcp/run/{token}`, for the same reason: the transport is stateless
  (`legacy_session_mode: false`, `json_response: true`), so nothing is cached per token, and
  a revoked token fails on its next request (ADR-0030 point 3). The board router's handlers
  already call 055's `run_tools` body functions, so the hosted surface adds no second copy
  of any tool.
- **`/mcp` reads neither `Rimaia-Protocol` nor `Rimaia-Team`.** Its client is Claude Code,
  not a Rimaia build, and cannot be made to send either header. The MCP handshake carries its
  own protocol version, a tool's input schema is what an agent is held to, and `team` is an
  argument. If 046 checks the protocol header inside the `Caller` extractor, move the check
  into the `/api/v1` routes rather than exempting `/mcp` by path.
- **No CORS allowance.** 050's allowance covers `/api/v1/*` for the Tauri origins. `/mcp`
  has no browser client.
- **Failed authentications are logged like 047's**, recording the reason and never the
  credential. The span of an authenticated call records the tool name, `user_id` and
  `token_id`. **It never records a tool's arguments or its result**, because they carry plans
  (ADR-0037 point 6, D32 point 2).

Core gains the constructor the route calls: `mcp::hosted_server(ctx: ServiceContext) ->
RimaiaServer`. It is the only way to build a server with the hosted planning router, and
nothing in `src-tauri` calls it.

**2. Team selection** (ADR-0035 point 2). New: `crates/core/src/mcp/team.rs`.

```rust
/// What an omitted `team` means for this call.
pub enum Omitted { Refuse, Span }
/// Narrows `ctx` to the one team a tool acts in. `team` is an id or a name.
pub async fn select(ctx: &ServiceContext, team: Option<&str>, omitted: Omitted)
    -> Result<ServiceContext>;
/// The matching and the refusals, over a list. `select` calls it over the scope.
pub fn choose(teams: &[TeamRef], team: Option<&str>, omitted: Omitted)
    -> Result<Option<TeamRef>>;
```

- **`select` reads only the context's scope**, joined to `teams` for names. For a restricted
  token that scope is already the restriction (047). It never reads memberships by user id.
- **Matching.** An exact id match wins. Otherwise names are compared after trimming
  surrounding whitespace and lowercasing both sides with `str::to_lowercase`. `Some` that
  matches one team returns `ctx.with_scope(TeamScope::one(id))`.
- **`None`** returns the context unchanged under a one-team scope (solo is always this
  case), and under several teams with `Omitted::Span`. Under several teams with
  `Omitted::Refuse` it is refused.
- **The refusals, exactly.** `{list}` is every team in scope as `"{name}" ({id})`, joined
  with `, `, personal team first, then by name, then by id. That is 050's `list_teams`
  order.

  | Case | Code | Message |
  | --- | --- | --- |
  | `None`, `Refuse`, several teams | `invalid` | `This needs a team, and this token reaches {n}. Pass team as one of: {list}. list_teams returns the same list.` |
  | Matches no team in scope | `not_found` | `No team matching "{team}" is reachable with this token. Pass team as one of: {list}.` |
  | Name matches several | `invalid` | `{n} teams reachable with this token are called "{team}". Pass team as one of their ids: {ids}.` |
  | `task_ids` in several teams | `invalid` | `The tasks given belong to {n} teams. Pass team as one of: {list}.` |

  In the last row `{list}` is the tasks' teams, in the same order. The not-found message is
  the same for a team that exists outside the scope and for one that does not exist. It
  lists only the caller's own teams, so it reveals nothing (ADR-0029 point 5).

**Which tools take `team`.** `Tool::takes_team(self) -> bool` in
`crates/core/src/mcp/scope.rs`, beside `run_access`, as an exhaustive `match` with no
wildcard arm. A tool added later does not compile until someone decides.

- **`true`: the tool can act without naming an entity.** On `main` these are `list_tasks`,
  `list_repositories`, `get_base_instructions`, `create_task`, `get_strategy_catalogue`,
  `set_strategy_catalogue`, `get_strategy_defaults`, `set_strategy_defaults`,
  `get_strategy_approval`, `set_strategy_approval`, `plan_tasks_strategy` and
  `get_analytics`. Add every tool from 033 to 059 whose handler reaches `TeamScope::sole()`
  or 039's single-team refusal: at least 034's digest reads and 021's team review settings.
  045's trust-list tools moved to 061 (045's Notes, "The split"), which applies this rule to
  them. Check each against its handler; do not copy this list.
- **`false`: an id in the request determines the team, or the tool is about the person.**
  These include `get_task`, `update_task`, `move_task`, the link, dependency and strategy
  tools on one task, `plan_task_strategy`, 045's `assign_task`, 051's team tools (they take a
  team id or a token), `get_subscription_cost` and `set_subscription_cost` (a user setting),
  and `list_teams`. **`assign_task` gains no `team` argument**: its task id determines the
  team, which is ADR-0035 point 2's first bullet (045's Notes record this).
- **How a `true` tool uses it.** The handler calls `select` first and passes the narrowed
  context to the service. The service code does not change. Which `Omitted` it passes:
  - **`team` given:** any id in the request is looked up **inside** the narrowed scope, so a
    `team` that does not hold it gets the existing `not_found` for that id.
  - **`team` omitted, and the request names an entity** (`repository_id`, or `task_ids`):
    `Span`. The entity's lookup under the full scope decides the team, as 039's services
    already do for an id. `create_task` is always this case, because `repository_id` stays
    required (D16), so `team` is never required on it. `plan_tasks_strategy` with `task_ids`
    and no `repository_id` reads the tasks' teams first: one team narrows to it, several are
    the table's last row.
  - **`get_analytics`** always passes `Span`. It aggregates (039): omitted, it spans every
    team in scope and is never refused; given, it narrows to that team.
  - **Otherwise** `Refuse`.
- **The argument is named `team` everywhere**, is `Option<String>`, and carries one
  description, word for word: `The team to act in, by id or by name. Needed only when this
  token reaches more than one team; list_teams lists them.`
- **It is on the schema of every endpoint that serves these tools**, embedded solo included,
  because it is one router. In solo the scope has one team, so omitting it is always
  accepted, the solo team's id or name is accepted, and anything else is `not_found`.

**3. `list_teams`** (ADR-0035 point 2). A new `Tool::ListTeams`, in the board router, with
no arguments. It answers:

```json
{ "user": { "id": "…", "login": "…" },
  "teams": [ { "id": "…", "name": "…", "role": "owner", "personal": true } ] }
```

- **`teams` is the context's scope**, in `select`'s order, with each team's role and
  `personal = (teams.personal_user_id = user.id)`. For a restricted token that is the
  restriction, not every membership. That is the one way it differs from 050's `list_teams`
  command, which reads every membership so the switcher can list them. The parity table
  (point 8) pairs them and records the difference.
- **`user`** is the caller, so an agent can assign a task to "me" by login without guessing.
- **`run_access` is `Refused` for every grant** (035's per-grant table). A run's team is its
  task's, and a run has no business enumerating its owner's teams.

**4. Assignment on the tools** (ADR-0035 point 3). Assignment is 045's service. This task
gives it the three tool arguments and nothing new in the rules.

- **`assignee` accepts a login or a user id.** One resolver,
  `tasks::assignment::resolve_assignee(ctx, team_id, assignee: &str) -> Result<UserId>`,
  matches an exact user id first, then a login among the team's members, compared
  case-insensitively, because GitHub logins are case-insensitive.
  - A value that matches no member is `not_found`, with the message 045's `assign_task`
    gives for a user who is not a member, and the value quoted in place of the id.
  - A login shared by two members (a renamed account, or a second identity provider) is
    `invalid`: `Two members of this team are called "{login}". Pass assignee as one of their
    ids: {ids}.`
- **`create_task`** takes `assignee`. It is resolved **before** anything is written, so a
  refusal creates nothing. The insert sets `assignee_id` and `assigned_by = ctx.actor` in the
  same statement. `tasks::NewTask` gains `assignee_id: Option<String>`, and 046's
  `NewTaskInput` gains an optional `assigneeId`, so the command and the tool reach one
  function (ADR-0006). 061's new-task form sends it.
- **`update_task`** takes `assignee`, and `ClearableField` gains `Assignee` (D16.5's explicit
  clear list). Either one goes through 045's `tasks::assign_task`, in the same transaction as
  the rest of the patch. `TaskPatchInput` is unchanged: the window assigns through
  `assign_task`.
- **`list_tasks`** takes `assignee` as a filter. `TaskFilter` gains `assignee_id`, and 046's
  `TaskFilterInput` gains an optional `assigneeId`.
- **The task views say who.** `get_task`'s and `list_tasks`' responses in
  `crates/core/src/mcp/responses.rs` gain `assignee` (login) and `assignee_id`, both `null`
  when the task is unassigned. D16.6's rule that `list_tasks` omits plan text is unchanged.
- **A run cannot assign.** 045 made `assign_task` `Refused` to every grant. `update_task` is
  `OwnTaskOnly`, so a run could otherwise reassign its own card through it, handing a task
  to a runner that never agreed to run its author's plan. An `update_task` from a run that
  sets `assignee` or clears it is refused with exactly: `assignee is not available to a run:
  this handle is scoped to task {task_id}, and a run may not change who a task is assigned
  to.` The check sits in `run_tools::call`'s `update_task` arm (055), after `authorize` and
  before the body function. Every run's write reaches that arm, in process or across
  `HttpBoard`, so one check covers both.
- **Attribution needs no new code.** 045 already writes `created_by` and `plan_updated_by`
  from `ctx.actor`, and `for_caller` sets the actor to the token's user. A plan written
  through the hosted endpoint is therefore the token user's own, and a runner of theirs runs
  it without a further acceptance (ADR-0032 point 3). This task tests that, and carries
  055's mark test for the one door an `rmp_` token opens; it reimplements neither.

**5. Strategy planning becomes a request on the hosted server** (ADR-0035 point 6).

*The service.* New: `crates/core/src/strategy/requests.rs`.

- **`request(ctx, task_id) -> Result<StrategyRequest>`** records
  `strategy_requested_at = now` (the injected clock) and `strategy_requested_by =
  ctx.actor`, and publishes `ChangeEvent::tasks` for the task's team. It refuses, in this
  order, with the tags of 023's `PlanSkip`, so a refusal is classified the same from every
  door, and with sentences for the board-side facts:
  - `repository_not_opted_in`: the team ceiling on unattended runs (045) is off. `The team
    does not allow unattended runs in this repository, so no runner may plan it.` Each
    runner's own consent is checked when it claims, not here;
  - `not_planned`: the effective mode, resolved board-side, is not `planned`. `PlanSkip`'s
    own sentence;
  - `in_flight`: a `runner_leases` row exists for the task. `A runner holds this task now.
    Ask again once it is free.`;
  - archived tasks are refused as `update_task` refuses them.

  A task that already carries a proposal is **not** refused. As on the desktop,
  `plan_task_strategy` means "re-plan". A task that already has a pending request keeps its
  first `requested_at` and `requested_by`, and the call succeeds with `already_requested:
  true`. A retrying agent gets no error, and a task never has two requests.
- **`request_many(ctx, selection: &PlanSelection) -> Result<RequestPass>`** resolves the
  selection with the same function 023's `plan_all` uses (`runner::strategy::selected_tasks`
  on `main`), applies the same skips (a card that carries a proposal is skipped as
  `already_proposed`), and records a request for each card that remains. It returns each
  card's outcome, `requested` or `skipped` with its tag and sentence, plus the two counts.
  It spawns nothing and waits for nothing.
- **Who may request.** Any member of the task's team, as for assigning. A request changes no
  content, so it asks for no consent. Consent is the claiming runner's (ADR-0032 point 5).

*Two board commands* (ADR-0006: the command and the tool are adapters over one function).
D32's appendix already lists them. Rows in `crates/core/src/api/registry.rs`, handlers in
`crates/core/src/api/board/strategy.rs`, `board<T>` wrappers in `src/lib/commands.ts`, and
types in `src/types.ts`:

| Command | Effect | Arguments | Answers |
| --- | --- | --- | --- |
| `request_task_strategy` | Write | `{ taskId }` | `StrategyRequestView` |
| `request_tasks_strategy` | Write | `{ selection }`, 023's `PlanSelectionInput` | `StrategyRequestPassView` |

Each gets a case in 046's `crates/server/tests/commands.rs` and a fixture row (028). No
component calls them yet; the browser's Plan buttons are 061's.

*The hosted tools.* A third `#[tool_router]` block, `hosted_planning`, in
`crates/core/src/mcp/server.rs`, over `request` and `request_many`. Its functions are
`request_task_strategy_tool` and `request_tasks_strategy_tool`, carrying
`#[tool(name = "plan_task_strategy")]` and `#[tool(name = "plan_tasks_strategy")]`, because
`RimaiaServer` already has inherent methods with the local names. The hosted server combines
the board router with it. The embedded server combines the board router with the local
router, exactly as 041 left it.

- **Same names, same arguments.** Both use `TaskStrategyRequest` and `PlanSelectionRequest`,
  so each tool's input schema is byte-identical on both endpoints. `PlanSelectionRequest`
  gains `team` (point 2) on both.
- **Different answers, and descriptions that say so.** The hosted tools answer
  `StrategyRequestView` (`task_id`, `title`, `requested_at`, `requested_by` as a login,
  `already_requested`) and `StrategyRequestPassView`. Each description says:
  - it returns once the request is recorded, not when the plan is written;
  - which runners claim it: the assignee's, or pool runners when the task has no assignee
    (045's eligibility);
  - a runner claims it when its queue next takes work, so a runner whose owner stopped it
    does not;
  - the proposal then appears on the card, readable with `get_task`.

  Neither description may say "wait".
- **`Tool::run_access` does not change.** Both stay `Refused` to every grant, on either
  endpoint.

*The claim.* The board half of D31's `claim(ClaimTarget::Next)` (042's selection in
`board::service`) offers requested strategies:

- **A task with a pending request is a `strategy` candidate**, offered **before** any
  implementation candidate for the same runner, oldest `strategy_requested_at` first. The
  planner someone asked for runs before the implementation it would shape. For the same
  reason `Next` offers **no implementation** for a task with a pending request. `Run` (Run
  now, Retry now) is a person's explicit choice and is not refused. Its implementation lease
  does not serve the request, which stays pending.
- **Every rule a claim applies still applies:** 045's eligibility and consent for purpose
  `strategy` (ADR-0032 point 5), a pin (043), the runner's `repositories` list, and
  `FreeCapacity`, where a planner takes one slot like any claim. A requested planner is work,
  and a runner takes work only when its owner has let it. 023's one-planner-at-a-time rule
  was about a pass the user watches in the window, and does not bind requests.
- **The lease is purpose `strategy`, with `run_id` NULL** (D28's `CHECK`), and no `run_state`
  edge, as for Plan now.
- **Only the end of a `purpose = 'strategy'` lease serves a request**: its `record_strategy`
  or its `release`, in that transaction, which clears both request columns. A planner that
  failed has already left its `failed` envelope on the card by then (D17, 031). Ending an
  implementation lease never serves one. **053's expiry sweep does not clear it.** The pin
  it sets sends the request back to the same runner. If the sweep shares `release`'s body,
  the clearing belongs in the port method, not in the shared settle function.
- **A local Plan now serves a pending request as well**, because its `claim(Plan)` lease has
  purpose `strategy` and is released through the same path.
- **A task that no longer qualifies** is dropped by selection, and its request is cleared
  in the same transaction with no lease. That covers archived, no longer `planned`, or a
  ceiling switched off since the request. Nothing is left pending that no claim could ever
  take.

*The runner.* The loop in `crates/runner/src/queue/` (042) handles a `Claim` whose purpose
is `strategy` by running the planner under that lease. It uses the same function Plan now
runs after its `claim(Plan)` (043), in a bounded `LocalSlot`, because `Next` already counted
it against free capacity. It reports as Plan now does after 055: the planner's own
`set_task_strategy` reaches the board as `record_strategy` through the proxy (`run_tool`
refuses it, D31 point 4), and the runner ends the lease through `record_strategy` or
`release`.

*Plan progress.* 048 fixed `plan-pass:progress`'s wire form and left its producer to this
task. **Nothing new produces it.** A served request announces itself on `tasks:changed`,
because the proposal lands on the card and the request columns clear. A request belongs to
no pass, so it has no "3 of 10" to report, and a pass id would need a D28 column that does
not exist (D4). The desktop's local pass stays the only producer, as 048 left it.

**6. What stays local, the relay, and what the hosted endpoint leaves out.**

- **The embedded solo server and a connected desktop's loopback endpoint keep
  `plan_task_strategy` and `plan_tasks_strategy` as they are:** they claim on this machine's
  runner and run the planner. The single-task tool still awaits the planner and answers
  `PlanResultView`. This is ADR-0035 point 6's second sentence. The Tauri commands of the
  same names, and `cancel_plan_pass`, stay `local` rows; D32 already records why (its
  amendment on the planning commands).
- **The relay (059 Scope 7).** It forwards every non-local tool to `/mcp`, so `team`,
  `assignee` and `list_teams` reach a connected desktop's agent through the server's own
  implementation, and the relay adds none of them. One change: **the local router wins a
  name.** The relay's `tools/list` drops any upstream tool whose name the local router
  answers, so each planning tool is listed once, in its local form, matching `tools/call`,
  which 059 already routes locally. If 059 took its size cut, its Scope 7 lands unchanged as
  this task's first commit, with 059's relay tests.
- **`team` on the connected local `plan_tasks_strategy`.** Connected, there is no board
  `ServiceContext` to `select` over. The handler calls the `list_teams` board command with
  the desktop token (D32 point 8), runs `choose` over its answer with the same `Omitted`
  rule, and sends the chosen id as 050's `Rimaia-Team` header on the board commands it
  dispatches for the selection. Refusals are `choose`'s, word for word. If 059's board
  client cannot set that header on a call, add it there; it is one header.
- **Every local-router tool except the two planning names is absent from the hosted
  endpoint:** the doctor, credentials, worktrees, capacity, schedules, on-archive and 066's
  `list_checkouts`. A call to one of them by name gets rmcp's unknown-tool error, not a
  refusal that confirms it exists somewhere. The two planning names are served from
  `hosted_planning`.
- **The three tools that admit people are not served on the hosted endpoint:**
  `create_invitation`, `accept_invitation` and `set_member_role`. `hosted_server` removes
  their routes (`ToolRouter::remove_route`). A token that could invite an account, accept
  an invitation for its user, or raise a role could widen access beyond its own team
  restriction, in a way that outlives its own revocation (ADR-0030 point 3), which is the
  ground 047's credential rows have no tool on. The web app and the desktop keep all three.
  051's other tools are served: they read, narrow access, or act only within teams the
  token already reaches. They stay on the embedded router, where no token is involved.
- **Owner-only tools refuse a member here too** (ADR-0029 point 3). `/mcp` is the first door
  on which a member's credential reaches MCP. The refusal needs no new code, because each
  tool's handler calls the function its command calls, and the role check is inside it
  (051). What it needs is proof: every owner-only write that the hosted endpoint serves gets
  a member case over `/mcp`, with the sentence 051 or 068 fixed for it. That list is, at
  least, 068's `set_transcript_retention` (`only an owner of this team can change the team's
  settings`), the strategy catalogue, defaults and approval setters and 021's
  `set_review_settings` (the same sentence, from the accessor), 021's
  `set_repository_review_config`, 045's `set_repository_unattended_ceiling`, 054's
  `register_repository`, and 051's `remove_member`, `list_invitations` and
  `revoke_invitation`. Build it from the hosted `tools/list` against 051's table, not from
  this paragraph: a tool whose command is in 051's table and that the hosted endpoint
  serves must have a member case (Acceptance criteria). Owner-only commands that have no tool
  yet (`update_repository`, `remove_repository`, `set_base_instructions`) gain their member
  case with their tool, in 071.

**7. The `claude mcp add` line for a new token.** 050 left it here: a line that connects to
nothing is the bug `src/components/McpAddCommand.tsx` exists to avoid, and `/mcp` exists only
now.

- `hostedMcpAddCommand(origin: string, token: string): string`, beside `mcpAddCommand` in
  that file, returns exactly `claude mcp add --transport http rimaia {origin}/mcp --header
  "Authorization: Bearer {token}"`. The server name stays `rimaia` (D30 point 1). `origin`
  loses any trailing `/`.
- The token dialog in `src/views/account/` (050, or this task if 050 took its cut 2 and moved
  the dialog here) shows the line **only for a personal access token**, once, with the
  secret, and with the same copy button. `origin` is `window.location.origin` in the browser,
  and the server URL 049's transport holds on a connected desktop. The line is dropped with
  the secret when the dialog closes.
- 028's screenshot fixtures gain the dialog's open state in the `browser` scenario.

**8. Parity, per command kind** (ADR-0021 point 1, ADR-0034 point 1, D32 point 9). New:
`crates/core/src/mcp/parity.rs`, holding two tables and nothing that runs in production.

```rust
pub enum BoardParity { Tool(Tool), NoTool(&'static str) }
/// Every board row of `api::registry::COMMANDS`, exactly once.
pub const BOARD_COMMANDS: &[(&str, BoardParity)];
/// Every hosted tool that no board command pairs with, and why it has none.
pub const MCP_ONLY: &[(Tool, &'static str)];
```

- `request_task_strategy` pairs with `plan_task_strategy`, and `request_tasks_strategy` with
  `plan_tasks_strategy`. 050's `list_teams` command pairs with `list_teams`, with the
  difference point 3 names. Local rows are not in the table.
- **A `NoTool` reason is one sentence that cites its ground.** There are two kinds:
  - **Deliberate:** `delete_task` (ADR-0021 point 5); `retry_task_now` (D23); `delete_team`
    and `delete_account` (051, on the same ground); 047's account, session, token and
    pairing rows; and point 6's three membership rows. For the last two groups the reason is:
    "a personal access token that could mint credentials or admit people could widen its own
    team restriction and outlive its own revocation (ADR-0030 point 3)."
  - **A defect that predates this task:** each of D32 point 9's 18 rows that still has no
    tool when this task starts (054 gave `register_repository` one, so expect 17), and any
    board row 033 to 068 added without a tool: 050's `list_runners` and `unpair_runner`, and
    054's `list_runner_doctor_reports`, at least. Each reason is exactly `ADR-0021 point 1
    defect, closed by task 071.` **Task 071 closes them** (Notes). It exists in the backlog
    now, placed after this task and before 064, so the rows have an owner and the reason is
    true when written. A row whose gap is a deliberate choice instead goes in the first group,
    citing its ground; a defect row may not be relabelled deliberate without a seam entry
    that argues it.
- `MCP_ONLY` holds `set_task_strategy` (a planner's write; D17), `list_teams` only if 050
  added no command, 035's two findings tools (a run's output; D30), and anything else the
  implementer finds. Each has a reason.

**9. Documentation.**

- CLAUDE.md's Gotchas gains one bullet: the hosted server serves `/mcp` to `rmp_` tokens
  only and serves no local tool, and the embedded server's loopback bind is unchanged
  (ADR-0035 point 1, ADR-0006).
- `docs/adr/0021-mcp-first-capability-parity.md` gets no edit. Its rule is enforced by
  point 8's test, and the seam entry points at the test.

**10. Records.**

- A seam-contract entry with the next free `D` number, *Task 060's cross-cutting choices*,
  in the four-part shape. It records:
  - `select`, `choose`, the `Omitted` rule, the matching rule and the four messages;
  - `Tool::takes_team` and the `team` description;
  - `list_teams` answering the token's reach;
  - `resolve_assignee` and the run refusal on `update_task`;
  - the request service, strategy-first selection with no implementation offered over a
    pending request, and that only a strategy lease's end serves one;
  - that nothing new produces `plan-pass:progress`;
  - `/mcp`'s exemption from both headers, and the no-team token as `unauthenticated`;
  - the relay's local-wins rule and the connected planner's `team`;
  - the three membership tools left off the hosted endpoint;
  - the parity tables, the gap list they pin, and that 071 closes every defect row;
  - that owner-only tools refuse a member over `/mcp` with the services' own sentences.

  Each comes with its reason and the alternative it declined. It binds 061, 064 and 071.
- A D31 amendment: `Next` offers requested strategies first and no implementation over a
  pending request, and the serving rule.
- D32 needs nothing further: its 2026-10-04 amendment already keeps the planning commands
  local and lists the two request rows.

## Out of scope

- **Closing ADR-0021's pre-existing gaps.** Point 8 makes them visible and pins them, each
  naming task 071, which writes the twenty-odd missing tools (Notes). ADR-0034 point 1's MCP
  half is therefore met when 071 lands, not here, and needs no amendment.
- **The run-scoped path across the network**, meaning the runner's forwarding, the server's
  second lease check and token stripping. That is 055's (ADR-0035 point 5). This task
  changes neither the run route nor `RimaiaHandle`.
- **The loopback endpoint's gate, its token and its re-registration**: 059's. This task
  changes the relay only as point 6 says.
- **Any interface for requests**: the browser's Plan buttons, the "plan requested" mark and
  the assignee picker are 061's. `Task` gains `strategyRequestedAt` and
  `strategyRequestedBy`, optional on the TypeScript side, and no component renders them.
  050's browser gates on the planning commands stay.
- **Withdrawing a request.** A request someone regrets is dropped by selection once the task
  no longer qualifies (point 5). A withdraw command is 061's to add if its UI needs one.
- **A `team` argument on HTTP board commands.** 050's `Rimaia-Team` header narrows those.
- **Rate limiting `/mcp`.** 047's limiter covers sign-in, pairing and minting. A token is a
  256-bit secret, so guessing is not the threat. A forwarded client address is 062's.
- **Any migration.** The two request columns are in 043's
  `20261003120100_runner_leases.sql` (D28), which says 060 writes them. If a column seems
  missing, stop and ask (D28's D4 amendment).
- **Any dependency** (D6, D34). rmcp, axum, reqwest and `tracing-subscriber` are already in
  the workspace.

## Acceptance criteria

**The route** (`crates/server/tests/mcp.rs`, over a loopback listener, driving rmcp's
Streamable HTTP client as `mcp::probe` does, against 047's real `SessionsAndTokens` and a
`TestClock`):

- `the_hosted_endpoint_refuses_a_request_without_a_token`: `401`, `WWW-Authenticate:
  Bearer`, body `{"code":"unauthenticated","message":"Sign in to continue."}`.
- `the_hosted_endpoint_accepts_only_personal_access_tokens`: `rmd_`, `rmr_`, a valid session
  cookie with its CSRF header, and a valid `rmp_` token sent together with a cookie are each
  `401` with the same body. A valid `rmp_` token alone lists tools.
- `a_revoked_token_is_refused_on_its_next_request` and
  `an_expired_token_is_refused_once_the_clock_passes_its_expiry`, the second by advancing the
  `TestClock`, with no sleep.
- `a_token_that_reaches_no_team_is_unauthenticated`: a token restricted to a team its user
  then leaves.
- `the_hosted_endpoint_reads_neither_rimaia_header`: a request with no `Rimaia-Protocol` and
  one with an unsupported value both answer `tools/list`, and a `list_tasks` carrying
  `Rimaia-Team` answers exactly as without it.
- `a_write_over_hosted_mcp_is_attributed_to_the_tokens_user`: `create_task` leaves
  `tasks.source = 'mcp'`, and `created_by` and `plan_updated_by` equal to the token's user.
  045's consent evaluation then treats that user's runner as the plan's author, needing no
  acceptance.
- `a_plan_written_over_hosted_mcp_while_the_users_runner_holds_another_lease_is_marked`:
  055's arrangement, with Bob's `rmp_` token editing another of his own tasks through
  `/mcp`. That revision has `plan_written_during_run = 1`.
- `the_route_span_records_who_and_never_what`: a real `tracing` capture (047's, or a
  test-only `tracing-subscriber` writer added here) shows the tool name, `user_id` and
  `token_id`, and no field containing a `create_task` call's plan text or the token.

**Isolation** (ADR-0029 point 5; the security lens reviews this):

- `every_hosted_tool_has_a_cross_team_case`: keyed to the hosted server's `tools/list`, as
  046's `every_board_command_has_a_case` is keyed to the registry.
- `a_hosted_tool_cannot_see_another_teams_ids`: every case, run with a token that reaches
  team A only, against team B's ids, answers `not_found` with the message a nonexistent id
  gets.
- `a_restricted_token_reaches_only_its_teams`: a token restricted to A, held by a member of
  A and B, cannot read or write B through any `takes_team` tool, with or without `team`, or
  with an id of B's.
- `the_hosted_endpoint_serves_no_tool_that_admits_people`: `tools/list` lacks
  `create_invitation`, `accept_invitation` and `set_member_role`; calling each with an
  owner's token gets rmcp's unknown-tool error, and no invitation or membership row
  changes. The embedded endpoint still lists all three.
- `a_member_cannot_do_what_only_an_owner_can_over_hosted_mcp`: a case table driven over
  `/mcp` with a member's `rmp_` token, one named row per owner-only tool the hosted endpoint
  serves (point 6). It includes `set_transcript_retention` setting 30 days, answered
  `invalid` with exactly `only an owner of this team can change the team's settings`, with
  the team's `transcript_retention` key unchanged and no change event published. Every row
  asserts its own exact sentence and unchanged rows. The same calls with an owner's token
  succeed.
- `every_owner_only_tool_on_the_hosted_endpoint_has_a_member_case`: for each tool in the
  hosted `tools/list` whose command is a row of 051's
  `a_member_cannot_do_what_only_an_owner_can` (068's included), the table above has a row.
  A tool added later that is owner-only and served fails this until someone writes its
  case.

**Team selection** (`crates/core/tests/mcp_tools.rs` and a unit module for `select` and
`choose`):

- `a_team_tool_without_team_under_two_teams_is_refused_with_the_candidates`: the exact
  message, with the personal team first.
- `a_team_tool_given_a_team_by_id_or_by_name_acts_in_that_team_only`, including a name given
  in a different case with surrounding spaces.
- `an_unreachable_team_and_a_nonexistent_team_get_the_same_not_found`, byte for byte.
- `two_reachable_teams_with_one_name_are_refused_by_name_and_accepted_by_id`.
- `every_tool_has_a_team_decision`: `Tool::takes_team` is exhaustive (compile-time), and the
  test checks, for every tool in `Tool::ALL`, that its input schema has a `team` property if
  and only if `takes_team` is true, with exactly point 2's description.
- `every_team_tool_is_refused_for_want_of_a_team_and_no_other_is`: under a two-team scope,
  each `takes_team` tool except `get_analytics`, called with neither `team` nor an entity
  id, gets the candidates message, and each other tool's case succeeds. This is what
  catches a tool 033–059 added that reaches `sole()` and was classified `false`.
- `a_team_tool_naming_an_entity_takes_its_team_from_it`, with `team` omitted under two
  teams: `create_task` and `set_strategy_defaults` by `repository_id`, and
  `plan_tasks_strategy` by `task_ids` in one team, each act in that entity's team.
- `a_team_that_does_not_hold_the_entity_is_not_found`: `create_task` with a `team` that
  does not hold the repository.
- `plan_tasks_strategy_over_two_teams_tasks_is_refused_with_their_teams`: the exact message.
- `get_analytics_spans_every_reachable_team_unless_team_narrows_it`.
- `the_embedded_endpoint_accepts_the_solo_team_and_nothing_else`: omitted, the solo team's
  id and its name are accepted, and any other value is `not_found`.

**`list_teams`:**

- `list_teams_returns_the_caller_and_every_team_the_token_reaches`, in `select`'s order,
  with `role` and `personal`.
- `list_teams_over_a_restricted_token_lists_only_its_teams`, while 050's command, called by
  the same user from a browser session, lists both.
- `mcp_scope.rs`: `every_registered_tool_has_a_run_scope_decision` passes, and
  `list_teams` is `Refused` for every grant.

**Assignment:**

- `create_task_with_an_assignee_assigns_it_and_records_who_assigned`: `assignee_id`, and
  `assigned_by` equal to the token's user, from one insert.
- `an_assignee_who_is_not_a_member_is_refused_and_nothing_is_created`: the task count is
  unchanged.
- `an_assignee_login_matches_case_insensitively_and_an_ambiguous_login_is_refused`.
- `update_task_assigns_and_clears_through_assign_task`: the same `assigned_by` and change
  event that `assign_task` itself produces.
- `a_run_cannot_assign_its_own_task_through_update_task`: setting and clearing are both
  refused with the exact sentence and the task is unchanged, in process and through
  `HttpBoard`'s `run_tool`.
- `list_tasks_filters_by_assignee_login_or_id`, over MCP and through the `list_tasks`
  command's `assigneeId`, which return the same ids.

**Strategy requests** (`crates/core/tests/strategy_requests.rs` for the service and the
selection, in-process; the new selection cases also go into D31's contract suite,
`crates/core/src/testing/board_contract.rs`, so 052's HTTP adapter runs them unedited):

- `hosted_plan_task_strategy_records_a_request_and_starts_nothing`: the request columns are
  set from the `TestClock` and the actor, no `runner_leases` row exists, and the call
  returns before any runner acts.
- `a_second_request_keeps_the_first`: the same `requested_at` and `requested_by`,
  `already_requested: true`, and no error.
- `a_request_is_refused_with_the_planners_tags`: one case each for
  `repository_not_opted_in`, `not_planned` and `in_flight`, asserting the tag and point 5's
  exact sentence. A task with a proposal is accepted.
- `hosted_plan_tasks_strategy_requests_the_same_cards_the_desktop_pass_would_plan`: over one
  fixture board, the set `request_many` requests equals the set `plan_all` would plan, and
  its skips carry the same tags.
- `next_offers_a_requested_strategy_before_any_implementation`: purpose `strategy`,
  `run_id` NULL, and no `run_state` change.
- `next_offers_no_implementation_for_a_task_with_a_pending_request`, while `Run` on it
  succeeds and leaves the request pending.
- `only_a_runner_eligible_for_the_task_claims_its_strategy_request`: the assignee's runner
  claims it. A teammate's runner does not, and neither does a runner without consent for the
  repository.
- `a_pinned_task_s_request_goes_to_its_pinned_runner`.
- `a_request_is_served_when_its_strategy_lease_ends`: once for `record_strategy` and once
  for `release`, with the columns cleared in the same transaction and one `tasks:changed`.
- `releasing_an_implementation_lease_leaves_the_request_pending`.
- `an_expired_strategy_lease_leaves_the_request_for_the_pinned_runner`, with the lease
  expired by advancing the `TestClock` through 053's sweep.
- `a_task_that_no_longer_qualifies_has_its_request_dropped_by_selection`: archived, and
  switched to `manual`.
- `a_local_plan_now_serves_a_pending_request`.
- `the_runner_loop_plans_a_task_it_claimed_for_a_request`: end to end, in process. A request
  goes through the hosted tool; the runner loop claims `Next`, spawns the planner as a real
  child replaying `crates/core/tests/fixtures/cli/strategy-proposal.jsonl`, and the proposal
  lands on the card, with the request cleared and the lease gone. There is no sleep: the
  loop is woken by the change event and the `TestClock`.
- `the_desktop_planning_tools_still_start_the_planner`: the existing embedded
  `plan_task_strategy` and `plan_tasks_strategy` tests in `crates/core/tests/mcp_tools.rs`,
  and `crates/core/tests/runner_strategy.rs` as a whole, pass with their assertions
  unchanged.
- `the_planning_tools_have_the_same_arguments_on_both_endpoints`: the hosted and embedded
  input schemas for both tools are equal JSON.
- `request_task_strategy` and `request_tasks_strategy` have cases in
  `crates/server/tests/commands.rs`, so 046's `every_board_command_has_a_case`,
  `a_team_cannot_see_another_teams_ids` and `both_transports_answer_every_case_identically`
  cover them.

**The relay** (`crates/runner/tests/mcp_relay_http.rs`, a connected loopback over the real
`rimaia-server`, as 055's `run_proxy_http.rs` runs it):

- `create_task_through_a_connected_loopback_reaches_the_server`: the task exists on the
  server with `source = 'mcp'` and `created_by` equal to the loopback token's user. This is
  059's end-to-end case.
- `the_relay_lists_each_planning_tool_once_in_its_local_form`.
- `a_connected_plan_tasks_strategy_resolves_team_through_list_teams`: given a team name, its
  board commands carry that team's id as `Rimaia-Team`; omitted under two teams, it gets
  `choose`'s candidates message.

**Parity** (`crates/core/tests/mcp_parity.rs`):

- `every_board_command_is_in_the_parity_table_exactly_once`, and no non-board name is.
- `every_board_command_reaches_the_hosted_endpoint_or_says_why`: each `Tool(_)` entry is in
  the hosted `tools/list`. Each `NoTool` reason is non-empty and cites an ADR, a seam entry
  or "ADR-0021 point 1 defect".
- `every_parity_defect_names_its_closing_task`: every `NoTool` reason that is not deliberate
  equals `ADR-0021 point 1 defect, closed by task 071.` byte for byte. The defect rows
  include `list_runners`, `unpair_runner` and
  `list_runner_doctor_reports` unless an earlier task gave them tools.
- `every_hosted_tool_is_a_board_capability`: each hosted tool is some row's `Tool(_)` or in
  `MCP_ONLY`.
- `no_local_tool_is_served_on_the_hosted_endpoint`: for every tool in 041's local router
  except `plan_task_strategy` and `plan_tasks_strategy`, the hosted `tools/list` lacks it,
  and calling it by name gets rmcp's unknown-tool error.
- `the_hosted_planning_tools_are_the_request_forms`: the hosted tool with each of those two
  names has `hosted_planning`'s description, which differs from the local one, says it
  returns once the request is recorded, and does not contain "wait".
- `the_loopback_endpoint_serves_every_local_tool`: the embedded server with `LocalTools`
  lists all of them.
- `no_endpoint_has_two_tools_with_one_name`: the hosted server, the embedded server, and
  the relay's `tools/list`.

**Frontend:**

- `hostedMcpAddCommand` is tested with exact strings, including an origin with a trailing
  `/`. The token dialog test shows the line for an `rmp_` token and not for any other kind,
  and no longer renders it once closed.
- `commands.ts` has `board<T>` wrappers for the two request commands, and
  `./scripts/check-command-wiring.sh` passes.

**Everything else:**

- `crates/core/.sqlx/` is regenerated with D33's recipe, and `crates/runner/.sqlx/` too if
  a runner query changed. No migration was added.
- The seam entry and the D31 amendment exist as point 10 describes. CLAUDE.md carries point
  9's bullet, and its command list still matches `.github/workflows/ci.yml` exactly.
- Every CI check passes, including the runner crate's clippy and tests added by 040:
  `npm run typecheck`, `npm run test`, `npm run build`, `cargo test -p rimaia-core`,
  `cargo test -p rimaia-runner`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo clippy -p rimaia-runner --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`, and the
  server crate's steps as 046 wrote them into CI.

## Notes

**Seam entries to read:** this task's row in "How to use this". Where to look: D8 (the
error type, and why a refusal reuses `invalid` and `not_found`), D16 (casing,
`update_task`'s clear list, `list_tasks` without plan text), D17 (the planner has no `runs`
row), D23 and D25 (recorded no-tool commands), D28 (`runner_leases`, the two request
columns, `api_tokens`, `api_token_teams`), D30 (why the registration stays `rimaia`), D31
(points 2 to 4 and 13), D32 (points 7 to 9, the appendix and its amendments), D33, and the
entries 045, 048, 050 and 051 added: consent and eligibility, the `plan-pass:progress` wire
form, `Rimaia-Team` and `Caller::narrow_to`, and roles.

**Files to start from.** On `main` @728a049:

- `crates/core/src/mcp/mod.rs`: `build` and the scoped route's `dispatch`, whose
  per-request construction the hosted route copies;
- `crates/core/src/mcp/server.rs`: `plan_task_strategy` and `plan_tasks_strategy` (the
  desktop forms, which stay);
- `crates/core/src/mcp/scope.rs`: `Tool`, `run_access`, `authorize`;
- `crates/core/src/mcp/requests.rs`: `CreateTaskRequest`, `ListTasksRequest`,
  `UpdateTaskRequest`, `ClearableField`, `PlanSelectionRequest`;
- `crates/core/src/mcp/responses.rs`: the task views, `PlanResultView`;
- `crates/core/src/runner/strategy.rs`: `PlanSkip` and its `message`/`as_str`,
  `selected_tasks`, `plan_all`, `claim_for_planning`;
- `src-tauri/src/commands/strategy.rs`: the three planning commands, which stay local;
- `src/components/McpAddCommand.tsx`;
- `crates/core/tests/mcp_tools.rs`, `mcp_scope.rs` and `runner_strategy.rs`;
- `crates/core/tests/fixtures/cli/strategy-proposal.jsonl`.

Created earlier in this chain, so read them where they now are:

- `crates/core/src/api/{registry,caller,mod}.rs` and `api/board/strategy.rs` (046, 050);
- `crates/server/src/{caller,…}.rs` and `crates/server/tests/commands.rs` (046, 047);
- `crates/core/src/identity/authenticate.rs` (047);
- `crates/core/src/board/{port,types,service}.rs` and `testing/board_contract.rs` (036,
  042, 043);
- `crates/core/src/mcp/run_tools.rs` (055): `call`'s `update_task` arm, where the run
  refusal goes, and the body functions the board router's handlers call;
- `crates/core/src/mcp/relay.rs` and `crates/runner/src/connection/mcp_upstream.rs` (059);
- `crates/runner/src/queue/` (042);
- `crates/core/src/consent/` (045) and `crates/core/src/teams/` (051);
- `src/views/account/` (050).

**What the previous tasks provide.**

- **055:** the run-scoped proxy with the server-side lease check, token stripping, and
  denial by URL. Together these mean no run carries a credential to `/mcp`, and a run's
  forwarded calls meet `run_tools::call` again on the board. This task's `update_task`
  refusal sits there.
- **059:** the relay. Every non-local tool goes to `<origin>/mcp` with the loopback `rmp_`
  token, and the local router, planning tools included, is answered on the machine. Until
  this task, a relayed call answers 059's "does not serve MCP yet".
- **053:** the expiry sweep, which pins an expired `strategy` lease's task.
- **050:** `list_teams` (the command), `Caller::narrow_to`, `Rimaia-Team`, and the token
  dialog.
- **047:** `Door::Mcp { token_id: Some(_) }` with the restriction applied, and one
  `unauthenticated` message.
- **045:** `assign_task`, eligibility and consent per purpose. If 045 left `strategy` claims
  outside consent, stop and ask, because ADR-0032 point 5 says they are inside it.
- **068:** `get_transcript_retention` and `set_transcript_retention` as board rows and tools,
  the owner-only check through the accessor, and a row in 051's role table. Its file says
  this task adds the member-over-MCP case. If 068 has not landed, stop and ask rather than
  writing the case against a tool that does not exist.
- **051:** `require_owner`, the fixed sentences, and
  `a_member_cannot_do_what_only_an_owner_can`, whose rows the hosted member table mirrors.

**What the next tasks expect.**

- **061:** the two request commands behind the browser's Plan buttons, the request columns
  on `Task` for its "plan requested" mark, and `create_task`'s `assigneeId`.
- **062:** `/mcp` behind the same reverse proxy. Nothing here reads a forwarded address.
- **064:** the docs pass describes the hosted registration line and the two MCP surfaces.
- **071:** `parity.rs`'s defect rows, each reading `closed by task 071`, the hosted
  cross-team suite keyed to `tools/list` (so its new tools need cases without being told),
  and the hosted member table, to which it adds `update_repository`, `remove_repository`
  and `set_base_instructions`.

**This task does not close ADR-0021's pre-existing gaps; task 071 does.** D32 point 9 says
hosted parity is 060's job, and ADR-0034 point 1 says every board command is reachable over
MCP. This task does the first half of that job by making the rule a test, per kind, with
every gap named. It does not write the roughly twenty missing tools: D32 point 9's rows still
open (for example `get_run`, `list_runs_for_task`, `get_blocking_reason`,
`set_base_instructions`, `update_task_link`, and the run controls), plus 050's two runner
rows and 054's doctor reports. At about 90 lines each with their cases, that is some 1,800
lines, which would take this task past one session.
[071](071-close-the-hosted-mcp-parity-gaps.md) is the second half, already in the backlog
after this task and before 064: it starts from `parity.rs`'s defect rows, turns each into a
tool with a run-scope decision, a `takes_team` decision and a cross-team case, and deletes
the row. The table can only shrink: a new board command without a tool fails
`every_board_command_is_in_the_parity_table_exactly_once` until someone writes its reason in
the same commit. Accepting the gap instead would need an
ADR-0034 amendment, and nothing argues for one: every missing tool is an adapter over a
function that already exists.

**Risks considered and accepted.**

- Any member can request a planner on a task assigned to someone else, which spends that
  person's subscription. It is bounded three ways: one pending request per task, the
  assignee's consent to the plan (ADR-0032), and their runner's own capacity and queue
  state. A planner costs cents, and ADR-0035's Consequences already accept that a token can
  write plans onto its user's boards.
- A leaked token held by an owner can still remove members, leave teams and revoke
  invitations. Each narrows access rather than widening it, is undone from the web app, and
  stops when the token is revoked.

**Size.** L. Roughly:

| Part | Lines |
| --- | --- |
| The route and `hosted_server` | ~250 |
| `select`, `choose`, `takes_team`, the `team` field on about fifteen request types, `list_teams` | ~500 |
| Assignment: resolver, three tools, filter, views, run refusal | ~350 |
| Requests: service, two commands, selection and serving, runner loop branch | ~650 |
| The relay's local-wins rule and the connected planner's `team` | ~150 |
| Parity tables | ~200 |
| The hosted member table (owner-only tools over `/mcp`, `set_transcript_retention` included) | ~120 |
| Frontend: the line, the dialog, wrappers, types, fixtures | ~200 |
| Tests (route, isolation, team, assignment, requests, relay, parity, frontend) | ~1,900 |
| Seam entry and D31 amendment, CLAUDE.md | ~200 |

That is about 4,500 lines before the regenerated cache, and some 500 more if 059 took its
cut. If it runs over, cut in this order, and amend the receiving task's file in the same
commit:

1. **The claim, serving and runner-loop half of point 5 moves to a follow-up task** placed
   directly after 060. The hosted tools and the request commands still record requests,
   which stay pending until it lands.
2. **The two request board commands move to 061**, which is their only caller. The hosted
   tools call `strategy::requests` directly, the parity table lists them under `MCP_ONLY`
   with "board command arrives with 061", and 061's file gains the two rows.
3. **The token dialog's line (point 7) moves to 061.**

Never cut the route's authentication, the cross-team cases, the member cases for owner-only
tools, `select`'s refusals, the membership tools' absence, the run refusal on `update_task`,
or the parity test. Those are what make a public MCP endpoint safe to serve. The missing
tools themselves are never a cut from this task either: they are already 071's, which is
what keeps this one inside a session.
