---
id: "048"
title: Change events over SSE, filtered by team
milestone: v0.5
status: ready
depends_on: ["047"]
adrs: ["0034", "0018", "0029", "0030", "0036"]
size: L
---

# Change events over SSE, filtered by team

## Goal

Serve `GET /api/v1/events`: one Server-Sent Events stream per client, carrying the same event
names and payloads the Tauri forwarder emits today (ADR-0034 point 3). Each subscriber
receives only events for the teams its caller may read (ADR-0029 point 5), a run's live tail
only when it asked for that run, and on every connection one wholesale re-read of each board
entity, which is ADR-0018's lag recovery applied to a dropped connection.

One split comes with it, because the stream cannot be honest without it: **board events and
local events.** `settings:changed` today announces both a team's base instructions and this
machine's `queue_state`. `schedules:changed`, and the repository and task events a checkout
or worktree write sends, announce rows that ADR-0031 point 6 gives to the runner. A
server has no machine to announce. Machine-state changes move to a channel of their own,
which the desktop shell forwards and the server never sees.

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
state has no team: `user_settings` (`subscription_monthly_usd` and 034's
`review_digest_seen_through`, D28 part 4), which belongs to a user in every team they are in.
Routing it by a team would either leak a teammate's write to the team, or miss the user's
other teams. So the field becomes an audience:

```rust
// crates/core/src/events.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience { Team(TeamId), User(UserId) }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvent { pub audience: Audience, pub change: Change }
```

- Every constructor 038 wrote keeps its signature and builds `Audience::Team`. One is added,
  `ChangeEvent::user_settings(user_id)`, which is `Change::Settings` for one user. Every
  `user_settings` write that publishes uses it for `ctx.actor`. `set_subscription_cost` drops
  the `sole()` refusal 039 gave it for want of this, and its comment naming 048.
- The team stays routing metadata, never row data (ADR-0034 point 3). The wire payload does
  not carry it.
- Every reader of `event.team_id` in the tree reads `event.audience`: 038's to 047's tests,
  and 051's `ChangeEvent::teams(team_id)` when it lands. 039's tenant-isolation check 4
  becomes: every event the case published is `Audience::Team(A)`, or `Audience::User` naming
  the acting user, which is what `set_subscription_cost` now publishes.

**2. Local events: `LocalEvents` replaces `MachineContext.changes`.**

```rust
// crates/core/src/events.rs
/// Something about this machine changed. Never sent to a server, and never filtered by
/// team, because a machine has no team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalChange {
    Runner,                         // a key in D28 part 4's Runner row
    Schedules(Arc<[ScheduleId]>),
    Checkouts(Arc<[RepositoryId]>), // checkouts: path, limit, on-archive, credentials
    Worktrees(Arc<[TaskId]>),       // worktree records
}
pub const LOCAL_EVENT_CAPACITY: usize = 256;
```

- `LocalEvents` wraps a `broadcast::Sender<LocalChange>` of that capacity with `publish` and
  `subscribe`, and enforces ADR-0018's two publishing rules exactly as
  `ServiceContext::publish` does: after the commit, and ignoring the send result. A variant
  with no ids is never published.
