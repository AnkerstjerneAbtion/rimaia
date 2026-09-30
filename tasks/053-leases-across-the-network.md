---
id: "053"
title: Leases across the network
milestone: v0.5
status: ready
depends_on: ["052"]
adrs: ["0031"]
size: L
---

# Leases across the network

## Goal

Make a lease mean what
[ADR-0031](../docs/adr/0031-runners-claim-work-with-leases.md) says it means when the runner
holding it is on another machine. Task 043 made the claim one transaction, fenced every
report by generation and pinned retries. Task 052 put the port on HTTP. Both still assume
the runner comes back. This task handles the case where it does not, and the case where it
comes back after the board has moved on:

- **Waiting for work is a long poll** (ADR-0031 point 2). `claim(Next { wait })` holds for up
  to 30 seconds and answers as soon as something becomes eligible. It is woken by the same
  `ChangeEvent`s that refresh the board, and by the next pinned retry falling due.
- **Leases are renewed by heartbeat** (point 3). A connected runner sends one heartbeat every
  30 seconds. The heartbeat renews every lease it lists, in one transaction, to three
  minutes from now.
- **An expired lease means `interrupted`, and the task stays pinned** (point 4). The server
  closes the run, pins the task to the runner that held it, and fences that runner's later
  reports.
- **A server restart is not evidence that a runner has gone** (point 3). At start, before
  its first expiry sweep, the server extends every expiring lease by one lifetime.
- **A laptop that slept recovers by itself** (point 4). Its first heartbeat after waking is
  fenced. The runner stops the agent through the normal cancel path, keeps the worktree,
  does not push, and claims the task again through its pin, which resumes the session.
