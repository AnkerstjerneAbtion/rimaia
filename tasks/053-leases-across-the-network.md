---
id: "053"
title: Leases across the network
milestone: v0.5
status: ready
depends_on: ["052", "070"]
adrs: ["0031", "0037", "0011", "0018", "0029"]
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

- **A connected runner waits for work with a long poll** (ADR-0031 point 2). 042 built the
  wait and 052 serves and clamps it. This task wakes it when a pinned retry falls due, and
  gives the connected runner loop its production use: `wait = CLAIM_WAIT_MAX`, at zero
  capacity too, dropped and re-asked when a slot frees, with backoff.
- **Leases are renewed by heartbeat** (point 3). A connected runner sends one heartbeat
  every 30 seconds, and the board renews every lease it lists to three minutes from now.
- **An expired lease means `interrupted`, and the task stays pinned** (point 4). The server
  closes the run, pins the task to the runner that held it, and fences that runner's later
  reports.
- **A server restart is not evidence that a runner has gone** (point 3). Before it accepts a
  request or sweeps, the server extends every expiring lease by one lifetime.
- **A laptop that slept recovers by itself** (point 4). Its first heartbeat after waking is
  fenced. The runner stops the agent through the normal cancel path, keeps the worktree,
  does not push, and claims the task again through its pin, which resumes the session.

Every one of these is decided against an injected clock. Nothing in this task's tests
sleeps: expiry, grace and a laptop's night are clock advances.

## Why now

052 is the first task in which a runner can hold a lease from another process. Without this
task, a runner that loses its network, crashes or sleeps leaves its task `running` for ever.
Nothing notices absence, and ADR-0031's own alternatives section rejects exactly that
design. Every task after this one assumes a lease ends:

- 057's "run elsewhere" and unpairing release pins that this task is the first to create
  for an absent runner. 054's dirty runner report rides this task's heartbeat tick.
- 056's outbox resends reports that may arrive after an expiry. D31 point 12 decides how a
  resend meets a fence, so the fence has to exist first.
- 058's headless runner and 059's connected desktop start the heartbeat loop and the
  connected claim loop this task writes. Before them neither has a production caller.
- The end-of-M4 manual smoke run includes "a sleep/wake resume", which is this task's
  behaviour on real hardware.

Doing it before any of those exist keeps the hard part, which is ordering between a
heartbeat, a sweep, a stale report and a re-claim, provable in `rimaia-core` against a
`TestClock`. It is not debugged across two laptops.

## Scope

**1. The numbers, in one place.** `crates/core/src/board/lease.rs` is 043's lease module. It
holds `LEASE_LIFETIME` and `LeaseTerm::Renewable(d)` (043), and `HEARTBEAT_INTERVAL` and
`CLAIM_WAIT_MAX`, both 30 seconds (052, for `mark_seen`'s throttle and the claim's clamp).
The server already builds its board with `LeaseTerm::Renewable(LEASE_LIFETIME)`, and 052's
online check reads the same constant. This task uses all four and adds no duration of its
own. `LeaseTerm::Never` stays solo's, and its `expires_at` stays NULL (ADR-0031 point 5).

**No SQL statement in this task compares `expires_at` as text.** It is TEXT, and a
lexicographic comparison is right only if every writer used one serialization. Rows are
read with `DateTime<Utc>` and compared in Rust. The table holds one row per running
process, so this costs nothing.

**2. Expiry is a fact about the clock, not about the sweep.** A lease whose `expires_at` is
at or before `ctx.clock.now()` has expired, whether or not anything has noticed yet. Two
functions in `board/lease.rs` act on it:

```rust
/// Ends the lease as an expiry does. The caller has decided that it ends. Never
/// commits, never publishes. Ok(None) when no row with this generation exists.
pub(crate) async fn expire_in(conn: &mut SqliteConnection, clock: &dyn Clock,
    task_id: &str, generation: i64, pin: bool) -> Result<Option<Ended>>;
/// BEGIN IMMEDIATE, re-read, expire_in(.., pin: true) only if the row still has
/// this generation and has expired, commit, then publish.
pub async fn expire(ctx: &ServiceContext, task_id: &str, generation: i64) -> Result<()>;
```

Because `expire` re-reads inside its transaction, a second caller is a no-op, not a second
interruption. `expire_in` does, in one connection:

- **A lease with a `runs` row** (purpose `implementation`, `review` or `fix`, and `run_id`
  set): the run is closed with the interrupted outcome and `resume_after` from
  `attempts::history` and `retry::decide`, with no run window. That is the decision
  `scheduler::reconcile::interrupted_after` makes today, so it is called, or moved beside
  043's per-runner reconcile, and never copied. The run is closed and the task landed
  through the connection-taking body of 043's lease-fenced `finish_run`. If 043 left that
  body inseparable from its transaction, extract it first, with no behaviour change, in its
  own commit. The run's `error_message` is exactly
  `format!("{label} stopped reporting, and its lease on this run expired")`, where `label`
  is `runners.label`. The task lands as `reconcile::settle` lands it: `waiting_retry` with a
  due deadline while ADR-0011's budget allows, otherwise `failed`. A waiting review or fix
  resumes as its own kind (D29 point 3, `ResumePoint`).
