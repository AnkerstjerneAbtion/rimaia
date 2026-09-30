---
id: "048"
title: Change events over SSE, filtered by team
milestone: v0.5
status: ready
depends_on: ["047"]
adrs: ["0034", "0018", "0029"]
size: M
---

# Change events over SSE, filtered by team

## Goal

Serve `GET /api/v1/events`: one Server-Sent Events stream per client, carrying the same event
names and payloads the Tauri forwarder emits today (ADR-0034 point 3). Each subscriber
receives only events for the teams its caller may read (ADR-0029 point 5), a run's live tail
only when it asked for that run, and on every connection one wholesale re-read of each board
entity, which is ADR-0018's lag recovery applied to a dropped connection.

Two splits come with it, because the stream cannot be honest without them:

- **Board events and local events.** `settings:changed` today announces both a team's base
  instructions and this machine's `queue_state`, and `schedules:changed` announces rows that
  ADR-0031 point 6 gives to the runner. A server has no machine to announce. Machine-state
  changes move to a channel of their own, which the desktop shell forwards and the server
  never sees.
- **`plan-pass:progress` gets a board form.** The shell emits it today, from
  `src-tauri/src/commands/strategy.rs`, straight into Tauri. It becomes a core publication
  that carries its card's team, so the same wire event can travel over SSE once 060 moves
  planning to a runner.

**The proof is a test in which two teams share one server and neither stream ever carries
the other team's ids.**

## Why now

- **049 builds the frontend's SSE transport against this stream**, and D34 has already
  approved its client (`@microsoft/fetch-event-source`). 049 needs a fixed wire: names,
  payloads, the opening burst, what a refused stream answers, and how a tail is asked for.
- **050's web shell is a board that never refreshes without it.** A browser has no Tauri
  `listen`, and polling was rejected by task 004 and again by ADR-0018.
- **052 hands remote runners' tails to "048's fan-out"** (D31 point 4, `publish_tail`). The
  fan-out and the tail relay have to exist first.
- **047 has just made credentials revocable.** ADR-0030 point 2 requires revocation to take
  effect on the next request. A stream is one request that lasts for hours. The rule for
  what a revoked credential does to an open stream belongs in the task that opens the first
  stream, not in a retrofit.