- **It is 041's `MachineContext.changes`, retyped**: `MachineContext { store, clock, local:
  LocalEvents }`. Every machine-state writer already takes `&MachineContext`, so none gains a
  parameter and none can reach a `ChangeEvent` sender. 041's `event_team` is deleted with the
  field, and so are the comments naming 048 at each publish site. It is not on
  `ServiceContext`: the board's context is what the server holds, and D32 point 8 forbids a
  local handler from reaching it. The host builds one `MachineContext`: the solo shell now,
  the headless runner in 058.
- **Which publications move:** every one that goes through `MachineContext.changes`.
  `Settings` for a Runner key becomes `Runner`, and `Schedules(ids)` becomes `Schedules`. So
  does the schedule-error publication in the moved queue (today `scheduler/queue.rs:818`, "so
  the Runs view re-reads"), as `Runner`. Every `sole()` publication 038 Scope 6 marked for
  048 is resolved here, and `Change::Schedules` is deleted.
- **Checkout and worktree-record writes** (`set_repository_max_concurrency`,
  `set_repository_on_archive`, the credential setters, worktree record and forget) publish
  `Checkouts` and `Worktrees` once they write through `MachineContext`, which is task 066's
  move. If 066 has landed, this task switches them. If not, they still write board columns
  and keep their board `Repositories` and `Tasks` events, which is right for a board row,
  and 066 switches each to its `LocalChange` as it moves it.
- **`Checkouts` and `Worktrees` keep their wire names** (Scope 3): a view re-reads the same
  ids either way, and the repository list and the cards already listen. Declined: new
  `checkouts:changed` and `worktrees:changed` names, one more subscription per consumer and
  one more chance at the Notes' first trap.
- **Every in-process subscriber that reacted to machine state listens to the local channel.**
  There are two today, and a missed one fails silently:
  - 042's runner loop in `crates/runner/src/queue/`. It keeps its board-change arm and gains
    a local arm on `machine.local.subscribe()` that wakes on every `LocalChange`, so a
    schedule edit, `queue_state` and, after 066, a checkout's limit still wake it;
  - `announce_run_windows` in `src-tauri/src/notify.rs`, which listens for the run window, a
    Runner key.

  `grep -rn 'subscribe()' crates src-tauri/src` lists the candidates.
- **The shell's local forwarder** follows `machine.local` in solo and in connected mode. On
  `RecvError::Lagged` it logs the count and emits Scope 3's local re-read, then carries on.

**3. The wire mapping, in core, used by both transports.** ADR-0018 kept the event-name
strings in one function in the shell. There are now two forwarders, so the mapping moves to
`crates/core/src/api/events.rs` (the `api` module 046 created):

| Wire event | Payload | Board source | Local source |
| --- | --- | --- | --- |
| `tasks:changed` | array of task ids | `Change::Tasks` | `LocalChange::Worktrees` |
| `repositories:changed` | array of repository ids | `Change::Repositories` | `LocalChange::Checkouts` |
| `runs:changed` | array of run ids | `Change::Runs` | none |
| `settings:changed` | `null` | `Change::Settings` (team and user settings) | none |
| `runs:tail` | `RunTail` | the live channel, opt-in per run | none |
| `plan-pass:progress` | `PlanProgressView` | none | the shell's local plan pass |
| `runner:changed` | `null` | none | `LocalChange::Runner` |
| `schedules:changed` | array of schedule ids | none | `LocalChange::Schedules` |

- `runner:changed` is the only new name. `schedules:changed` keeps its name and payload, and
  becomes local.
- The board half and the local half are separate functions over separate types, so no arm
  could map a local source onto the SSE stream. Two names have a source on each side, with
  the same payload meaning on both.
- `WireEvent { name: &'static str, data: serde_json::Value }` is what every function
  produces. The Tauri forwarders emit `data`, and the SSE stream writes `event: <name>` and
  `data: <json>`, with **no `id:` line**, because there is no log to resume from.
- `board_reread()` is the four-event burst of Scope 6. `local_reread()` is `runner:changed
  null`, `schedules:changed []`, `repositories:changed []` and `tasks:changed []`. The
  stream, both lag paths and both shell forwarders use these lists and keep none of their
  own. The shell keeps no event-name string: `emit_change_event`'s match is deleted.

**4. The live channel carries its team. Plan progress stays local.**

```rust
// crates/core/src/events.rs
pub struct TeamTail { pub team_id: TeamId, pub tail: RunTail }
```

- D14's tail channel on `ServiceContext` becomes `broadcast::Sender<TeamTail>`, and
  `publish_tail(team_id, tail)` takes the team. The in-process `BoardPort` adapter passes
  `lease.team_id` (038 added it to `LeaseRef`). **Every tail enters through `publish_tail`**,
  052's remote ones included. Nothing writes to the tail relay (Scope 5) directly, because
  the relay is one follower of the channel and the SSE subscriptions are others.
- **`plan-pass:progress` has one producer, the desktop's local plan pass, and 060 Scope 5
  keeps it that way.** The shell's local `plan_tasks_strategy` keeps emitting straight into
  Tauri, now the `WireEvent` that core's `plan_progress_wire(&view)` returns.
  `PlanProgressView` and `PlanResultView` move from `src-tauri/src/commands/strategy.rs` into
  `crates/core/src/api/events.rs` with their serde attributes unchanged, because
  `src/types.ts`'s `PlanProgress` describes them. This works in both modes: the command
  touches no board context, which D32 point 8 forbids and which a connected desktop does not
  have (059, `AppState.board = None`). The MCP tool still publishes nothing, as today.
- Declined: a board `LiveView::PlanProgress` carrying its card's team. 060 gives the server no
  pass, so nothing would ever publish it, and a connected desktop would be waiting for its
  own pass to come back over SSE.

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

- **`Subscription::open(host, caller, tails, revalidate)`** subscribes to the change channel,
  the live channel and `StreamControl` **before** it yields anything. Its first four events
  are `board_reread()`: `tasks:changed []`, `repositories:changed []`, `runs:changed []`,
  `settings:changed null`, in that order. The server does not know whether a connection is a
  first one or a reconnect, and does not need to: a re-read on first load is harmless, and a
  missed one after a reconnect is a stale board. A client that has read the burst is
  therefore subscribed. The tests rely on this.
- **Admission.** A change event is delivered when `Audience::Team(t)` names a team in
  `caller.teams`, or `Audience::User(u)` is `caller.user_id`. A tail is delivered when its
  `team_id` is in `caller.teams` and its run is one the stream asked for. The check reads only
  the event and the caller, with no database lookup, so a deleted task's event is routed as
  easily as a live one (ADR-0029 point 5).
- **Lag.** `RecvError::Lagged` on the change channel yields `board_reread()` again and logs
  the count. On the live channel it is counted and dropped (D14 rule 1). On the control
  channel it means "revalidate now", because the lost message may have been this user's.
  None ends the stream.
- **Tails are asked for at connect.** `GET /api/v1/events?tail=<run_id>` names a run, and
  may repeat, up to `MAX_TAILS_PER_STREAM = 8`. An id that is not a UUID (every run id is
  one, `db::models`), or a ninth id, is `invalid` before the stream: both are client bugs,
  and 049 caps at eight before it connects. A well-formed id is not looked up. Admission
  already requires the tail's team to be the caller's, so an unknown run, a run deleted with
  its task and another team's run behave identically: the stream opens and that tail never
  arrives. There is nothing to probe, and a client holding a stale id loses that tail, not
  its board. Changing the set of watched runs means reconnecting, and the burst comes with
  it. One stream per client (ADR-0034 point 3) is kept, and the server keeps no per-stream
  state that another request could reach.
- **The solo shell routes board events through the same admission**, with `Caller::solo`,
  so "works in solo" stays evidence for "works connected". Its only differences are that it
  admits every tail, as today, and that it passes no `revalidate` closure (`None`).

**7. A stream re-checks its caller.** ADR-0030 point 2 says revocation, and removal from a
team, take effect on the next request. A stream is one request that lasts for hours, so it
resolves its caller again:

- **What resolving again means.** `revalidate` is a closure the route builds and hands to
  the subscription: `Fn() -> Pin<Box<dyn Future<Output = Result<Caller>> + Send>>`. It re-runs
  the route's whole caller resolution, which is 047's `Authenticate` over the credential the
  stream was opened with (held in memory for the stream's life, never logged) followed by
  every later edge step, such as 050's `Caller::narrow_to`. So a stream narrowed to team A
  resolves to team A again, and a later edge step composes without touching this code.
- **When the stream ends.** The closure answers an error, or a caller whose team ids differ
  from the ones the stream opened with. The client reconnects and is either refused or
  re-scoped, and re-reads. A changed team set ends the stream rather than being patched in
  place, so a stream's scope is only ever the one it was opened with. A role change alone
  does not end it, because both roles read the same events (ADR-0029 point 3).
- **On a signal.** `BoardHost` gains `streams: StreamControl`, a broadcast of user ids.
  `StreamControl::revalidate(user_id)` makes every open stream of that user resolve again
  before it delivers anything more. The loop selects with the control arm first (`biased`).
  048 calls it from 047's revocation paths: sign-out, revoking a session and revoking a
  token. 051 calls it from membership removal.
- **On an interval**, `STREAM_REVALIDATE_INTERVAL = 60s`, read from the injected `Clock`
  (`sleep_until`). This is the backstop for what nothing signals: a desktop token reaching
  its `expires_at`. An open stream counts as use. Resolving goes through `Authenticate`, which
  touches a session at most once an hour (047), so a tab left open keeps its session alive,
  as a tab making requests would, and idle expiry applies to sessions nobody has open.

**8. The route.** `GET /api/v1/events` in `crates/server/src/events.rs`, mounted by the
server beside the board routes.

- `Caller` is its first extractor, as on every board route (D32 point 7). It accepts
  `Browser`, with the CSRF header, and `Desktop`, and nothing else. An `rmp_` or `rmr_`
  token is `unauthenticated`. The route keeps the credential to build Scope 7's closure.
- `Rimaia-Protocol` is handled exactly as 046 handles a `Read` board command.
- A refusal is answered before any byte of the stream, with the error shape and status table
  of D32 point 3, so 049's `onopen` sees a `401` or a `400` and not an event.
- The body is `axum::response::Sse` over the subscription, with axum's `KeepAlive`, a
  comment line that clients ignore. The route spawns one pump task per stream, which moves
  `Subscription::next()` into a bounded `tokio::sync::mpsc` channel, and
  `tokio_stream::wrappers::ReceiverStream` hands that channel to `Sse`. The pump exits when
  its `mpsc` send fails or the receiver closes, dropping the subscription and its receivers,
  so a closed tab leaks nothing. The route logs neither the query nor any event body
  (ADR-0037 point 6).
- **The one dependency, `tokio-stream`, as D34's amendment `048's ask, answered` approves
  it:** a `[workspace.dependencies]` line, `tokio-stream = { version = "0.1",
  default-features = false }`, referenced with `{ workspace = true }` from
  `crates/server/Cargo.toml` alone. No feature is turned on, because `ReceiverStream` needs
  none. Core, the shell and the runner do not take it: `Subscription::next()` is a plain
  `async fn`, and core's tests drive it directly.

**9. The frontend, limited to what the split forces.** Still Tauri `listen` only, because
the SSE client is 049's.

- `src/lib/events.ts` gains `subscribeToRunnerChanged`. Its doc comment says what moved and
  why, and `subscribeToSettingsChanged`'s comment stops naming `queue_state`. The comments in
  `commands.ts` that name `subscribeToSettingsChanged` for a runner key (`mcp_port`) follow.
- Each consumer subscribes to the event for the store it re-reads:
  - **`runner:changed` only:** `McpSection.tsx` and `McpAddCommand.tsx` (`mcp_port`), and
    `ConcurrencySection.tsx` (`max_concurrency`, `schedule_mode`).
  - **Both:** `RunsView.tsx` and `TaskCard.tsx` (`TaskCard.tsx:232-239`'s shared cache).
    Both re-read `get_queue_status`, whose runner half (`queue_state`, capacity, the window,
    the schedule error) is runner state, and whose plan is a board read (042 point 8) that
    045's `SkipReason::NotEligible` takes from `team_settings`.
  - **`settings:changed` only:** `settings/StrategySection.tsx` and
    `panel/StrategySection.tsx` (the strategy catalogue, a team key).
  - **Unchanged:** `SchedulesSection.tsx`, because the name is, and every consumer of
    `repositories:changed` and `tasks:changed`. `RunsView`'s `getRunEnvironment` is read
    once at mount and subscribes to nothing.
- Their tests emit the event the component now listens to. No component imports `listen`.

**10. The records.**

- A seam-contract entry, with the next free `D` number, in the four-part shape: *Task 048's
  cross-cutting choices*. It records `Audience`, stating that it refines ADR-0034 point 3's
  "each `ChangeEvent` carries the team it belongs to" for user settings; `LocalChange` and
  `LocalEvents` as `MachineContext.changes` retyped; `runner:changed`; the wire table with its
  two two-sided names; both re-reads; `?tail=`, its cap and its silent drop; the
  revalidation closure; and plan progress as a local emission. Each comes with its reason
  and the alternative it declined. It binds 049, 050, 051, 052, 056, 059, 060 and 066 as
  "What the next tasks expect" lists.
- A D14 amendment: the channel carries `TeamTail`, and every tail enters by `publish_tail`.
- A D24 amendment to point 7: schedules are announced by `LocalChange::Schedules`, and
  `Change::Schedules` is gone.
- An amendment to D32's appendix row for `plan_tasks_strategy`: `plan-pass:progress`'s wire
  form is in core, it is a local emission, and its only producer is the desktop's local pass
  (060 Scope 5).
- A row for 048 in the seam contract's "How to use this" table.

## Out of scope

- **The frontend's SSE client, and choosing a transport per mode.** That is 049's. This task
  changes `events.ts` only for the new local name.
- **CORS for the Tauri origins, serving the bundle, and narrowing a stream to one team.**
  Those are 050's. Its `Rimaia-Team` narrowing composes with Scope 7 through the closure.
- **Closing a removed member's streams.** 051 calls `StreamControl::revalidate`. This task
  provides it and wires 047's paths.
- **Receiving tails from remote runners.** 052's `publish_tail` route calls Scope 4's
  `publish_tail`. Nothing in this task receives a tail over HTTP.
- **A replay log or `Last-Event-ID`.** ADR-0034 point 3 chose the re-read.
- **Counters for lagged or revalidated streams.** 062's metrics may read them from the log
  lines this task writes.
- **Any migration or new query.** Neither admission nor `?tail=` reads a row.
- **Any dependency beyond `tokio-stream`** (D6, and D34 with its amendment `048's ask,
  answered`). No `futures-util`, no `http-body`, and no `tokio-stream` feature.

## Acceptance criteria

**Isolation: the reason this task exists.** Two teams on one server never see each other's
events.

- `crates/server/tests/events.rs` runs over a loopback listener with `reqwest`, reading the
  stream with `Response::chunk()` and ignoring every line that starts with `:` (axum's
  `KeepAlive`). Its fixture is two teams, each with a repository, a task and a run, plus a
  user in team A, a user in team B and a user in both.
  - `two_teams_on_one_server_never_see_each_others_events`: with all three streams open,
    one change of each board kind is published in each team. Each stream receives exactly
    the events of its teams, and the two-team user's stream receives both teams' events.
  - Absence is proven by ordering, never by a timeout. Each "must not arrive" is followed by
    a sentinel event the stream must receive, published on the same channel after the
    forbidden one. **No test sleeps or waits on a timer.**
  - `a_foreign_tail_id_opens_the_same_stream_as_an_unknown_one_and_delivers_nothing`:
    `?tail=<team B's run>` as team A and `?tail=<a random UUID>` both open with the burst.
    Tails published for team B's run never arrive, by the sentinel rule.
  - `a_stale_tail_id_does_not_end_the_stream`: after the task is deleted and its runs with
    it, a stream naming that run opens, and board events keep arriving.
  - `a_stream_never_carries_another_teams_tail`: a stream that asked for its own run
    receives that run's tails, and a tail published for team B's run is never delivered.
  - `a_user_setting_reaches_only_that_users_streams`: the two-team user writes
    `subscription_monthly_usd`. That user's stream receives `settings:changed`, and the
    A-only and B-only users' streams do not.
  - `a_closed_client_releases_its_subscriptions`: after the client drops the response, the
    change and live channels' `receiver_count()` return to their baseline. The test yields
    (`tokio::task::yield_now`) between checks and never sleeps.
- `cargo test -p rimaia-core` holds the same rules without HTTP, over `Subscription`:
  - `a_subscriber_receives_only_its_teams_change_events`;
  - `a_deleted_tasks_event_reaches_its_team_without_a_lookup`: the event is published after
    the row is gone, and admission still routes it;
  - `a_live_event_outside_the_callers_teams_is_never_admitted`, including a tail for a run
    the stream asked for whose event names another team.

**The wire.**

- `every_change_maps_to_the_name_the_frontend_listens_for` asserts Scope 3's table as exact
  strings and exact JSON payloads, for the board and the local functions, and asserts both
  re-read lists.
- In the server tests, every SSE frame's `event` and `data` equal the `WireEvent` that core's
  board function returns for the published event. The shell holds no mapping of its own:
  `scripts/check-command-wiring.sh` gains a step that fails if any wire event name appears as
  a string literal under `src-tauri/src/`.
- `a_new_stream_opens_with_one_wholesale_reread_of_each_board_entity`: the first four events
  are exactly `board_reread()`, in order. No local event and no `id:` line appears on the
  stream at all.
- `a_lagged_stream_rereads_each_board_entity_once_and_carries_on`, and
  `a_lagged_tail_is_dropped_and_the_stream_carries_on`.
- `a_malformed_or_ninth_tail_id_is_invalid`: `400` before the stream.
- Refusals come before the stream: no credential is `401` with `WWW-Authenticate: Bearer`, a
  browser cookie without the CSRF header is `401`, an `rmp_` or `rmr_` token is `401`, and
  each body is `{ code, message }`.

**Revalidation, with `testing::clock::TestClock`.**

- `revoking_a_session_ends_its_stream_before_the_next_event`: 047's revoke path is called,
  then an event is published. The stream ends without delivering it.
- In core, over `Subscription` with a scripted closure:
  - `a_stream_ends_when_the_callers_teams_change`;
  - `a_stream_survives_revalidation_to_the_same_teams_whatever_the_role`;
  - `a_lagged_control_receiver_revalidates_at_once`;
  - `a_stream_is_not_revalidated_before_the_interval`: advancing the clock to one second
    short of `STREAM_REVALIDATE_INTERVAL` calls the closure no further times, and advancing
    past it calls it once.

**The split.**

- `every_machine_state_write_announces_a_local_change_and_no_board_event`, over 041's
  `testing::machine::MemoryMachine`. It iterates every key in D28 part 4's *Runner* row
  through its setter, and schedule create, update, enable, delete and fire. If 066 has
  landed it also covers the checkout and worktree-record writes; if not, 066 adds them to
  this test. Each write publishes the matching `LocalChange` with its
  ids, and nothing on the change channel.
- `every_team_setting_write_announces_its_teams_settings`: iterates the *Team* row. Each
  write publishes `ChangeEvent { audience: Team(<its team>), change: Settings }` and nothing
  local.
- `a_refused_scheduled_start_announces_a_local_change`: the schedule-error path.
- `the_runner_loop_wakes_on_a_board_change_and_on_a_local_change`, in
  `crates/runner/tests/queue.rs` under `cargo test -p rimaia-runner`, with the `TestClock`:
  a task moved to ready wakes the loop through the board arm, and a schedule edit and a
  queue start each wake it through the local arm. The shell's run-window
  notification reacts to a window change arriving as `LocalChange::Runner`, which a unit test
  of `announce_run_windows`'s event match covers.
- `Change::Schedules` no longer exists, and no file under `crates/server/` names
  `LocalChange` or `LocalEvents`, which the same `check-command-wiring.sh` step checks.

**Tails and plan progress.**

- `the_tail_relay_keeps_the_latest_snapshot_per_run_and_forgets_the_oldest` (capacity from
  the constructor).
- `get_run_tail` is a board `Read` row, served through `dispatch`. 046's
  `every_board_command_has_a_case`, `a_team_cannot_see_another_teams_ids` and
  `both_transports_answer_every_case_identically` cover it with a new case. The case returns
  the relay's latest snapshot for team A's run, and `not_found` for team B's.
- `plan_progress_keeps_its_wire_form`: `plan_progress_wire` names `plan-pass:progress`, and a
  `PlanProgress` from `plan_all`, driven over a fixture stream as
  `crates/core/tests/runner_strategy.rs` drives it, serializes byte for byte as the shell's
  `PlanProgressView` did.
- The solo desktop still receives `runs:tail` and `plan-pass:progress` through Tauri, with
  unchanged payloads. `PlanPassPanel.test.tsx` and the Runs view tests pass untouched, apart
  from the Runs view's added `runner:changed` emission.

**Frontend.**

- `events.ts` exports `subscribeToRunnerChanged`, and the components in Scope 9 subscribe as
  it says. Their tests assert which event triggers the re-read, and for `RunsView` and
  `TaskCard` that each of the two does.
- `npm run test` passes. The only test edits are the events emitted in the tests of the
  components Scope 9 changed.

**Records and gates.**

- The seam entry and the D14, D24 and D32 appendix amendments and the "How to use this" row
  from Scope 10 are in the diff.
- `./scripts/check-command-wiring.sh` passes with `get_run_tail` flipped.
- No `.sqlx` change is expected. If one appears, both caches are regenerated by D33's recipe.
- **The dependency change is exactly D34's amendment.** The root `Cargo.toml` gains one
  `[workspace.dependencies]` line, `tokio-stream = { version = "0.1", default-features =
  false }`, and `crates/server/Cargo.toml` is the only manifest that references it. No other
  manifest, and no `package.json`, changes its dependencies. `Cargo.lock` gains no
  `[[package]]` entry: `tokio-stream` stays at the `0.1.19` that `rmcp` and `sqlx-core`
  already resolve, and `cargo tree -d` shows one `tokio-stream`.
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

**The stream crate is already approved.** D34 recorded a known gap: `axum::response::Sse`
takes a `Stream`, and nothing it approved could build one from a channel. Phase 0 closed it
before this task starts, with D34's amendment `048's ask, answered`. It approves
`tokio-stream = { version = "0.1", default-features = false }` in `rimaia-server` only, for
`tokio_stream::wrappers::ReceiverStream`, which in `0.1.19` sits outside every feature gate.
The crate is already in `Cargo.lock` (`v0.1.19`, through `rmcp` and `sqlx-core`), so it adds
nothing to the tree. The amendment also records what was declined: `futures-util`, a
hand-written `http_body::Body` (which would need `http-body` as a direct dependency), and
`BroadcastStream` straight over the channel, which would skip the subscription's admission
and re-reads. This task adds the crate exactly as the amendment says, and writes no D34
record of its own.

**Read first.** ADR-0034 point 3 (all of it) and point 4's table. ADR-0018 in full, because
this task moves its mapping table and applies its recovery to a new transport. ADR-0029
points 3 and 5 (two roles that read alike; not found, never forbidden; events filtered per
subscriber). ADR-0030 point 2 (revocation on the next request). ADR-0036 point 2 (the tail
through the server). Seam entries:

- **D14 and its amendment**, which this task amends again;
- **D24 point 7**, the `Schedules` variant this task deletes;
- **D28 part 4's placement table**, which decides which settings writes become local;
- **D31 point 4's `publish_tail`**;
- **D32 points 2, 3, 7 and 8, and the appendix rows for `get_run_tail` and
  `plan_tasks_strategy`**;
- **D34's last bullet** (the gap) and **its amendment `048's ask, answered`** (the crate,
  its line and its one user), D7 (the frontend's one subscription module), D8 and D10;
- **D4 and D6, as prohibitions.**

**Files to start from.**

- `crates/core/src/events.rs` (`ChangeEvent`, `Change`, `CHANGE_BUFFER_CAPACITY`) and
  `crates/core/src/context.rs` (`publish`, `publish_tail`, `subscribe_tail`).
- `crates/core/src/runner/events.rs` (`RunTail`, `TAIL_CHANNEL_CAPACITY`).
- `crates/core/src/runner/strategy.rs` (`PlanProgress`, `plan_all`'s `on_progress`).
- 041's `MachineContext` and every writer that publishes through its `changes`;
  `grep -rn '\.changes\.send\|machine.*publish' crates` finds them.
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
- 041: `MachineContext { store, clock, changes: broadcast::Sender<ChangeEvent>, event_team
  }`, `changes` being the board context's sender in solo. The runner-key and schedule
  writers take `&MachineContext` and publish `Settings` and `Schedules(ids)` through it.
- 066, if it has landed: the checkout and worktree-record writers on `MachineContext` too,
  publishing `Repositories(ids)` and `Tasks(ids)` through the same field.
- 042: the runner loop in `crates/runner/src/queue/`, whose board-change arm is the
  in-process context's `subscribe()`, handed to `build` by the shell.
- 046: `BoardHost`, `dispatch`, `Caller`, the registry, and the loopback test harness.
- 047: `Authenticate` over sessions and hashed tokens, the CSRF rule, and revocation.

If any of these has a different name in the tree, follow the tree. If a machine-state
writer still takes the board's `ServiceContext` rather than `&MachineContext`, stop: that is
041's to finish, not 048's to work around.

**What the next tasks expect.**

- **049:** Scope 3's wire table, copied into `EVENT_SOURCES` with three values: `board`,
  `local`, and `both` for `tasks:changed` and `repositories:changed`. This revises 049's "no
  row names two sources": a `both` row subscribes once per distinct installed transport, so
  solo and fixture mode, where one transport serves both, still deliver once.
  `plan-pass:progress` is `local`. Also: the opening burst; no `id:` lines; refusals as `401`
  and `400` before the stream, which its `onopen` must throw on, as D34 says (the stream is a
  read, so an unsupported protocol version is served, not `426`); `?tail=`, its cap, and an
  unknown or foreign id that is dropped, not refused; and a stream that ends after
  revalidation, which it reconnects.
- **050:** `/api/v1/events` filtered by `caller.teams`, and Scope 7's closure, which must
  include `Caller::narrow_to` so a narrowed stream stays narrowed.
- **051:** `StreamControl::revalidate`, to call on removal. Its `teams:changed []` joins
  `board_reread()`, so it rides the opening burst and the lag burst.
- **052:** remote tails enter through `publish_tail(team_id, tail)` on the live channel,
  taking the team from the lease, and never through `TailRelay` directly. Its `publish_tail`
  handler calls that, not the relay.
- **066**, if it lands after this task: each checkout and worktree-record writer it moves
  publishes `LocalChange::Checkouts` or `LocalChange::Worktrees`, and joins this task's
  machine-state test.
- **056:** nothing new. The transcript flip does not touch the stream.
- **059:** the local forwarder, running even when `AppState.board` is `None`, and
  `plan-pass:progress` emitted by the local pass, which never reaches for a board context.
- **060:** `plan-pass:progress`'s wire form, with the desktop's local pass as its only
  producer.

**The traps, named so nobody finds them twice.**

- **A moved publication with a subscriber left behind** is a queue that no longer wakes when
  started, a card whose skip reason never updates, or a run-window notification that never
  fires. All compile, and all pass every test that only checks the publication. Hence the
  wake tests and Scope 9's per-store rule.
- **Subscribing after sending the burst** loses whatever is published in between. It also
  makes every test's "the burst has arrived, so we are subscribed" false.
- **Checking the team at connect only** is exactly the cached-membership bug ADR-0030 point
  2 rules out, stretched over the life of a stream.

**Size.** L: roughly 2.5–3k lines. About 900 are core (`events.rs`, the `api/events.rs`
subscription, `api/tail.rs`, and moving the publication sites) with their tests, 500 are the
server route and its loopback tests, 300 are shell changes, 250 are frontend edits and their
tests, and 250 are the seam records. If it runs over, cut Scope 7 (revalidation) into its own
commit series at the end of the task, with the route refusing a cookie-authenticated stream
until it lands. Do not cut it out of the task: an unrevalidated stream is the one way this
task could leak. Moving plan progress's wire form into core is the other clean cut, because
no server carries it. The isolation tests and the board/local split cannot be separated.