- **The board can say who holds a task and when that runner was last heard from**
  (ADR-0031's consequences). The summary and the detail carry it. Rendering it is 061's.

Every one of these is decided against an injected clock. Nothing in this task's tests
sleeps: expiry, grace and a laptop's night are clock advances.

## Why now

052 is the first task in which a runner can hold a lease from another process. Without this
task, a runner that loses its network, crashes or sleeps leaves its task `running` for ever.
Nothing notices absence, and ADR-0031's own alternatives section rejects exactly that
design. Every task after this one assumes a lease ends:

- 054's checkout mapping and 057's "run elsewhere" release pins that this task is the first
  to create for an absent runner.
- 056's outbox resends reports that may arrive after an expiry. D31 point 12 decides how a
  resend meets a fence, so the fence has to exist first.
- 058's headless runner and 059's connected desktop start the heartbeat loop this task
  writes. Before them it has no production caller, the way several of D31's methods had
  none when 036 shipped them.
- The end-of-M4 manual smoke run includes "a sleep/wake resume", which is this task's
  behaviour on real hardware.

Doing it before any of those exist keeps the hard part, which is ordering between a
heartbeat, a sweep, a stale report and a re-claim, provable in `rimaia-core` against a
`TestClock`. It is not debugged across two laptops.

## Scope

**1. The numbers, in one place.** `crates/core/src/board/leases.rs` is 043's lease module.
If 043 kept its lease SQL in `board/service.rs`, this task moves it into `leases.rs` first,
with no behaviour change, in its own commit. The module gains:

```rust
pub const LEASE_LIFETIME: Duration = Duration::minutes(3);   // ADR-0031 point 3
pub const HEARTBEAT_INTERVAL: Duration = Duration::seconds(30);
pub const CLAIM_WAIT_MAX: Duration = Duration::seconds(30);  // ADR-0031 point 2
```

043's `LeaseTerm` gets the variant the server uses, `LeaseTerm::Expiring`, which writes
`expires_at = now + LEASE_LIFETIME` on every claim and renewal. `LeaseTerm::Never` stays
solo's, and its `expires_at` stays NULL (ADR-0031 point 5).

**2. Expiry is a fact about the clock, not about the sweep.** A lease whose `expires_at` is
at or before `ctx.clock.now()` has expired, whether or not anything has noticed yet. One
function, `leases::expire(ctx, task_id, generation)`, acts on it in one transaction:

- It re-reads the lease row inside the transaction, and does nothing unless the row still
  has that generation and has still expired. That makes a second caller a no-op, not a
  second interruption.
- **A lease with a `runs` row** (purpose `implementation`, `review` or `fix`, and `run_id`
  set): the run is closed through `runner::outcome::finish_run` with the interrupted outcome
  and `resume_after` from `attempts::history` and `retry::decide`, with no run window. This
  is exactly the decision `scheduler::reconcile::interrupted_after` makes today, so it is
  called, or moved to `board::service` beside 043's per-runner reconcile, and never copied.
  The run's `error_message` is exactly
  `format!("{label} stopped reporting, and its lease on this run expired")`, where `label`
  is `runners.label`. The task then settles as `reconcile::settle` settles it:
  `waiting_retry` with a due deadline while ADR-0011's budget allows, otherwise `failed`.
- **A lease with no `runs` row** (a claim whose reply never arrived, or a runner that died
  between claim and `start_run`): the task goes where `release` sends it, `running` to
  `failed`. D31 point 4 gives that rule. No run row is invented.
- **A `strategy` lease:** `run_state` is left alone, as `release` leaves it for that
  purpose.
- **Pinning.** Every purpose that had a run, and `strategy`, sets
  `tasks.pinned_runner_id` to the lease's runner. The worktree and the agent session exist
  on that machine and nowhere else. A lease with no `runs` row adds no pin, because nothing
  was started to continue, and it leaves an existing pin from an earlier attempt as it was.
- The lease row is deleted. The generation is not bumped here. `tasks.lease_generation` is
  bumped by the next claim, as 043 does. With no row left, any report carrying the old
  generation is `Conflict`.
- `ChangeEvent::tasks` and `ChangeEvent::runs` are published for the task's team, after
  commit (ADR-0018).

**Every path that checks a lease honours expiry.** 043's generation check is extended. A
lease that has expired is expired through `expire` first, and the call then answers
`Conflict` exactly as it would for a stale generation. The heartbeat does the same, per
lease. A report that arrives in the gap between `expires_at` and the next sweep is
therefore refused, just like one that arrives after the sweep. Without this, a test's
answer would depend on which background task ran first.

**3. The sweep is how the server notices runners that never come back.**
`leases::sweep(ctx)` finds every lease whose `expires_at` has passed and calls `expire` for
each one. A failure on one lease is logged and does not stop the rest, which is how
`reconcile_interrupted` already treats a bad row.

- The only read across teams is a private function in `leases.rs`, `due(ctx)`. It takes a
  `&ServiceContext`, never a pool, so 039's `no_service_takes_a_pool_without_a_scope`
  still holds. It returns `(task_id, team_id, generation)` and nothing else. Its doc says
  why it ignores the scope: expiry belongs to no member of any team. Every write happens
  under `ctx.with_scope(TeamScope::one(team_id))`, through the same scoped services every
  other caller uses.
- **The sweep acts for nobody, so the actor is widened, as 038 said this task would.**
  `ServiceContext::actor` becomes `Actor`:

  ```rust
  pub enum Actor { User(UserId), Server }
  impl Actor {
      pub fn as_str(&self) -> &str;         // the id, or "server": for tracing spans only
      pub fn user(&self) -> Result<&UserId>; // Error::internal for Server
  }
  ```

  Every writer of an author column, starting with 045's `created_by`, `plan_updated_by`
  and `review_instructions_updated_by`, takes its value through `actor.user()?`. A
  server-acting context can then never write a blank or invented author. The sweep's
  context is `MutationSource::System` and `Actor::Server`.
- **The loop lives in `rimaia-server`**, in `crates/server/src/leases.rs`, and is started
  from the server's startup, after migrations. It waits on
  `Clock::sleep_until(min(earliest expires_at, now + LEASE_LIFETIME))` and never on
  `tokio::time::sleep`. The cap is `queue.rs`'s `DEADLINE_CAP` argument again: a `tokio`
  timer does not measure a suspended host. A heartbeat moves the earliest expiry later,
  and the next wake re-reads it. The rule lives in `rimaia-core`, and the server crate
  holds only the loop (ADR-0006, and D33 point 2: the server holds no query macros).

**4. Restart grace.** `leases::grant_restart_grace(ctx)` runs one statement in one
transaction:

```sql
UPDATE runner_leases SET expires_at = max(expires_at, ?now_plus_lifetime)
 WHERE expires_at IS NOT NULL
```

The server's startup awaits it **before it spawns the sweeper**, so no sweep can see a lease
the downtime aged. Two readings of ADR-0031's "extends every lease by one lifetime" were
possible, and this is the one under which the ADR's reason holds. `expires_at + 3 min`
would still expire every lease after a ten-minute deploy, and ADR-0037 point 3 says a slow
deploy must not cost running work. `max` never shortens a lease, and a solo lease (NULL) is
not touched. D31's amendment below records this reading, and ADR-0031 is not edited.

**5. The heartbeat, board side.** `board::service::heartbeat` from 043 is extended:

- It renews, in one transaction, every lease in `held` that is current, unexpired and held
  by **the calling runner**. The new `expires_at` is `now + LEASE_LIFETIME` under
  `LeaseTerm::Expiring`, and stays NULL under `Never`.
- Every other entry is returned in `fenced`: stale, expired (after `expire`), unknown,
  another runner's, or another team's. The reply never distinguishes these. A runner
  cannot use its heartbeat to ask whether a task id exists (ADR-0029 point 5).
- It writes `runners.last_seen_at = now`, and `runners.app_version` when the adapter
  supplied one. The HTTP adapter adds `appVersion`
  (`env!("CARGO_PKG_VERSION")` of the runner binary) to the heartbeat body, the way D31
  point 10 adds `ProviderId` to the bodies that need it. The server reads it and passes it
  to the service. D28 names this column "reported by heartbeat (053)".
- `claim` also writes `last_seen_at`. A runner waiting on a long poll is being heard from.
- **Neither publishes a change event.** One write every 30 seconds per runner, turned into
  one SSE message every 30 seconds per member's browser, is the noise ADR-0018 exists to
  avoid. The next board read shows the new time. Expiry, which changes run state, does
  publish.
- `cancel` is filled from `board::cancel::CancelRequests` (point 7 below).

**6. The long poll.** `board::service::claim` for `ClaimTarget::Next { wait, .. }`:

1. `deadline = now + min(wait, CLAIM_WAIT_MAX)`. A longer `wait` is clamped, never
   refused.
2. Subscribe to `ctx.subscribe()` **before** the selection read, never after. That is
   `queue.rs`'s drain-before-read rule, for the same reason: an event between the read and
   the wait must still wake it.
3. Run 042's selection and 043's one-transaction claim. A `Claim` is returned at once.
4. Otherwise, wait on whichever comes first:
   - a `ChangeEvent` whose `team_id` is in the context's scope. An event for another team
     is ignored without a re-read;
   - `RecvError::Lagged`, which is read as "look again";
   - `Clock::sleep_until(min(deadline, earliest_due))`. `earliest_due` is the earliest
     `resume_after` among the tasks the selection skipped only because they were not yet
     due, which is the deadline `queue.rs`'s `step` already computes today. A pinned retry
     that falls due at 03:12 is claimed at 03:12, when no event fires.
5. At the deadline, answer `None`. On `RecvError::Closed`, answer `None`.

**Dropping the future while it waits writes nothing.** The wait holds no transaction and no
row, so a runner that gives up, or a solo loop whose `select!` picks another arm, costs
nothing. The HTTP adapter's per-request timeout for `claim` is `CLAIM_WAIT_MAX + 15 s`, so
the server always answers first unless the network drops. The other methods keep 052's
timeout. The long poll leaves the solo runner loop's own wake sources as 042 left them.

**7. Cancel reaches a remote runner through its heartbeat.** D31 point 4: "apart from the
claim, the heartbeat is the board's only channel to a runner". `CancelRequests` lives in
`crates/core/src/board/cancel.rs`, an in-memory map from `(task_id, generation)`, so the
rule stays in core.

- The server holds one in its state. 052's `cancel_task_run` handler, after deciding who
  may ask, calls `CancelRequests::request(task_id, generation)` for the current lease.
- `heartbeat` lists a task in `cancel` while its request matches a current lease that the
  runner listed in `held`. It keeps listing it until the lease ends, so a lost reply
  costs one more heartbeat and not the cancel. An entry whose lease has ended is dropped
  on the next heartbeat that looks at it.
- The in-process adapter passes an always-empty registry. A solo Cancel reaches
  `InFlight::cancel` directly, as D31 point 4 says.
- The runner treats an entry as a user's cancel: the normal cancel path, then
  `finish_run` with the cancelled outcome, under a lease that is still current. The run
  keeps its metrics, and the task lands exactly where a local cancel lands it today.
- **The registry is memory, deliberately.** A server restart loses a pending request, and
  the person presses Cancel again. The card still says `running`, so nothing is hidden.
  Persisting it would need a column, and the D4 amendment lists every file team mode may
  write. Latency is one heartbeat, at most 30 seconds.

**8. The runner side: `crates/runner/src/heartbeat.rs` and `crates/runner/src/fence.rs`.**

- **The cadence is a pure function.**
  `next_heartbeat(last_ok, last_attempt_failed, now) -> DateTime<Utc>`. A successful
  heartbeat is followed by the next one 30 seconds of clock time later. A failed one is
  retried at the next wake. The loop sleeps on
  `Clock::sleep_until(min(next_heartbeat, now + HEARTBEAT_WAKE_CAP))`, with
  `HEARTBEAT_WAKE_CAP = 5 s`. This is the same cap-and-re-read that `SystemClock`'s doc
  requires of every caller. A laptop that wakes after four hours sends its heartbeat
  within five seconds, not 30 seconds of monotonic time later. That also bounds the time
  a woken agent keeps working on a fenced lease.
- **A connected runner heartbeats whether or not it holds anything.** An empty `held` is a
  presence heartbeat. ADR-0031 point 3 says "while it holds anything". This task sends
  one while connected at all, because two decisions already need presence from an idle
  runner. D28's `app_version` is "reported by heartbeat" and read for ADR-0037 point 5's
  out-of-date list, and a runner idle all day would otherwise never report it. ADR-0031
  point 7 refuses Run now for an offline runner. ADR-0028 point 1's load estimate, one
  request per active runner every 30 seconds, is unchanged. The D31 amendment records
  this.
- `held` is read from `runner.db`'s `held_leases` (043) on every beat and rebuilt as
  `LeaseRef`s, team included (D28's reason for `held_leases.team_id`).
- **The loop is not started in solo.** Nothing there expires and nobody else can see
  presence. `heartbeat::spawn(board, store, clock, slots)` is what 058 and 059 call. Its
  tests run it against the in-process board built with `LeaseTerm::Expiring`.
- **The runner never fences itself on its own clock.** An unreachable server is not a
  fence. Restart grace may still be holding the lease, and ADR-0037 point 3 wants a deploy
  to cost running work nothing. Only the server's answer fences: an entry in `fenced`, or
  a `Conflict` from any method that takes a lease.

**One reaction to a fence: `fence::on_fenced(lease)`.** D31 point 11 gives this function
to this task. It does four things, in this order:

1. It fences the local slot through a new `CancelSignal::fence()`, beside `cancel()`. It
   takes the normal cancel path (`signal_group`, TERM, `DEFAULT_GRACE_PERIOD`, then KILL),
   and records that the reason was a fence.
2. It keeps the worktree. It does not push, and does not reach any code that pushes (057's
   postcondition runs before `finish_run`, which a fenced run never calls).
3. It deletes the lease's `held_leases` row, so the next heartbeat does not list it.
4. It wakes the runner loop so that it claims again through the pin.

`run_task`, seeing a fenced signal, makes **no** further report on that lease: no
`finish_run`, no `release`, no `append_transcript`. Each would be `Conflict`, and the board
has already decided what happened. It keeps the local transcript file. A fenced run whose
CLI reported a usage limit before it was stopped still raises the runner's own pause, from
`usage_limit_resets_at` or the fixed poll. The limit belongs to the subscription, not to the
lease (ADR-0031 point 6).

Every place in `crates/runner` that receives `Conflict` from a lease method calls
`on_fenced`: the heartbeat's `fenced` list, `run_task`'s reports, the planner's
`record_strategy` and `run_context`, and 043's startup reconciliation. There is no second
handler.

**9. Who holds a task, on the board DTO (a D12 amendment).** `TaskSummary` and `TaskDetail`
gain two fields. Both are filled by `LEFT JOIN`s in the one summary query, so a board read
stays one query (D12's argument):

```rust
pub struct RunnerPresence {
    pub runner_id: RunnerId,
    pub label: String,
    pub owner_id: UserId,
    pub owner_login: String,
    pub last_seen_at: Option<DateTime<Utc>>,
}
pub struct LeaseHolder {
    pub runner: RunnerPresence,
    pub purpose: LeasePurpose,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>, // None for a solo lease
}
// TaskSummary, TaskDetail:
pub holder: Option<LeaseHolder>,
pub pinned_to: Option<RunnerPresence>,
```

`src/types.ts` mirrors both types as `RunnerPresence` and `LeaseHolder`, with the fields on
`TaskSummary` and `TaskDetail`, camelCase. No component renders them. That is 061's work,
and "Alice's laptop, last seen 04:12" is its sentence to write. Existing frontend tests
that build a `TaskSummary` gain `holder: null, pinnedTo: null` through their shared
builder, and no assertion changes.

**10. The records.**

- **A dated amendment under D31** records the refinements this task needs and D31 does not
  state:
  - the presence heartbeat;
  - `appVersion` in the heartbeat body;
  - `last_seen_at`'s two writers and the absence of a change event;
  - expiry as `expires_at <= now` at every check;
  - restart grace as `max(expires_at, now + lifetime)`;
  - a lease the caller does not hold answered as `fenced`;
  - `CancelRequests` in memory;
  - the runner never fencing itself.

  Nothing already in D31 is edited.
- **A dated amendment under D12** for `holder` and `pinned_to`.
- **CLAUDE.md.** Check that 043 added the lease protocol to the must-test list. If it did
  not, add it. The entry must name expiry, restart grace and sleep recovery alongside claim
  races, fencing and pinning (ADR-0031's consequences).
- **The offline cache.** The new queries (`due`, `expire`, grace, the heartbeat's writes,
  the summary's joins) are all in `rimaia-core`. Regenerate both caches with D33's recipe.
  No crate, dependency or CI step is added.

## Out of scope

- **Anything 043 owns:** the one-transaction claim, generation fencing on each method,
  `tasks.lease_generation`, the pin rule for `waiting_retry`, only the pinned runner
  claiming, `held_leases`, per-runner startup reconciliation, the `Conflict` code, and
  `LeaseTerm::Never`. This task calls them. If one is missing, stop and ask, rather than
  writing it here under another name.
- **Releasing a pin.** "Run this elsewhere", a fenced worktree's `fenced_at`, the push
  postcondition, and pins released on unpairing are all 057's. After this task, a task
  pinned to a runner that never returns waits for it. ADR-0031 says the server never moves
  a pinned task on its own.
- **Resends and the outbox** (056). Until then `finish_run` keeps its "already finalized"
  refusal. D31 point 12's resend-before-fence ordering is 056's.
- **Starting the heartbeat loop in a binary.** 058 (headless) and 059 (connected desktop)
  call `heartbeat::spawn`.
- **The card, the panel, a runners list, and the out-of-date list** (061, 063).
  `app_version` is written here and read nowhere yet.
- **Protocol-version refusal of a heartbeat or claim** (046's `UpgradeRequired`), and what
  a refused runner does about it (058, 059).
- **Metrics:** active leases and claim latency are 062's (ADR-0037 point 7).
- **Who may cancel a run** (052). This task only carries the request.
- **Any migration.** `runners.last_seen_at` and `app_version` are D28's (038).
  `runner_leases`, `pinned_runner_id` and `lease_generation` are 043's. If a column is
  missing, D4's amendment makes that a stop-and-ask.

## Acceptance criteria

- `LEASE_LIFETIME`, `HEARTBEAT_INTERVAL` and `CLAIM_WAIT_MAX` exist once, in
  `crates/core/src/board/leases.rs`, and every use reads them from there. No other file
  restates any of the three durations as a literal.
- **Board-port contract cases** (D31 point 13), added to
  `crates/core/src/testing/board_contract.rs`. They pass through
  `crates/core/tests/board_port_in_process.rs`, with runners on `LeaseTerm::Expiring`, and
  through `crates/runner/tests/board_port_http.rs`. One `TestClock` drives everything,
  `FakeCli` is not involved, and nothing sleeps.
  - `an_expired_lease_closes_its_run_as_interrupted_and_pins_the_task`: A claims and
    starts. The clock advances `LEASE_LIFETIME + 1s`, and `leases::sweep` runs on
    `harness.board()`. The run has `status = 'interrupted'`, `exit_class = 'interrupted'`
    and the exact `error_message` above. The task is `waiting_retry` with a due
    `resume_after`, `pinned_runner_id` is A, and no lease row remains.
  - `a_second_interruption_past_the_budget_lands_the_task_failed`.
  - `an_expired_lease_with_no_run_fails_the_task_and_adds_no_pin`.
  - `an_expired_strategy_lease_leaves_run_state_alone_and_pins_the_task`.
  - `a_report_after_expiry_is_conflict_even_before_any_sweep`: `finish_run` with A's
    generation, after the clock passes `expires_at`, with no sweep called, answers
    `Conflict`. The run is `interrupted`, not whatever A reported.
  - `a_heartbeat_renews_to_one_lifetime_from_now`: at +2m59s the lease is renewed, and
    `expires_at` is `now + 3 min` exactly.
  - `a_heartbeat_after_expiry_fences_that_lease_and_renews_the_others`.
  - `a_heartbeat_never_renews_a_lease_the_caller_does_not_hold`: B lists A's lease, and a
    made-up task id. Both come back in `fenced`, A's `expires_at` is unchanged, and the
    two answers cannot be told apart.
  - `a_heartbeat_records_when_the_runner_was_last_seen`: after it, `runners.last_seen_at`
    is the clock's now. `claim` does the same. Neither publishes a `ChangeEvent`.
  - `restart_grace_extends_every_expiring_lease_one_lifetime_from_now`: a lease expired
    ten minutes ago is not expired by a sweep that runs after `grant_restart_grace`.
  - `restart_grace_never_shortens_a_lease_and_never_touches_a_solo_one`.
  - `only_the_pinned_runner_claims_the_next_attempt_after_an_expiry`: B's `claim(Next)`
    returns `None`. A's returns the task with `resume` naming the interrupted run's
    `session_id`, and a generation greater than the expired one.
  - **`a_runner_that_slept_is_fenced_on_waking_and_resumes_through_its_pin`**: A claims
    and starts. The clock jumps 4 hours, and the board is not touched meanwhile. Then:
    - A's first heartbeat lists the task in `fenced`;
    - A's `finish_run` with the old generation is `Conflict`;
    - B cannot claim it;
    - A's `claim(Next)` resumes the session.
  - `a_waiting_claim_answers_when_a_task_is_moved_to_ready_without_the_clock_moving`:
    `claim(Next { wait: 30s })` is started while the board has nothing claimable. A task
    is then moved to `ready` through the board's services. The claim returns that task,
    and the `TestClock` was never advanced.
  - `a_waiting_claim_answers_none_at_its_deadline`, and
    `a_wait_longer_than_thirty_seconds_is_clamped`: with `wait: 5 min`, an advance of
    `CLAIM_WAIT_MAX` answers `None`.
  - `a_waiting_claim_wakes_when_a_pinned_retry_falls_due`: no event is published, and the
    advance to `resume_after` alone produces the claim.
  - `another_teams_change_does_not_wake_a_waiting_claim_into_its_task`.
  - `a_waiting_claim_that_is_dropped_leaves_the_board_untouched`: the future is dropped
    mid-wait, and a task then moved to `ready` is still `idle` with no lease row.
- **`expire` is idempotent:** `expiring_twice_interrupts_once`. Two calls, and a sweep
  racing a heartbeat, leave one closed run, one pin and one pair of change events.
- **The server** (`crates/server/tests/leases.rs`, with a `TestClock` and a board in a
  `TempDir`):
  - `the_server_grants_restart_grace_before_its_first_sweep`: the board holds a lease that
    expired ten minutes before start. After the server's startup has returned, the run is
    still open and `expires_at` is `now + 3 min`.
  - `the_sweeper_expires_a_lease_nobody_renewed`: the clock is advanced past `expires_at`.
    The test awaits the task's `ChangeEvent`, under `TEST_TIMEOUT` as a hang guard, never
    a sleep, and the run is `interrupted`.
  - `a_cancel_reaches_the_holding_runner_on_its_next_heartbeat` and
    `a_cancel_request_ends_with_its_lease`.
- **The runner** (`crates/runner/tests/heartbeat.rs`). Real `FakeCli` children replay
  fixtures from `crates/core/tests/fixtures/cli/`, real git runs in `TempRepo`s, the
  in-process board uses `LeaseTerm::Expiring`, and one `TestClock` drives both sides.
  Calls are awaited through a delegating spy over the real board, never by sleeping.
  - `next_heartbeat` unit tests: 30 seconds after a success, and immediately after a
    failure. A four-hour jump makes it due at once.
  - `an_idle_connected_runner_sends_presence_heartbeats`: an empty `held`, and
    `last_seen_at` moves.
  - **`a_runner_that_slept_past_its_lease_stops_its_agent_keeps_the_worktree_and_resumes_the_session`**.
    `FakeCli::hangs` holds attempt 1. The clock is set four hours ahead. Then:
    - the child receives TERM, the first run row is the server's `interrupted` (the
      runner wrote no `finish_run` for it), and `held_leases` no longer lists it;
    - the worktree directory and its commits are unchanged, and the bare remote has no
      task branch;
    - attempt 2 replays `resume-success.jsonl`, and `FakeCli::argv` shows it resumed
      attempt 1's session id in the same worktree path;
    - the task lands in `in_review`, with both runs naming runner A.
  - `a_conflict_on_finish_goes_through_on_fenced_and_is_not_reported_again`.
  - `a_fenced_run_that_hit_a_usage_limit_still_pauses_this_runner`, from
    `usage-limit.jsonl`.
  - `a_cancel_in_a_heartbeat_stops_the_run_and_finishes_it_as_cancelled`: the run row
    carries the fixture's metrics and the task lands where a local cancel lands it.
  - `the_runner_never_fences_itself_while_the_server_is_unreachable`: the spy fails every
    heartbeat, and the clock passes ten lifetimes. After the third failed heartbeat has
    been answered, the slot is still held, its signal has not fired, and the child is
    alive.
- **One reaction:** no function in `crates/runner/src/` other than `fence::on_fenced`
  matches on `ErrorCode::Conflict`.
- **The DTO:** `the_board_summary_names_the_holder_and_the_pinned_runner`, in
  `crates/core/tests/tasks.rs`. A held task carries `holder`, with the runner's label,
  the owner's login, `last_seen_at`, `purpose`, `acquired_at` and `expires_at`. A pinned
  task that nobody holds has `pinned_to` and no `holder`. A solo task holds
  `This computer` with `expires_at` `None`. `src/types.ts` mirrors both types, and
  `npm run typecheck` passes.
- `ServiceContext::actor` is an `Actor`. Every author-column writer reads `actor.user()?`.
  `a_server_context_cannot_write_an_author` shows that a create through an
  `Actor::Server` context is `Internal` and writes nothing.
- The D31 and D12 amendments exist, dated. No existing sentence in either entry, or in
  ADR-0031, is edited.
- **No migration is added** in either set. Both `.sqlx` caches are regenerated with D33's
  recipe, and they are committed.
- **Every CI check passes**, with `SQLX_OFFLINE=true` exported: the full command block in
  CLAUDE.md as 052 leaves it. That includes `cargo test` and `cargo clippy --all-targets`
  for `rimaia-core`, `rimaia-runner` and `rimaia-server`, `npm run typecheck`,
  `npm run test`, `npm run build`, `cargo fmt --all --check`,
  `cargo check --workspace --all-targets` and `./scripts/check-command-wiring.sh`.

## Notes

**Read first.** ADR-0031 in full: points 2 to 5 are this task, and point 6 is why the
usage-limit pause survives a fence. ADR-0037 point 3 is why restart grace is `max`. ADR-0011
gives the interrupted-run budget. ADR-0018 says why the heartbeat publishes nothing. Seam
entries:

- **D31**, points 3, 4 (`heartbeat`, `release`, `claim`'s `wait`), 9, 10, 11 (the one
  reaction), 12 (why resends are 056's) and 13 (the contract suite, and 053's line in it).
- **D28**: the `runners` DDL (`last_seen_at`, `app_version`), 043's `runner_leases` file,
  and part 6's `held_leases`.
- **D9** and its amendment: what `interrupted` means, and where the task lands.
- **D12** and its amendments: the summary is one query.
- **D19**: the local slot. **D14**: nothing here touches the tail.
- **D8**: `Conflict` is 043's, and this task adds no code. **D33**: the recipe.
- **The D4 amendment**: no file here, so a missing column is a stop-and-ask.

**Files to start from.** These are on `main` @ 728a049:

- `crates/core/src/clock.rs` (`sleep_until`, and the doc on why every caller caps its wait);
- `crates/core/src/testing/clock.rs` (`TestClock::set` resolves every pending waiter);
- `crates/core/src/scheduler/queue.rs` (`DEADLINE_CAP`, drain-before-read, `step`'s
  deadline);
- `crates/core/src/scheduler/reconcile.rs` (`interrupted`, `interrupted_after`,
  `settle`);
- `crates/core/src/scheduler/retry.rs` and `attempts.rs`;
- `crates/core/src/runner/outcome.rs` (`finish_run`);
- `crates/core/src/runner/process.rs` (`CancelSignal`, `DEFAULT_GRACE_PERIOD`, the cancel
  path);
- `crates/core/src/scheduler/inflight.rs`;
- `crates/core/src/tasks/run_state.rs` (the `Running -> Failed` edge `release` uses);
- `crates/core/src/tasks/service.rs` (`TaskSummary`, `TaskDetail`,
  `TASK_SUMMARY_SELECT`);
- `crates/core/src/context.rs` and `events.rs`;
- `crates/core/src/testing/cli.rs` (`FakeCli::hangs`, `argv`);
- `crates/core/tests/fixtures/cli/` (`resume-success.jsonl`, `usage-limit.jsonl`);
- `src/types.ts`.

These exist only once earlier tasks on this branch have landed:

- `crates/core/src/board/{port,types,service,in_process}.rs` (036);
- `crates/core/src/board/leases.rs`, or 043's equivalent;
- `crates/core/src/testing/board_contract.rs` (036, 043);
- `crates/core/tests/board_port_in_process.rs`;
- `crates/runner/src/board/http.rs` and `crates/runner/tests/board_port_http.rs` (052);
- `crates/server/src/runner_api.rs` (052);
- the server's startup (046);
- `crates/runner/src/` with the runner loop (042) and `held_leases` (043).

**What earlier tasks provide.**

- **038:** the scoped context with `actor: UserId` and a note that this task widens it;
  `ChangeEvent::team_id`; `runners.last_seen_at` and `app_version`.
- **042:** the runner loop and `claim(Next)`'s selection, returning immediately; the
  local slot (`LocalSlot`).
- **043:** `runner_leases`, generation fencing, `Conflict`, pinning on `waiting_retry`,
  only-the-pinned-runner claims, `held_leases`, per-runner reconcile and `LeaseTerm::Never`.
- **045:** the author columns that `actor.user()?` now guards.
- **046:** the server crate, its startup and its state.
- **052:** the runner routes, `HttpBoard`, the HTTP contract harness, `cancel_task_run`'s
  "who may ask", and Run now for one runner.

If 052's `cancel_task_run` already stores requests somewhere, `CancelRequests` replaces
that store rather than sitting beside it. If 052 already writes `last_seen_at` from its
route layer, the write moves into `board::service` so both adapters share it. There is one
writer.

**What the next tasks expect.**

- **054** reports the checkout set on a method of its own, not on the heartbeat. The
  heartbeat body stays `held` plus `appVersion`.
- **056:** its outbox resends a report that may meet this task's fence. D31 point 12
  puts the resend check before the generation check, and that ordering is 056's to add
  inside the same check this task extends.
- **057** releases pins and fences a moved worktree. Its push postcondition must stay
  unreachable from a fenced run, which this task's sleep-recovery test already asserts.
- **058 and 059** call `heartbeat::spawn` and nothing else from this task.
- **061** renders `holder` and `pinned_to`. **062** counts active leases.

**The residual, stated.** Between a laptop waking and its first heartbeat, at most
`HEARTBEAT_WAKE_CAP` plus one round trip, the woken agent keeps working in its own worktree
on a lease the server has already ended. That work is on the branch of a task pinned to
this runner, which nobody else may claim. The resumed session finds it there. Nothing is
lost, and nothing lands on another machine's branch.

**Size.** L. Roughly 3,000–3,500 lines, over half of them tests: about 17 contract cases,
the server and runner suites, the `leases.rs` rules, the waiting claim, the DTO join and its
TypeScript mirror, and two amendments. That is at the top of what one session carries. If
the diff passes about 3,800 lines, cut in this order:

1. Move the DTO fields (scope 9 and the D12 amendment) to 061, which is their first
   reader. Nothing else in this task depends on them.
2. Move cancel delivery (scope 7) to 057, which already changes how a runner learns that
   a task left it. This needs one dated line in D31 that moves "(053)" in point 4 to 057,
   so ask before doing it.

Never cut expiry, grace, the long poll or sleep recovery. Those are the task.