- **A lease with no `runs` row** (a claim whose reply never arrived, or a runner that died
  between claim and `start_run`): the task goes where `release` sends it, `running` to
  `failed`. D31 point 4 gives that rule. No run row is invented.
- **A `strategy` lease** (D17's planner, which has no `runs` row): `run_state` is left
  alone, as `release` leaves it.
- **The pin is decided here, not by `finish_run`'s body**, whose own pin step is skipped on
  this path. `lease::pin` gains a second caller. A pin to the lease's runner is written
  when all of these hold:
  - `pin` is true;
  - the lease had a run, or was `strategy`, because the worktree or the agent session
    exists on that machine and nowhere else;
  - the runner's owner is still a member of the task's team, read in this connection. 051
    expires a removed member's leases and clears their pins, and relies on this check so
    that the expiry does not pin the task back to a runner that can no longer reach the
    team. 057 adds the parallel guard on `runners.unpaired_at`.

  Otherwise no pin is written, and an existing pin from an earlier attempt is left as it
  was.
- The lease row is deleted. `tasks.lease_generation` is not bumped here; the next claim
  bumps it, as 043 does. With no row left, any report carrying the old generation is
  `Conflict`.
- It returns `Ended { team_id, task_id, run_id }`, and its caller publishes
  `ChangeEvent::tasks` and `ChangeEvent::runs` for that team after its own commit
  (ADR-0018).

057's `unpair_runner` calls `expire_in(.., pin: false)` for each of the runner's live leases,
inside its existing transaction and whatever their `expires_at`.

**Every path that checks a lease honours expiry.** 043's fence, `current`, runs inside the
transaction of the write it guards. It gains a third answer, crate-private: **expired**, for
a lease that matches by generation and runner but whose `expires_at <= now`. A method that
gets it rolls back its own transaction, calls `expire(ctx, task_id, generation)`, and only
then returns `Conflict`, exactly as for a stale generation. Running `expire_in` on the
method's connection instead would roll the interruption back with the method's own error.
One helper in `lease.rs` does this for all eight lease-bearing methods, so none repeats it.

**The heartbeat** collects its expired entries, commits its renewals, then expires each one
in its own transaction through `expire`, and returns them in `fenced`. A report that arrives
between `expires_at` and the next sweep is therefore refused, just like one that arrives
after the sweep. Without this, a test's answer would depend on which background task ran
first.

**3. The sweep is how the server notices runners that never come back.**
`lease::sweep(ctx)` finds every expired lease and calls `expire` for each one. A failure on
one lease is logged and does not stop the rest, as `reconcile_interrupted` treats a bad row.

- **Two functions read across teams, and both say so in their docs:** `due(ctx)`, which
  returns `(task_id, team_id, generation)` and nothing else, and `grant_restart_grace`
  (point 4). Expiry belongs to no member of any team. Both take a `&ServiceContext`, never
  a pool, so 039's `no_service_takes_a_pool_without_a_scope` still holds. Every other write
  runs under `ctx.with_scope(TeamScope::one(team_id))`, through the same scoped services as
  every other caller.
- **The sweep acts for nobody.** Task 070 widened `ServiceContext::actor` to
  `Actor { User(UserId), Server }`, routed every read of it through `actor.user()?`, and
  made 046's base context `Actor::Server`. This task uses that and changes nothing about
  it. **Nothing on the expiry or grace path reads the actor**: `expire_in`, `finish_run`'s
  connection-taking body, `reconcile::settle`, `lease::pin` and the membership check read
  the lease's runner and its owner, never `ctx.actor`. If one of them turns out to call
  `actor.user()?`, a sweep fails `internal` on every lease, so that call is a bug to remove
  here, not a reason to give the sweeper a user.
- **The sweeper's context** is 046's base context, `BoardHost.context`, which 070 left as
  `Actor::Server`, re-sourced to `MutationSource::System`. 046's rule still holds:
  `dispatch` replaces it before any handler runs, and the sweep replaces its scope per
  lease. `grant_restart_grace` runs on the same context.
- **The loop lives in `rimaia-server`**, in `crates/server/src/leases.rs`. It waits on
  `Clock::sleep_until(min(earliest expires_at, now + LEASE_LIFETIME))` and never on
  `tokio::time::sleep`. The cap is `queue.rs`'s `DEADLINE_CAP` argument again: a `tokio`
  timer does not measure a suspended host. A heartbeat moves the earliest expiry later, and
  the next wake re-reads it. The rule lives in `rimaia-core`, and the server crate holds
  only the loop (ADR-0006, and D33 point 2: the server holds no query macros).

**4. Restart grace.** `lease::grant_restart_grace(ctx)` reads every lease with a non-NULL
`expires_at` and, in one transaction, raises each one earlier than `now + LEASE_LIFETIME` to
that value. A later lease is left alone, and a solo lease (NULL) is not touched.

ADR-0031 point 3 says the server "extends every lease by one lifetime" because "its own
downtime is not evidence that a runner has gone". `expires_at + 3 min` would still expire
every lease after a ten-minute deploy, which treats the downtime as exactly that evidence.
Raising to `now + lifetime` is the reading under which the reason holds, and it never
shortens a lease. It also makes ADR-0037 point 3's case ("a deploy that takes longer than a
lease lifetime interrupts running work") rarer than that ADR describes: a runner that is
still working when the server comes back keeps its lease. The D31 amendment records this,
and neither ADR is edited.

**Grace runs before anything can check a lease.** Point 2 makes every lease check expire
lazily, so a heartbeat reaching a new server before grace would expire the lease the
downtime aged. `crates/server/src/lib.rs` gains `pub async fn prepare(host: &BoardHost) ->
Result<()>`, which awaits `grant_restart_grace`. `main.rs` calls it after `db::migrate` and
before it binds the listener, and the sweeper is spawned only after it returns. A failure
exits non-zero with a message naming the step, as 046's other startup failures do (D11).

**5. The heartbeat, board side.** 043's `board::service::heartbeat` is extended:

- It renews every listed lease that is current, unexpired and held by **the calling
  runner** to `now + LEASE_LIFETIME`. A `Never` lease stays NULL.
- Every other entry is returned in `fenced`: stale, expired (after `expire`), unknown,
  another runner's or another team's. The reply never distinguishes these, so a runner
  cannot use its heartbeat to ask whether a task id exists (ADR-0029 point 5). 043's fence
  answers `NotFound` for the last two, and the heartbeat folds that into `fenced`.
- It writes `runners.app_version` when the adapter supplied one. `HttpBoard` adds
  `appVersion` (`env!("CARGO_PKG_VERSION")` of the runner binary) to the heartbeat body, the
  way D31 point 10 adds `ProviderId` to the bodies that need it. D28 names this column
  "reported by heartbeat (053)". The in-process adapter passes none.
- **`last_seen_at` stays 052's.** `board::runners::mark_seen`, called from every runner
  route and throttled to once per `HEARTBEAT_INTERVAL`, is its one writer. The heartbeat adds
  no second write. The in-process adapter never calls it: in solo nobody else reads
  presence, and a `wait: ZERO` loop would otherwise write the row on every pass.
- **It publishes no change event.** One write every 30 seconds per runner, turned into one
  SSE message every 30 seconds per member's browser, is the noise ADR-0018 exists to avoid.
  Expiry, which changes run state, does publish.
- `cancel` is 052's: `board::cancel::CancelRequests`, keyed by `(task_id, generation)`,
  listed on every heartbeat from the holding runner until the lease ends, so a lost reply
  costs one more heartbeat and not the cancel. This task does not change that store, and
  only adds the runner's reaction (point 7).

**6. The long poll, board side: a delta on 042's body.** 042's `Next` body already
subscribes, tries, waits on a change event or `Clock::sleep_until(deadline)`, and tries
again. 052 already clamps `wait`. This task adds:

- **The team filter.** A `ChangeEvent` whose `team_id` is outside the context's scope is
  ignored without a re-read.
- **`RecvError::Lagged`** is read as "look again", and `RecvError::Closed` answers `None`.
- **A due retry wakes it.** The wait is `Clock::sleep_until(min(deadline, earliest_due))`.
  `earliest_due` is the earliest `resume_after` among the tasks selection skipped only
  because they were not yet due, which is the deadline `queue.rs`'s `step` computes today.
  A pinned retry that falls due at 03:12 is claimed at 03:12, when no event fires.
- **Dropping the future while it waits writes nothing.** The wait holds no transaction and
  no row. A subscription taken before the selection read is 042's and stays.

**7. The runner side: `crates/runner/src/heartbeat.rs` and `crates/runner/src/fence.rs`.**

- **The cadence is a pure function.**
  `next_heartbeat(last_ok, last_attempt_failed, now) -> DateTime<Utc>`. A successful
  heartbeat is followed by the next one `HEARTBEAT_INTERVAL` of clock time later. A failed
  one is retried at the next wake. The loop sleeps on
  `Clock::sleep_until(min(next_heartbeat, now + HEARTBEAT_WAKE_CAP))`, with
  `HEARTBEAT_WAKE_CAP = 5 s`. This is the cap-and-re-read that `SystemClock`'s doc requires
  of every caller. A laptop that wakes after four hours sends its heartbeat within five
  seconds, not 30 seconds of monotonic time later.
- **A request that never completes is bounded.** After a wake, a heartbeat on a half-open
  connection would otherwise hang, and the loop would never see the fence. Each heartbeat is
  raced against `Clock::sleep_until(sent + HEARTBEAT_TIMEOUT)`, `HEARTBEAT_TIMEOUT = 10 s`.
  `HttpBoard` also gets per-request timeouts, replacing 052's "none of its own": `claim` at
  `CLAIM_WAIT_MAX + 15 s`, so the server always answers first unless the network drops;
  `heartbeat` at `HEARTBEAT_TIMEOUT`; every other method at `REQUEST_TIMEOUT = 30 s`. A
  timeout is a failed heartbeat, retried at the next wake, and never a fence.
- **A connected runner heartbeats whether or not it holds anything.** An empty `held` is a
  presence heartbeat. ADR-0031 point 3 says "while it holds anything". This task sends one
  while connected at all, because two decisions already need presence from an idle runner:
  D28's `app_version` is read for ADR-0037 point 5's out-of-date list, and ADR-0031 point 7
  refuses Run now for an offline runner. ADR-0028 point 1's load estimate, one request per
  active runner every 30 seconds, is unchanged. The D31 amendment records this.
- `held` is read from `runner.db`'s `held_leases` (043) on every beat and rebuilt as
  `LeaseRef`s, team included (D28's reason for `held_leases.team_id`).
- **Each `cancel` entry is a user's cancel.** The loop calls `InFlight::cancel` for it: the
  normal cancel path, then `finish_run` with the cancelled outcome under a lease that is
  still current. The run keeps its metrics, and the task lands where a local cancel lands
  it today.
- **A per-tick hook.** `heartbeat::spawn(board, store, clock, slots, on_tick)` takes
  `on_tick: Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>`, awaited after every
  heartbeat attempt, failed or not. 053 passes a no-op. 054 resends a dirty `report_runner`
  snapshot from it.
- **The loop is not started in solo.** Nothing there expires and nobody else can see
  presence. 058 and 059 call `heartbeat::spawn`. Its tests run it against the in-process
  board built with `LeaseTerm::Renewable(LEASE_LIFETIME)`.
- **The runner never fences itself on its own clock.** An unreachable server is not a
  fence: restart grace may still be holding the lease (ADR-0031 point 3). Only the server's
  answer fences: an entry in `fenced`, or a `Conflict` from any method that takes a lease.

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

**8. The connected runner loop uses the long poll.** 042 left the loop on `wait: ZERO` and
handed this half to this task (042 point 4: "a long poll … and a way to give up a claim that
has not committed when a slot frees"; 052: "a runner loop that polls at zero capacity so it
hears Run now"). `rimaia_runner::queue::build` gains `claim_wait: Duration`. Solo passes
`Duration::ZERO` and is otherwise unchanged. 058 and 059 pass `CLAIM_WAIT_MAX`. With a
non-zero `claim_wait`:

- **The claim is one more arm of the loop's `select!`**, beside 042's releases, signals,
  join and deadline arms, so a waiting claim never blocks a Stop.
- **It claims at zero free capacity too.** 042's early return at `FreeCapacity::total == 0`
  is skipped. 052 answers a zero-capacity `Next` with a relayed Run now or `None`, never
  with a queue pick, so this is how a busy runner hears Run now.
- **A freed slot drops the waiting claim and asks again.** A waiting claim carries the
  `FreeCapacity` of the moment it was sent (D21, 042 point 4). When the releases or join
  arm fires, the loop drops the claim future and sends a new one with fresh capacity. 043's
  claim is one transaction and the wait holds nothing, so a drop before commit costs
  nothing. A drop that races a server-side commit leaves a lease no heartbeat lists, which
  expires as a lease with no `runs` row: the task lands `failed` with no run row. That is
  D31 point 4's recovery, and the same visible cost as 052's post-claim refusal.
- **Errors back off on the injected clock** (ADR-0037 point 3: "long polls … reconnect
  with backoff"). `next_claim_after(consecutive_failures, now)` is a pure function: one
  second, doubling, capped at `CLAIM_WAIT_MAX`, reset by any answer. It applies to `io`,
  `internal` and `database`. `unauthenticated` and `upgrade_required` end the loop's
  claiming and are handed to the host, whose reaction is 058's and 059's.

**9. The records.**

- **A dated amendment under D31** records the refinements this task needs and D31 does not
  state:
  - the presence heartbeat, `appVersion` in its body, and no change event for it;
  - expiry as `expires_at <= now` at every check, compared in Rust; `expire_in` and
    `expire`, the fence's expired answer, and a method that abandons its transaction
    before expiring;
  - restart grace as "raise to `now + lifetime`", run before the listener binds, and that
    this makes ADR-0037 point 3's interruption case rarer than that ADR describes;
  - a lease the caller does not hold answered as `fenced`;
  - the pin rule's departures from 043: `lease::pin` gains a second caller, `expire_in`; an
    expired `strategy` lease pins (060 relies on it); a lease with no `runs` row adds no pin;
    no pin to a runner whose owner left the team (051), or that is unpaired (057);
  - the connected loop's `claim_wait`, zero-capacity polling and dropped claims;
  - the runner never fencing itself.

  Nothing already in D31 is edited.
- **The "How to use this" table** gains the 053 row: D4 · D6 · D8 · D9 · D11 · D14 ·
  D17 · D19 · D21 · D28 · D29 · D31 · D32 · D33.
- **CLAUDE.md.** 043's must-test entry for the lease protocol gains expiry, restart grace and
  sleep recovery, alongside claim, fencing, pinning and per-runner reconcile (ADR-0031's
  consequences).
- **The offline cache.** The new queries (`due`, `expire_in`, grace, the heartbeat's
  `app_version` write) are all in `rimaia-core`. Regenerate both caches with D33's recipe.
  No crate, dependency or CI step is added.

## Out of scope

- **Anything 043 owns:** the one-transaction claim, generation fencing on each method,
  `tasks.lease_generation`, the pin rule for `waiting_retry`, only the pinned runner
  claiming, `held_leases`, per-runner startup reconciliation, the `Conflict` code,
  `LEASE_LIFETIME` and `LeaseTerm`. This task calls them. If one is missing, stop and ask,
  rather than writing it here under another name.
- **Anything 052 owns:** `mark_seen` and its throttle, `HEARTBEAT_INTERVAL`,
  `CLAIM_WAIT_MAX` and the clamp, `RelayedRequests`, `CancelRequests`, and who may cancel.
- **The `Actor` widening** (070): the enum, `actor.user()?` at every use, `for_caller`'s
  `Actor::User`, and the server base context's `Actor::Server`. This task reads the last of
  these and writes none of it.
- **Holder, pin and last seen on the board DTO.** 061 adds `holder: Option<Holder>` and
  `pinned_runner: Option<RunnerRef>`, with `RunnerRef.last_seen_at`, through its batched
  reads and its D12 amendment. 061 is their first reader, and one task owns the shapes.
- **Releasing a pin.** "Run this elsewhere", a fenced worktree's `fenced_at`, the push
  postcondition, and pins released on unpairing are all 057's. After this task, a task
  pinned to a runner that never returns waits for it. ADR-0031 says the server never moves
  a pinned task on its own.
- **Resends and the outbox** (056). Until then `finish_run` keeps its "already finalized"
  refusal. D31 point 12's resend-before-fence ordering is 056's.
- **Starting the heartbeat loop or a connected queue in a binary** (058, 059).
- **The card, the panel, a runners list and the out-of-date list** (061, 063).
  `app_version` is written here and read nowhere yet.
- **Protocol-version refusal** (046's `UpgradeRequired`) and what a refused runner does
  about it (058, 059).
- **Metrics:** active leases and claim latency are 062's (ADR-0037 point 7).
- **Any migration.** `runners.app_version` is D28's (038). `runner_leases`,
  `pinned_runner_id` and `lease_generation` are 043's. If a column is missing, D4's
  amendment makes that a stop-and-ask.

## Acceptance criteria

- `HEARTBEAT_INTERVAL` and `CLAIM_WAIT_MAX` sit beside 043's `LEASE_LIFETIME` in
  `crates/core/src/board/lease.rs`. No lease, heartbeat, claim-wait or online-check code
  under `crates/{core,runner,server}/src` restates any of the three as a literal.
- **Board-port contract cases** (D31 point 13), added to
  `crates/core/src/testing/board_contract.rs`. They pass through
  `crates/core/tests/board_port_in_process.rs`, with runners on
  `LeaseTerm::Renewable(LEASE_LIFETIME)`, and through
  `crates/runner/tests/board_port_http.rs`.
  One `TestClock` drives everything, `FakeCli` is not involved, and nothing sleeps.
  - `an_expired_lease_closes_its_run_as_interrupted_and_pins_the_task`: A claims and
    starts. The clock advances `LEASE_LIFETIME + 1s`, and `lease::sweep` runs on
    `harness.board()`. The run has `status = 'interrupted'`, `exit_class = 'interrupted'`
    and the exact `error_message` above. The task is `waiting_retry` with a due
    `resume_after`, `pinned_runner_id` is A, and no lease row remains.
  - `a_second_interruption_past_the_budget_lands_the_task_failed`.
  - `an_expired_lease_with_no_run_fails_the_task_and_adds_no_pin`.
  - `an_expired_strategy_lease_leaves_run_state_alone_and_pins_the_task`.
  - `an_expiry_after_a_member_is_removed_does_not_pin_to_their_runner`: A's owner is removed
    through 051's `remove_member`, and a sweep closes the run as `interrupted` with
    `pinned_runner_id` NULL.
  - `a_report_after_expiry_is_conflict_even_before_any_sweep`: `finish_run` with A's
    generation, after the clock passes `expires_at`, with no sweep called, answers
    `Conflict`. The run is `interrupted`, not whatever A reported, and the pin is written.
  - Two extensions of 043's
    `the_heartbeat_renews_current_leases_and_fences_stale_ones_per_lease`:
    `…_and_fences_an_expired_one`, in which one lease has crossed `expires_at`, is fenced
    and interrupted, and the other is still renewed; and
    `…_and_answers_unknown_and_cross_team_leases_as_stale`, in which a made-up task id and a
    lease naming another team's task both come back in `fenced`, the replies cannot be told
    apart from a stale one, and nothing is written.
  - `restart_grace_extends_every_expiring_lease_one_lifetime_from_now`: a lease expired
    ten minutes ago is not expired by a sweep that runs after `grant_restart_grace`.
  - `restart_grace_never_shortens_a_lease_and_never_touches_a_solo_one`.
  - `only_the_pinned_runner_claims_the_next_attempt_after_an_expiry`: B's `claim(Next)`
    returns `None`. A's returns the task with `resume` naming the interrupted run's
    `session_id`, and a generation greater than the expired one.
  - **`a_runner_that_slept_is_fenced_on_waking_and_resumes_through_its_pin`**: A claims
    and starts. The clock jumps 4 hours, and the board is not touched meanwhile. Then A's
    first heartbeat lists the task in `fenced`; A's `finish_run` with the old generation is
    `Conflict`; B cannot claim it; and A's `claim(Next)` resumes the session.
  - Three extensions of 042's waiting cases:
    `a_waiting_next_claim_wakes_when_a_pinned_retry_falls_due` (no event is published, and
    the advance to `resume_after` alone produces the claim);
    `a_waiting_next_claim_ignores_another_teams_change`; and
    `a_waiting_next_claim_that_is_dropped_leaves_the_board_untouched` (the future is dropped
    mid-wait, and a task then moved to `ready` is still `idle` with no lease row).
- **`expire` is idempotent:** `expiring_twice_interrupts_once`. Two calls, and a sweep
  racing a heartbeat, leave one closed run, one pin and one pair of change events.
- **Expiry and grace act as the server.** Every contract case above that calls
  `lease::sweep`, `expire` or `grant_restart_grace` calls it on a context with 070's
  `Actor::Server` and `MutationSource::System`, never on a user's context, so a hidden
  `actor.user()?` on that path fails the suite as `Internal`. The heartbeat's lazy expiry
  runs on the runner caller's context, and the cases that exercise it pass the same way.
- No `Actor` type, method or construction site is added or changed in this task (070's).
  Outside tests, `grep -rn 'Actor::Server' crates src-tauri/src` finds only the sites 070
  left: the sweeper takes the base context's actor and does not spell its own.
- **The server** (`crates/server/tests/leases.rs`, with a `TestClock` and a board in a
  `TempDir`):
  - `restart_grace_is_granted_before_the_server_accepts_a_request`: the board holds a lease
    that expired ten minutes before start. The test calls `prepare`, serves the router on
    `127.0.0.1:0`, and at once sends a heartbeat for that lease through `HttpBoard`. It is
    renewed, not fenced, the run is still open, and `expires_at` is `now + 3 min`.
  - `the_sweeper_expires_a_lease_nobody_renewed`: the clock is advanced past `expires_at`.
    The test awaits the task's `ChangeEvent`, under `TEST_TIMEOUT` as a hang guard, never a
    sleep, and the run is `interrupted`.
  - `a_heartbeat_records_the_runners_app_version_and_publishes_nothing`.
- **The runner** (`crates/runner/tests/heartbeat.rs`). Real `FakeCli` children replay
  fixtures from `crates/core/tests/fixtures/cli/`, real git runs in `TempRepo`s, the
  in-process board uses `LeaseTerm::Renewable(LEASE_LIFETIME)`, and one `TestClock` drives
  both sides. Calls are awaited through a delegating spy over the real board, never by
  sleeping.
  - `next_heartbeat` unit tests: `HEARTBEAT_INTERVAL` after a success, and immediately after
    a failure. A four-hour jump makes it due at once.
  - `an_idle_connected_runner_sends_presence_heartbeats`: the spy sees a heartbeat with an
    empty `held` every `HEARTBEAT_INTERVAL` of clock time.
  - `a_heartbeat_that_never_answers_is_abandoned_and_the_next_one_is_sent`: the spy never
    answers one heartbeat. After the clock passes `HEARTBEAT_TIMEOUT`, the next heartbeat
    reaches the spy, and nothing was fenced.
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
  - `a_cancel_in_a_heartbeat_stops_the_run_and_finishes_it_as_cancelled`: the spy adds the
    task to one heartbeat's `cancel`. The run row carries the fixture's metrics, and the
    task lands where a local cancel lands it.
  - `the_runner_never_fences_itself_while_the_server_is_unreachable`: the spy fails every
    heartbeat, and the clock passes ten lifetimes. After the third failed heartbeat has
    been answered, the slot is still held, its signal has not fired, and the child is
    alive.
  - `on_tick_runs_after_every_heartbeat_attempt_including_a_failed_one`.
- **The connected loop** (`crates/runner/tests/queue_connected.rs`, same rules):
  - `a_connected_loop_starts_a_task_moved_to_ready_without_the_clock_moving`: the spy sees
    one claim with `wait = CLAIM_WAIT_MAX`, and the `TestClock` is never advanced.
  - `a_freed_slot_drops_the_waiting_claim_and_asks_again`: one free slot in repository A
    and none in B; B's run finishes, and B's `ready` task is claimed by a new claim
    carrying B's slot. The dropped claim left no lease row.
  - `a_connected_loop_at_zero_capacity_collects_a_relayed_run_now`, over 052's HTTP
    harness.
  - `next_claim_after` unit tests, and `a_connected_loop_backs_off_after_io_errors`: claims
    the spy fails with `io` are spaced 1, 2 and 4 seconds of clock time apart, capped at
    `CLAIM_WAIT_MAX`, and the spacing resets after an answer.
  - 042's solo loop tests pass unchanged, with `wait: ZERO`.
- **One reaction:** no function in `crates/runner/src/` other than `fence::on_fenced`
  matches on `ErrorCode::Conflict`.
- The D31 amendment exists, dated, and no existing sentence in D31 or in ADR-0031 is edited.
  The "How to use this" table has the 053 row.
- **No migration is added** in either set. Both `.sqlx` caches are regenerated with D33's
  recipe, and they are committed.
- **Every CI check passes**, with `SQLX_OFFLINE=true` exported: the full command block in
  CLAUDE.md as 052 leaves it. That includes `cargo test` and `cargo clippy --all-targets`
  for `rimaia-core`, `rimaia-runner` and `rimaia-server`, `npm run typecheck`,
  `npm run test`, `npm run build`, `cargo fmt --all --check`,
  `cargo check --workspace --all-targets` and `./scripts/check-command-wiring.sh`.

## Notes

**Read first.** ADR-0031 in full: points 2 to 5 are this task, and point 6 is why the
usage-limit pause survives a fence. ADR-0037 point 3 is the deploy case that grace narrows,
and the source of claim backoff. ADR-0011 gives the interrupted-run budget. ADR-0018 says why
the heartbeat publishes nothing. ADR-0029 point 5 is why fenced answers are
indistinguishable. Seam entries:

- **D31**, points 3, 4 (`heartbeat`, `release`, `claim`'s `wait`), 9, 10, 11 (the one
  reaction), 12 (why resends are 056's) and 13 (the contract suite, and 053's line in it).
- **D28**: the `runners` DDL (`app_version`), 043's `runner_leases` file, and `held_leases`.
- **D29**: `ResumePoint`, and the purpose a review or fix lease carries.
- **D32**, with 070's amendment: `for_caller`, the actor it sets, and the base context the
  sweeper reuses, which acts for nobody.
- **D17**: the planner, whose `strategy` lease has no `runs` row.
- **D21**: why a waiting claim's `FreeCapacity` goes stale.
- **D11**: `prepare` is a startup step that can fail.
- **D9** and its amendment: what `interrupted` means, and where the task lands.
- **D19**: the local slot. **D14**: nothing here touches the tail.
- **D8**: `Conflict` is 043's, and this task adds no code. **D33**: the recipe.
- **D4's amendment** and **D6**: no file and no dependency here.

**Files to start from.** These are on `main` @ 728a049:

- `crates/core/src/clock.rs` (`sleep_until`, and the doc on why every caller caps its wait);
- `crates/core/src/testing/clock.rs` (`TestClock::set` resolves every pending waiter);
- `crates/core/src/scheduler/queue.rs` (`DEADLINE_CAP`, `step`'s deadline);
- `crates/core/src/scheduler/reconcile.rs` (`interrupted`, `interrupted_after`, `settle`);
- `crates/core/src/scheduler/retry.rs` and `attempts.rs`;
- `crates/core/src/runner/outcome.rs` (`finish_run`);
- `crates/core/src/runner/process.rs` (`CancelSignal`, `DEFAULT_GRACE_PERIOD`, the cancel
  path);
- `crates/core/src/scheduler/inflight.rs`;
- `crates/core/src/tasks/run_state.rs` (the `Running -> Failed` edge `release` uses);
- `crates/core/src/context.rs` and `events.rs`;
- `crates/core/src/testing/cli.rs` (`FakeCli::hangs`, `argv`);
- `crates/core/tests/fixtures/cli/` (`resume-success.jsonl`, `usage-limit.jsonl`).

These exist only once earlier tasks on this branch have landed:

- `crates/core/src/board/{port,types,service,in_process,lease,relay,runners}.rs` (036, 043,
  052);
- `crates/core/src/testing/board_contract.rs` (036, 043) and
  `crates/core/tests/board_port_in_process.rs`;
- `crates/runner/src/board/http.rs` and `crates/runner/tests/board_port_http.rs` (052);
- `crates/server/src/{main,lib,runner_api}.rs` (046, 052);
- `crates/runner/src/queue/` (042) and `held_leases` (043).

**What earlier tasks provide.**

- **038:** the scoped context; `ChangeEvent::team_id`; `runners.app_version`.
- **042:** the runner loop, `claim(Next)`'s selection and its tested wait, and `LocalSlot`.
- **043:** `board/lease.rs`, `LEASE_LIFETIME`, `LeaseTerm`, generation fencing, `Conflict`,
  pinning on `waiting_retry`, only-the-pinned-runner claims, `held_leases` and per-runner
  reconcile.
- **046:** the server crate, `main.rs`'s startup, `BoardHost` and its base context.
- **051:** expiring a removed member's leases, which relies on point 2's membership check.
- **052:** the runner routes, `HttpBoard`, the HTTP contract harness, `mark_seen`, the
  two constants and the clamp, `RelayedRequests`, `CancelRequests` read by the heartbeat,
  and the online check.
- **070:** `Actor { User, Server }`, every read of the actor through `actor.user()?`, and
  `BoardHost.context` as `Actor::Server`, which is the context the sweeper and grace use.

**What the next tasks expect.**

- **054** resends a dirty `report_runner` from `on_tick`. The heartbeat body stays `held`
  plus `appVersion`.
- **056:** its outbox resends a report that may meet this task's fence. D31 point 12 puts
  the resend check before the generation check, and that ordering is 056's to add inside the
  same check this task extends.
- **057** calls `expire_in(.., pin: false)` from `unpair_runner`, adds the `unpaired_at`
  guard to the pin rule, releases pins and fences a moved worktree. Its push postcondition
  must stay unreachable from a fenced run, which this task's sleep-recovery test asserts.
- **058 and 059** call `heartbeat::spawn` and build the queue with
  `claim_wait = CLAIM_WAIT_MAX`.
- **060** relies on an expired `strategy` lease pinning its task.
- **061** adds `holder` and `pinned_runner` to the board DTO and renders them. **062** counts
  active leases and reads `LEASE_LIFETIME` for its connected gauge.

**The residual, stated.** Between a laptop waking and its first heartbeat, the woken agent
keeps working in its own worktree on a lease the server has already ended. That window is at
most `HEARTBEAT_WAKE_CAP` plus one round trip, or `HEARTBEAT_TIMEOUT` more if a heartbeat was
in flight when the machine slept. The work is on the branch of a task pinned to this runner,
which nobody else may claim, and the resumed session finds it there. Nothing is lost, and
nothing lands on another machine's branch.

**Size.** L, and just under the line. With the DTO moved to 061, cancel delivery left with
052, and the `Actor` widening cut out to 070:

| Part | Lines |
| --- | --- |
| `lease.rs`: `expire_in`, `expire`, the fence's expired answer, sweep, grace | ~400 |
| Heartbeat and long-poll deltas, server `prepare` and sweeper | ~250 |
| Runner: `heartbeat.rs`, `fence.rs`, `CancelSignal::fence`, `HttpBoard` timeouts | ~450 |
| Runner: the connected loop and its backoff | ~250 |
| Contract cases and server tests | ~1,150 |
| Runner heartbeat and connected-loop tests | ~1,100 |
| Records and caches | ~100 |
| **Total** | **~3,700** |

The first cut has been made: the `Actor` widening, about 350 lines of churn through 039, 045,
046, 048 and 051's readers and writers, is task 070, ordered directly before this one. That
leaves about 100 lines of margin under the 3,800 one session carries, which is thin. If the
diff still passes about 3,800 lines, stop and propose the next split rather than trimming
tests. The natural one is the connected loop (Scope 8 and `queue_connected.rs`, about 600
lines with its tests): 058 and 059 are its first production callers, and of the parts here
it is the least entangled with expiry. Propose it and ask; do not make the cut unasked.

Never cut expiry, grace, the long poll or sleep recovery. Those are the task.