- **059's connected desktop listens to local events through Tauri and board events over SSE**
  (ADR-0034 point 4's table). That split has to exist in core before either transport can
  follow it.

## Scope

**1. The routing key on `ChangeEvent`.** 038 gave `ChangeEvent` a `team_id`. One board
state has no team: `user_settings` (`subscription_monthly_usd`, D28 part 4), which belongs to
a user in every team they are in. Routing it by a team would either leak a teammate's write
to the team, or miss the user's other teams. So the field becomes an audience:

```rust
// crates/core/src/events.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience { Team(TeamId), User(UserId) }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvent { pub audience: Audience, pub change: Change }
```

- Every constructor 038 wrote keeps its signature and builds `Audience::Team`. One is added,
  `ChangeEvent::user_settings(user_id)`, which is `Change::Settings` for one user.
  `set_subscription_monthly_usd` publishes it for `ctx.actor`.
- The team stays routing metadata, never row data (ADR-0034 point 3). The wire payload does
  not carry it.
- Readers of `event.team_id` in 038's and 039's tests read `event.audience`. That is the only
  change to their assertions.

**2. Local events, on a channel that is not `ChangeEvent`'s.**

```rust
// crates/core/src/events.rs
/// Something about this machine changed: its runner settings, its queue, its schedules.
/// Never sent to a server, and never filtered by team, because a machine has no team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalChange { Runner, Schedules(Arc<[ScheduleId]>) }
```

- `LocalEvents` wraps a `broadcast::Sender<LocalChange>` with `publish` and `subscribe`, and
  enforces ADR-0018's two publishing rules exactly as `ServiceContext::publish` does: after
  the commit, and ignoring the send result. `Schedules` with no ids is never published.
- **It is not a field of `ServiceContext`.** The board's context is what the server holds,
  and D32 point 8 forbids a local handler from reaching it. The host constructs one
  `LocalEvents` and hands it to every machine-state writer 041 and 042 moved: the solo shell
  now, the headless runner in 058.
- **Which writes move.** Every publication of `Change::Settings` for a key in D28 part 4's
  *Runner* row, every `Change::Schedules`, and the schedule-error publication in
  `crates/core/src/scheduler/queue.rs` (today line 818, "so the Runs view re-reads") become
  `LocalChange::Runner` or `LocalChange::Schedules`. Every `sole()` publication that 038
  Scope 6 marked for 048 is resolved here. After this task no machine state publishes a
  `ChangeEvent`, and `Change::Schedules` is deleted.
- **Every in-process subscriber that reacted to machine state listens to the local channel.**
  There are two today, and a missed one fails silently:
  - the queue loop's wake (`scheduler/queue.rs`, or wherever 042 moved the runner loop),
    which must still wake on a schedule edit and on `queue_state`;
  - `announce_run_windows` in `src-tauri/src/notify.rs`, which listens for the run window, a
    runner key.

  `grep -rn 'subscribe()' crates src-tauri/src` lists the candidates.

**3. The wire mapping, in core, used by both transports.** ADR-0018 kept the event-name
strings in one function in the shell. There are now two forwarders, so the function moves to
`crates/core/src/api/events.rs` (the `api` module 046 created):

| Source | Wire event | Payload | Board or local |
| --- | --- | --- | --- |
| `Change::Tasks` | `tasks:changed` | array of task ids | board |
| `Change::Repositories` | `repositories:changed` | array of repository ids | board |
| `Change::Runs` | `runs:changed` | array of run ids | board |
| `Change::Settings` | `settings:changed` | `null` | board: team and user settings only |
| `LiveView::RunTail` | `runs:tail` | `RunTail` | board, opt-in per run |
| `LiveView::PlanProgress` | `plan-pass:progress` | `PlanProgressView` | board |
| `LocalChange::Runner` | `runner:changed` | `null` | local |
| `LocalChange::Schedules` | `schedules:changed` | array of schedule ids | local |

- `runner:changed` is the only new name. `schedules:changed` keeps its name and payload, and
  becomes local.
- The board half and the local half are two functions over two types. There is no arm that
  could map a `LocalChange` to the SSE stream.
- `WireEvent { name: &'static str, data: serde_json::Value }` is what both produce. The
  Tauri forwarder emits `data`, and the SSE stream writes `event: <name>` and
  `data: <json>`, with **no `id:` line**, because there is no log to resume from.

**4. The live channel carries its team, and plan progress.** D14's tail channel becomes
`broadcast::Sender<LiveEvent>` on `ServiceContext`:

```rust
pub struct LiveEvent { pub team_id: TeamId, pub view: LiveView }
pub enum LiveView { RunTail(RunTail), PlanProgress(PlanProgressView) }
```

- `publish_tail(team_id, tail)` takes the team. The in-process `BoardPort` adapter passes
  `lease.team_id` (038 added it to `LeaseRef`). `publish_plan_progress(team_id, progress)`
  is new.
- **Plan progress goes on this channel, not a third one.** Both are views, both are
  droppable (a plan pass returns its summary, and each proposal already announces itself on
  `tasks:changed`), and both must not lag change events. That is D14's argument for
  separating the tail, and it covers both.
- `PlanProgressView` and `PlanResultView` move from `src-tauri/src/commands/strategy.rs` into
  core with their serde attributes unchanged, because `src/types.ts`'s `PlanProgress`
  describes them. The shell's `plan_tasks_strategy` publishes each card's progress with that
  card's team, instead of calling `app.emit`. The MCP tool still publishes nothing, as today:
  the board's panel is showing the pass it started.

**5. The tail relay, and the `get_run_tail` flip (D32 point 8).**

- `TailRelay` in `crates/core/src/api/tail.rs` keeps the latest `RunTail` per run, with its
  team, in a bounded cache. It is the shell's `state::RunTails` moved into core. The capacity
  is a constructor argument: 32 in solo (today's `MAX_TRACKED_RUN_TAILS`), and 1024 on the
  server. A snapshot that ages out costs a client that opens a run mid-flight nothing except
  waiting for the next snapshot.
- `BoardHost` gains `tails: TailRelay` (D32 point 2's "048 adds the tail relay"). The host
  spawns its follower on the live channel, once.
- `get_run_tail` becomes a board `Read` row, in one commit (D32 point 8): the registry row,
  the handler moved into `crates/core/src/api/board/runs.rs`, and the wrapper switched to
  `board<T>`. The handler reads the run under the caller's scope first, so another team's run
  id is `not_found`, then answers from the relay. `commands::runs::get_run_tail` and
  `state::RunTails` are deleted.

**6. The subscription: filtering, the opening burst, lag.** `crates/core/src/api/events.rs`
owns one subscriber's life. The server adds only HTTP framing to it.

- **`Subscription::open(host, caller, tails)`** subscribes to the change channel and the live
  channel **before** it yields anything. Its first four events are the wholesale re-read:
  `tasks:changed []`, `repositories:changed []`, `runs:changed []`, `settings:changed null`,
  in that order. The server does not know whether a connection is a first one or a
  reconnect, and does not need to: a re-read on first load is harmless, and a missed one
  after a reconnect is a stale board. A client that has read the burst is therefore
  subscribed. The tests rely on this.
- **Admission.** A change event is delivered when `Audience::Team(t)` names a team in
  `caller.teams`, or `Audience::User(u)` is `caller.user_id`. A live event is delivered when
  its `team_id` is in `caller.teams` and, for a tail, its run is one the stream asked for.
  The check reads only the event and the caller, with no database lookup, so a deleted
  task's event is routed as easily as a live one (ADR-0029 point 5).
- **Lag.** `RecvError::Lagged` on the change channel yields the same four-event burst again
  and logs the count. On the live channel it is counted and dropped (D14 rule 1). Neither
  ends the stream.
- **Tails are asked for at connect.** `GET /api/v1/events?tail=<run_id>` names a run, and
  may repeat, up to `MAX_TAILS_PER_STREAM = 8`. Each id is read under the caller's scope
  before the stream starts. An unknown or foreign id refuses the whole request with
  `not_found`, the same answer `get_run` gives, so the tail cannot be used to probe. More
  than eight is `invalid`. Changing the set of watched runs means reconnecting, and the
  burst comes with it. One stream per client (ADR-0034 point 3) is kept, and the server keeps
  no per-stream state that another request could reach.
- **The solo shell routes board events through the same admission**, with `Caller::solo`,
  so "works in solo" stays evidence for "works connected". Its only differences are that it
  admits every tail, as today, and that it also forwards the local channel.

**7. A stream re-checks its caller.** ADR-0030 point 2 says revocation, and removal from a
team, take effect on the next request. A stream is one request that lasts for hours, so it
authenticates again:

- **On a signal.** `BoardHost` gains `streams: StreamControl`, a broadcast of user ids.
  `StreamControl::revalidate(user_id)` makes every open stream of that user authenticate
  again before it delivers anything more. The loop selects with the control arm first
  (`biased`). 048 calls it from 047's revocation paths: sign-out, revoking a session and
  revoking a token. 051 calls it from membership removal and role changes.
- **On an interval**, `STREAM_REVALIDATE_INTERVAL = 60s`, read from the injected `Clock`
  (`sleep_until`). This is the backstop for what nothing signals, such as a session reaching
  its idle expiry.
- **Authenticating again** calls 047's `Authenticate` with the credential the stream was
  opened with, held in memory for the stream's life and never logged. If the answer is
  `unauthenticated`, or `caller.teams` differs from the set the stream opened with, the
  stream ends. The client reconnects and is either refused or re-scoped, and re-reads. A
  changed team set ends the stream rather than being patched in place, so a stream's scope
  is only ever the one it was opened with.

**8. The route.** `GET /api/v1/events` in `crates/server/src/events.rs`, mounted by the
server beside the board routes.

- `Caller` is its first extractor, as on every board route (D32 point 7). It accepts
  `Browser`, with the CSRF header, and `Desktop`, and nothing else. An `rmp_` or `rmr_`
  token is `unauthenticated`. A second extractor keeps the credential for point 7.
- `Rimaia-Protocol` is handled exactly as 046 handles a `Read` board command.
- A refusal is answered before any byte of the stream, with the error shape and status table
  of D32 point 3, so 049's `onopen` sees a `401` or a `404` and not an event.
- The body is `axum::response::Sse` over the subscription, with axum's `KeepAlive` (a
  comment line, which clients ignore). The route logs neither the query nor any event body
  (ADR-0037 point 6).

**9. The frontend, limited to what the split forces.** Still Tauri `listen` only, because
the SSE client is 049's.

- `src/lib/events.ts` gains `subscribeToRunnerChanged`. Its doc comment says what moved and
  why, and `subscribeToSettingsChanged`'s comment stops naming `queue_state`.
- Each consumer subscribes to the event for the store it re-reads, and a consumer that reads
  both subscribes to both. `RunsView.tsx` (queue status), `McpSection.tsx`,
  `McpAddCommand.tsx` (`mcp_port`) and `ConcurrencySection.tsx` move to `runner:changed`.
  `StrategySection.tsx`, `panel/StrategySection.tsx` and `TaskCard.tsx` stay on
  `settings:changed`. `SchedulesSection.tsx` is unchanged, because the name is.
- Their tests emit the event the component now listens to. No component imports `listen`.

**10. The records.**

- A seam-contract entry, with the next free `D` number, in the four-part shape: *Task 048's
  cross-cutting choices*. It records `Audience`, `LocalChange` and `runner:changed`, the
  wire table, the opening burst, `?tail=` and its cap, and the revalidation rule, each with
  its reason and the alternative it declined. It binds 049, 050, 051, 052, 056, 059 and 060.
- A D14 amendment: the channel carries `LiveEvent`, with its team and plan progress.
- An amendment to D32's appendix row for `plan_tasks_strategy`: 048 fixes the wire form of
  `plan-pass:progress`, and 060 decides only what produces it.
- The D34 amendment for the stream crate, if the person approved it (Notes).
- A row for 048 in the seam contract's "How to use this" table.

## Out of scope

- **The frontend's SSE client, and choosing a transport per mode.** That is 049's. This task
  changes `events.ts` only for the new local name.
- **CORS for the Tauri origins, and serving the bundle.** That is 050's.
- **Closing a removed member's streams.** 051 calls `StreamControl::revalidate`. This task
  provides it and wires 047's paths.
- **Tails from remote runners.** 052's `publish_tail` route publishes onto the same live
  channel. Nothing in this task receives a tail over HTTP.
- **Planning on a runner, and who beyond the card's team sees a pass.** That is 060's. Until
  then no server runs a pass, so no SSE stream carries `plan-pass:progress` in practice.
- **A replay log, `Last-Event-ID`, or narrowing a stream to one team.** ADR-0034 point 3
  chose the re-read, and a multi-team client re-reads what an event names.
- **Counters for lagged or revalidated streams.** 062's metrics may read them from the log
  lines this task writes.
- **Any migration or new query.** Admission reads no row. The tail check reuses the existing
  run read.
- **Any dependency beyond the one the Notes describe** (D6, D34).

## Acceptance criteria

**Isolation: the reason this task exists.** Two teams on one server never see each other's
events.

- `crates/server/tests/events.rs` runs over a loopback listener with `reqwest`, reading the
  stream with `Response::chunk()`. Its fixture is two teams, each with a repository, a task
  and a run, plus a user in team A, a user in team B and a user in both.
  - `two_teams_on_one_server_never_see_each_others_events`: with all three streams open,
    one change of each board kind is published in each team. Each stream receives exactly
    the events of its teams, and the two-team user's stream receives both teams' events.
  - Absence is proven by ordering, never by a timeout. Each "must not arrive" is followed by
    a sentinel event the stream must receive, published on the same channel after the
    forbidden one. **No test sleeps or waits on a timer.**
  - `a_tail_for_another_teams_run_is_not_found`: `?tail=<team B's run>` as team A is `404
    {"code":"not_found",…}` before any event, and the body is the same one `get_run` gives
    for that id.
  - `a_stream_never_carries_another_teams_tail`: a stream that asked for its own run
    receives that run's tails, and a tail published for team B's run is never delivered to
    it, with the sentinel rule above.
  - `a_user_setting_reaches_only_that_users_streams`: team A's other members do not receive
    it, and the writer's stream in team B does.
- `cargo test -p rimaia-core` holds the same rules without HTTP, over `Subscription`:
  - `a_subscriber_receives_only_its_teams_change_events`;
  - `a_deleted_tasks_event_reaches_its_team_without_a_lookup`: the event is published after
    the row is gone, and admission still routes it;
  - `a_live_event_outside_the_callers_teams_is_never_admitted`, including a tail for a run
    the stream asked for whose event names another team.

**The wire.**

- `every_change_maps_to_the_name_the_frontend_listens_for` asserts the table in Scope 3 as
  exact strings and exact JSON payloads, for both the board and the local mapping.
- `the_stream_speaks_what_the_tauri_forwarder_emits`: for every board event, the SSE frame's
  `event` and `data` equal what the shell's forwarder passes to `emit`, because both come
  from the same function.
- `a_new_stream_opens_with_one_wholesale_reread_of_each_board_entity`: the first four events
  are exactly `tasks:changed []`, `repositories:changed []`, `runs:changed []` and
  `settings:changed null`, in that order. No local event and no `id:` line appears on the
  stream at all.
- `a_lagged_stream_rereads_each_board_entity_once_and_carries_on`, and
  `a_lagged_tail_is_dropped_and_the_stream_carries_on`.
- `more_than_eight_tails_is_invalid`.
- Refusals come before the stream: no credential is `401` with `WWW-Authenticate: Bearer`, a
  browser cookie without the CSRF header is `401`, an `rmp_` or `rmr_` token is `401`, and
  each body is `{ code, message }`.

**Revalidation, with the fake clock.**

- `revoking_a_session_ends_its_stream_before_the_next_event`: 047's revoke path is called,
  then an event is published. The stream ends without delivering it.
- `a_stream_ends_when_the_callers_teams_change`, and
  `a_stream_is_not_revalidated_before_the_interval`: advancing the `FakeClock` to one second
  short of `STREAM_REVALIDATE_INTERVAL` calls `Authenticate` no further times, and advancing
  past it calls it once.

**The split.**

- `every_runner_setting_write_announces_a_local_change_and_no_board_event`: iterates every
  key in D28 part 4's *Runner* row through its setter. Each write publishes
  `LocalChange::Runner` and nothing on the change channel.
- `every_team_setting_write_announces_its_teams_settings`: iterates the *Team* row. Each
  write publishes `ChangeEvent { audience: Team(<its team>), change: Settings }` and nothing
  local.
- `schedule_writes_announce_local_changes_only`, over create, update, enable, delete and
  fire.
- `a_refused_scheduled_start_announces_a_local_change`: the queue.rs schedule-error path.
- `the_runner_loop_wakes_on_a_local_change`: a schedule edit and a queue start each wake the
  loop, with the fake clock. The shell's run-window notification reacts to a window change
  arriving as `LocalChange::Runner`, which a unit test of `announce_run_windows`'s event
  match covers.
- `Change::Schedules` no longer exists, and no file under `crates/server/` names
  `LocalChange` or `LocalEvents`. The second is checked by a `grep` step added to
  `scripts/check-command-wiring.sh`, which already runs in CI and already reads source files.

**Tails and plan progress.**

- `the_tail_relay_keeps_the_latest_snapshot_per_run_and_forgets_the_oldest` (capacity from
  the constructor).
- `get_run_tail` is a board `Read` row, served through `dispatch`. 046's
  `every_board_command_has_a_case`, `a_team_cannot_see_another_teams_ids` and
  `both_transports_answer_every_case_identically` cover it with a new case. The case returns
  the relay's latest snapshot for team A's run, and `not_found` for team B's.
- `plan_progress_is_published_with_the_cards_team`: `plan_all` driven over a fixture stream,
  as `crates/core/tests/runner_strategy.rs` drives it. Every progress carries its card's
  team, and its payload serializes byte for byte as the shell's `PlanProgressView` did.
- The solo desktop still receives `runs:tail` and `plan-pass:progress` through Tauri, with
  unchanged payloads. `PlanPassPanel.test.tsx` and the Runs view tests pass untouched.

**Frontend.**

- `events.ts` exports `subscribeToRunnerChanged`, and the components in Scope 9 subscribe as
  it says. Their tests assert which event triggers the re-read.
- `npm run test` passes. The only test edits are the event names emitted in the tests of the
  components that moved.

**Records and gates.**

- The seam entry, the D14 amendment, the D32 appendix amendment and the "How to use this"
  row from Scope 10 are in the diff.
- `./scripts/check-command-wiring.sh` passes with `get_run_tail` flipped.
- No `.sqlx` change is expected. If one appears, both caches are regenerated by D33's recipe.
- Every CI command in CLAUDE.md passes, with whatever `rimaia-server` test command 046 added
  to that list.
- **Needs a person, and the PR body carries it as a checklist:** start `rimaia-server`
  locally. With two browser tabs signed in as members of different teams, create and move
  cards in each. Each tab's board refreshes for its own team only, and `curl -N` against
  `/api/v1/events` with each tab's cookie and CSRF header shows no foreign id. Restart the
  server while both tabs are open: each tab's board reloads once. This waits for 049,
  because a tab cannot listen to the stream until then. Until 049 lands, the `curl` half is
  the check.

## Notes

**Before this task starts: a dependency must be approved by a person.** D34 records a
known gap: `axum::response::Sse` needs a `Stream`, and nothing approved can build one from a
channel. D34 says "048 asks". The ask, made here so it can be answered in the Phase 0
review rather than in the middle of an unattended run:

- **`tokio-stream = { version = "0.1", default-features = false }`**, in `rimaia-server`
  only, for `tokio_stream::wrappers::ReceiverStream`. That wrapper needs no feature.
- **It is already in `Cargo.lock`** (`v0.1.19`, through `rmcp` and `sqlx-core`), so it adds
  nothing to the tree: the argument D34 accepted for `sha2` and `base64`.
- **The shape it allows:** the route spawns one task per stream. The task pumps
  `Subscription::next()` into a bounded `tokio::sync::mpsc` channel, and `ReceiverStream`
  hands that channel to `Sse`. Core never names a `Stream`, so core's tests need no crate.
- **Declined:** implementing `http_body::Body` by hand, which needs `http-body` as a direct
  dependency to construct a `Frame`, and is more code for the same ask. Also declined:
  `futures-util`, which is larger and not needed for one adapter.

If D34 does not carry this approval when the task starts, return `blocked` and add nothing.
The approval is a one-line D34 amendment, `048's ask, answered`.

**Read first.** ADR-0034 point 3 (all of it) and point 4's table. ADR-0018 in full, because
this task moves its mapping table and applies its recovery to a new transport. ADR-0029
point 5 (not found, never forbidden; events filtered per subscriber). ADR-0030 point 2
(revocation on the next request). ADR-0036 point 2 (the tail through the server). Seam
entries:

- **D14 and its amendment**, which this task amends again;
- **D28 part 4's placement table**, which decides which settings writes become local;
- **D31 point 4's `publish_tail`**;
- **D32 points 2, 3, 7 and 8, and the appendix rows for `get_run_tail` and
  `plan_tasks_strategy`**;
- **D34's last bullet** (the gap), D7 (the frontend's one subscription module), D8 and D10;
- **D4 and D6, as prohibitions.**

**Files to start from.**

- `crates/core/src/events.rs` (`ChangeEvent`, `Change`, `CHANGE_BUFFER_CAPACITY`) and
  `crates/core/src/context.rs` (`publish`, `publish_tail`, `subscribe_tail`).
- `crates/core/src/runner/events.rs` (`RunTail`, `TAIL_CHANNEL_CAPACITY`).
- `crates/core/src/runner/strategy.rs` (`PlanProgress`, `plan_all`'s `on_progress`).
- Where machine state is published today: `crates/core/src/db/settings.rs` (`set`),
  `crates/core/src/mcp/settings.rs`, `crates/core/src/schedule/mod.rs`, and
  `crates/core/src/scheduler/queue.rs`. Wherever 041 moved any of these, start there
  instead; `grep -rn 'ChangeEvent::settings\|ChangeEvent::schedules' crates` finds them.
- `src-tauri/src/lib.rs` (`forward_change_events`, `emit_change_event`,
  `forward_run_tail`), `src-tauri/src/state.rs` (`RunTails`, `MAX_TRACKED_RUN_TAILS`),
  `src-tauri/src/notify.rs`, `src-tauri/src/commands/strategy.rs`
  (`PLAN_PASS_PROGRESS_EVENT`, `PlanProgressView`), and `src-tauri/src/commands/runs.rs`
  (`get_run_tail`).
- `src/lib/events.ts`, `src/lib/commands.ts`, and the components named in Scope 9.
- From 046: `crates/core/src/api/{mod,registry,caller}.rs`, `crates/core/src/api/board/`,
  `crates/server/src/`, and `crates/server/tests/commands.rs`, whose loopback harness the
  events tests reuse. From 047: the `Authenticate` implementation and its revocation
  functions.

**Migration.** None.

**What the chain provides.**

- 038: `ChangeEvent { team_id, change }`, `LeaseRef.team_id`, and `TeamId`/`UserId`.
- 039: every publication names the right team, and has a test that says so.
- 041 and 042: machine-state writers on the runner side, still publishing through
  `ServiceContext` under `sole()`, each with a comment pointing here.
- 046: `BoardHost`, `dispatch`, `Caller`, the registry, and the loopback test harness.
- 047: `Authenticate` over sessions and hashed tokens, the CSRF rule, and revocation.

If any of these has a different name in the tree, follow the tree. If a machine-state
writer still reaches the board's `ServiceContext` for its pool, not only for its channel,
stop: that is 041's to finish, not 048's to work around.

**What the next tasks expect.**

- **049:** Scope 3's wire table; the opening burst; no `id:` lines; refusals as `401` and
  `404` before the stream, which its `onopen` must throw on, as D34 says (the stream is a
  read, so an unsupported protocol version is served, not `426`); `?tail=` and its cap; a
  stream that ends after revalidation, which it reconnects; and `runner:changed` as a local
  event that the connected desktop takes from Tauri.
- **051:** `StreamControl::revalidate`, to call on removal and role changes.
- **052:** a live channel whose `LiveEvent` a remote runner's `publish_tail` route can
  publish onto, taking the team from the lease.
- **056:** nothing new. The transcript flip does not touch the stream.
- **059:** the local channel, forwarded by the shell even when `AppState.board` is `None`.
- **060:** the `plan-pass:progress` wire form, and the choice of its producer.

**The traps, named so nobody finds them twice.**

- **A moved publication with a subscriber left behind** is a queue that no longer wakes when
  started, or a run-window notification that never fires. Both compile, and both pass every
  test that only checks the publication. Hence the two wake tests.
- **Subscribing after sending the burst** loses whatever is published in between. It also
  makes every test's "the burst has arrived, so we are subscribed" false.
- **Checking the team at connect only** is exactly the cached-membership bug ADR-0030 point
  2 rules out, stretched over the life of a stream.

**Size.** M, at the top of it: roughly 2.5–3k lines. About 900 are core (`events.rs`, the
`api/events.rs` subscription, `api/tail.rs`, and moving the publication sites) with their
tests, 500 are the server route and its loopback tests, 300 are shell changes, 250 are
frontend edits and their tests, and 250 are the seam records. If it runs over, cut Scope 7
(revalidation) into its own commit series at the end of the task, with the route refusing a
cookie-authenticated stream until it lands. Do not cut it out of the task: an unrevalidated
stream is the one way this task could leak. Scope 4's plan-progress half is the other clean
cut, because until 060 no server carries it. The isolation tests and the board/local split
cannot be separated.
