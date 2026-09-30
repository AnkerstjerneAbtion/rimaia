---
id: "052"
title: The runner protocol and an HTTP board adapter
milestone: v0.5
status: ready
depends_on: ["051"]
adrs: ["0031", "0034", "0037", "0027"]
size: L
---

# The runner protocol and an HTTP board adapter

## Goal

Put a network between a runner and the board, and prove the network changes nothing. The
server gains `/api/v1/runner/<method>`, one route per `BoardMethod`, authenticated only by a
runner's `rmr_` token and fenced by the lease generation every report already carries.
`rimaia-runner` gains `HttpBoard`, the second adapter of the port task 036 drew
([ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md) point 5).
The contract suite that 036 wrote against the in-process adapter then runs, unedited,
against `HttpBoard` talking to the real `rimaia-server` router on an ephemeral loopback
port. Both answer every case the same way, or this task is not done.

Two smaller things ride on the routes because they cannot land anywhere else:

- **The protocol version is enforced on every runner route** (ADR-0037 point 4). A runner
  outside the server's supported window is refused before it can claim or report anything.
- **Run now, Retry now and Cancel become board commands** (seam-contract D32 appendix, "052,
  local until then"). A person can ask the server to start a task on one specific runner of
  theirs, and to stop a run on whichever runner holds it. Only the owner may start
  (ADR-0031 point 7).

**This task builds no runner loop and no network hardening.** Nothing in production uses
`HttpBoard` yet: the headless binary builds it in 058 and the connected desktop in 059.
Lease expiry, restart grace, sleep recovery, the heartbeat cadence and the one reaction to
`Conflict` are 053's.

## Why now

Task 051 is the last piece of the server a runner needs before it can talk to one: 046 gave
it a router, a `Caller` and a protocol header, 047 gave it runner tokens, 048 gave it a tail
relay, and 051 gave it teams with more than one member, which is the first time "only the
runner's owner" means anything. Every task after this one assumes the routes exist: 053
hardens them for a network that drops, 054 adds `report_runner` to them, 055 adds
`run_tool`, 056 streams transcripts through them, and 058 and 059 are the first production
callers of the adapter.

It has to be one task, because the suite is the point. ADR-0027's Consequences make the
contract suite "what keeps 'works in solo' meaning 'works connected'". A route landed
without the adapter has nothing to prove it against, and an adapter landed without the
routes can only be tested against a mock of the server, which proves the mock. D31 point 13
already decided where the suite runs over HTTP; this task is the one that makes it pass.

The three run controls flip now because this is the task that can deliver them. D32 point 8
forbids a board classification before there is an honest mechanism behind it. Until a runner
can hear from the server, "start this on Alice's laptop" has no mechanism. From this task it
does: a request the server holds for that runner, collected by its next claim.

## Scope

Read D31 in full before starting, then D32 points 2, 3, 5, 7 and 8. Everything below
refines them. Where this task decides something they leave open, it records the decision as
a dated amendment in the same commit (last item of this section).

**1. The routes.** `crates/server/src/runner_api.rs` (D31 point 10).

- The router is built by iterating `BoardMethod::ALL`. Each variant maps to its handler
  through one exhaustive `match`, so a variant added without a handler does not compile.
  Every route is `POST /api/v1/runner/<BoardMethod::as_str()>`, nested under 046's
  `/api/v1` with its JSON fallback, so an unknown runner method is
  `404 {"code":"not_found",…}` and never the web bundle's `index.html`.
- Bodies are the D31 DTOs, parsed from bytes the way 046's board routes parse them. A
  malformed body is `invalid` in the error shape, never axum's plain-text rejection.
  Replies are `200` with JSON. A unit reply is `null`. Errors are D8's `{ code, message }`
  with D32 point 3's status table.
- `append_transcript` is the one non-JSON body (ADR-0034 point 2). Its query string carries
  the whole `LeaseRef` (`taskId`, `teamId`, `generation`) plus `runId` and `offset`, and
  the body is the raw bytes. D31 point 10 names only `run_id`, `generation` and `offset`.
  The handler calls the same service function as every other lease method, and that
  function takes a whole `LeaseRef`, so the query carries all of it. Deriving the task and
  team from `runId` instead would give this one method a scope that no other method has.
  This is recorded in the D31 amendment. The body limit stays axum's default until 056
  chooses its chunk size and raises it on this route alone (D34).
- Every handler resolves the caller, builds a context from it, and makes **one call** into
  the same `board::service` function that `InProcessBoard` calls. Handlers make no
  decisions of their own. The exception is `claim`, which goes through the relay function
  in point 5.
- `publish_tail`'s handler hands the message to the context's tail relay, which is 048's
  fan-out. It answers `null` even when nobody is subscribed.
- The server builds its board side with 043's finite `LeaseTerm` (ADR-0031's three
  minutes), so an HTTP claim writes `expires_at` and a heartbeat renews it. Nothing acts on
  an expired lease until 053's sweep.

**2. The caller: `RunnerCaller`.** In `crates/server/src/caller.rs`, next to 046's
extractor (D32 point 7's table).

- `RunnerCaller(Caller)` accepts only `Authorization: Bearer rmr_…`, resolved through 047's
  `Authenticate`, and only when the result is `Door::Runner { runner_id }`. Any other
  credential on a runner route is `unauthenticated`: a valid `rmd_` token, an `rmp_` token,
  a session cookie, both a cookie and a bearer, or nothing.
- A token whose runner has `unpaired_at` set is `unauthenticated`, whatever 047 does to the
  token row at unpairing.
- Every runner handler takes `RunnerCaller` as its first extractor. There is no other way to
  mount a runner route, which is D32 point 7's rule carried over to this table.
- **The context.** It is `ServiceContext::for_caller(&caller)`: the scope is every team the
  runner's owner belongs to, read from `team_memberships` on this request (ADR-0030 point
  2), the actor is the owner, and the source is `System` (D32 point 7's mapping). A
  method that takes a `LeaseRef` then runs under `TeamScope::one(lease.team_id)`, after
  checking that the team is in the caller's scope. A `teamId` outside the scope is
  `not_found`. The service's own check that the lease's task is in that team is
  unchanged (D31 point 3: the team is a claim to verify, never an input).
- **Last seen.** Every authenticated runner request sets `runners.last_seen_at` to the
  clock's now. It is one conditional `UPDATE … WHERE last_seen_at IS NULL OR last_seen_at <
  ?`, so the column is written at most once per ADR-0031's 30-second heartbeat interval.
  A runner publishing a tail several times a second must not become a stream of writes
  that Litestream replicates (ADR-0037 point 2). The function lives in `rimaia-core`
  (`board::runners::mark_seen`), not in the extractor, so the same rule has one home.

**3. The protocol version.** Every runner route refuses a request whose `Rimaia-Protocol`
header is missing, unparsable, older than the oldest minor version the server supports, or
newer than its own (ADR-0037 point 4). The refusal is 046's `upgrade_required`, with 046's
message naming the minimum version.

- **The whole runner protocol refuses, reads included.** D32 point 7 answers `Read` board
  commands at any version, because a person must still be able to see their board.
  Nothing on this table is read by a person. `preview` and `run_context` exist only to
  compose a run that is about to be claimed or reported. A stale runner that could read
  but not report would compose and spawn a run whose result it could not deliver, and
  that is exactly the outcome ADR-0037 forbids ("stops taking work rather than taking it
  and misreporting it").
- The check uses 046's parser and 046's supported window. There is no second copy of
  either.
- Apply the checks in the order 046's board routes apply them: caller first, then version.
  If 046 ordered them the other way, follow 046 and say so in the amendment. Both surfaces
  must answer a request with neither credential nor header the same way.

**4. The adapter: `HttpBoard`.** `crates/runner/src/board/http.rs` (D31 point 10).

- `HttpBoard::new(origin, token, provider)` holds one `reqwest::Client`, the server
  origin, the runner token and the runner's `ProviderId`. `rimaia-runner`'s manifest turns
  on reqwest's TLS feature, spelled `rustls` on the 0.13 line, at this use site and never
  on the workspace line (D34).
- **It refuses to send a token in clear.** An `http://` origin whose host is not loopback
  is `Error::invalid` at construction. Loopback over plain HTTP stays allowed, because the
  test harness and a same-machine server use it.
- **The token never appears in `Debug`, in an error message or in a span.** Use 047's
  secret type if it has one. Otherwise use a newtype whose `Debug` prints `rmr_…`.
- Every request sends `Authorization: Bearer rmr_…` and `Rimaia-Protocol:
  <rimaia_core::api::PROTOCOL_VERSION>`. Bodies are `serde_json::to_vec` with an explicit
  `Content-Type`, so reqwest's `json` feature stays off (D34).
- It adds the `ProviderId` to the `preview`, `claim` and `run_context` bodies, as a field
  of the wire body and not of the trait's arguments. The server resolves the catalogue for
  that provider (D32 point 2: the runner protocol never reads `BoardHost.provider`).
- **Errors are rebuilt from the body's `code`, never from the status.** That covers all
  eight codes: `invalid`, `not_found`, `database`, `io`, `internal`, `conflict`,
  `unauthenticated` and `upgrade_required`. A reply that is not an error body, such as a
  proxy's HTML 502, is `internal`, and its message names the status and the origin. A
  connection that cannot be made is `io` and names the origin. Neither message contains the
  token.
- **It does not retry** (D31 point 10). 056's outbox resends reports, and callers ask again
  for reads.
- **`publish_tail` is synchronous and cannot fail** (D14, D31 point 4). It `try_send`s into
  a bounded channel of 64 messages, and one drain task posts them one at a time. A full
  channel drops the message. A failed post is logged at `debug` and dropped. The drain task
  is spawned at construction and ends when the adapter drops.
- `wait` on `ClaimTarget::Next` is sent as-is. The adapter sets no request timeout of its
  own; choosing one that outlives the server's hold is 053's.

**5. `claim` over HTTP answers what `claim` in process answers.** The claim route calls
the same function for `Next` that the in-process adapter calls, including its wait on
`ServiceContext::subscribe` and `Clock::sleep_until`. The server clamps `wait` at ADR-0031
point 2's 30 seconds. D31 point 4 wrote "in process" because 042 built the wait for the
in-process adapter first. The suite compares the two adapters, and a wait honoured by one
and ignored by the other is exactly the difference the suite exists to find. What 053 adds
is everything that keeps a held request alive across a real network: client timeouts,
backoff, what a server shutdown does to held requests, and restart grace.

**6. Run now, Retry now and Cancel, for one runner.** ADR-0031 point 7, and D32 appendix
rows `start_task_run`, `retry_task_now` and `cancel_task_run`.

*The relay.* A new core module, `crates/core/src/board/relay.rs`, holds one trait:

```rust
pub trait RunnerRelay: Send + Sync + 'static {
    /// Start `request.task_id` on `request.runner_id`, or refuse with a sentence.
    fn run_now<'a>(&'a self, ctx: &'a ServiceContext, request: RunNowRequest)
        -> BoardFuture<'a, ()>;
    /// Ask the runner holding `task_id`'s lease to stop it. Idempotent.
    fn cancel<'a>(&'a self, ctx: &'a ServiceContext, runner_id: &'a str, task_id: &'a str)
        -> BoardFuture<'a, ()>;
}

pub struct RunNowRequest {
    pub task_id: String,
    pub team_id: String,
    pub runner_id: String,
    pub continue_session: bool, // Retry now
    pub requested_by: String,   // users.id
}
```

`BoardHost` gains `relay: Arc<dyn RunnerRelay>`. D32 point 2 lets `BoardHost` gain a field
only through the task that needs it, and this is that task. It has two implementations:

- **Solo: `ShellRelay`, in `src-tauri/src/relay.rs`.** `run_now` is the body of today's
  `start_task_run` and `retry_task_now` shell commands: 036's starter function
  (`preview`, D19's slot, the opt-in, `negotiate`, `probe_cli`, then
  `claim(Run { trigger: Manual, continue_session })`), followed by spawning `run_task`.
  `cancel` is `InFlight::cancel`.
  - It answers synchronously, with today's sentences, byte for byte.
  - Task 008's and ADR-0026's "refused before any run state is written" still hold in
    solo, with no new state in between.
  - A solo desktop cannot tell that the command moved.
- **Server: `RelayedRequests`, in `crates/core/src/board/relay.rs`.** It holds requests in
  server memory, keyed by runner.
  - **`run_now` refuses an offline runner.** A runner is online when `last_seen_at` is
    within ADR-0031's three-minute lease lifetime. Use 043's constant if it defined one,
    otherwise add `board::runners::LEASE_LIFETIME` here, and 053 uses it for expiry.
  - **Otherwise it records one request per task.** A second press replaces the first and
    does not add another.
  - **A request expires.** One that no claim collected within one lease lifetime is
    dropped, measured on the injected clock.
  - **`cancel` records the task against the holding runner.** It is returned once, on that
    runner's next `heartbeat`, in D31's `Heartbeat::cancel`, and then forgotten. A
    heartbeat from any other runner never sees it.

*Why the requests live in memory and not in a column.* A Run now is a button press for a
runner that is online at that moment, and the runner should collect it within seconds.
If the server restarts first, the press is lost. The card is unchanged, and the person
presses again. If the press were persisted, it would start work hours later, on a machine
whose owner has since moved on. D28 keeps sign-in state in memory for the same kind of
reason, and D4 reserves no migration for this task.

*Collection.* `board::relay::claim_next(ctx, relayed, target)` is what the claim route
calls for `Next`. Before any queue selection, it takes this runner's oldest unexpired
request. It then claims it through the ordinary `claim(Run { trigger: Queued,
continue_session })` path, in one transaction, with every eligibility and consent check
that path applies (045). If the claim is lost or refused, the request is dropped and
selection proceeds as usual.

- **Capacity does not apply** (D19 point 5, ADR-0031 point 7). A `Next` with zero free
  capacity is answered with a relayed request if there is one, and with `None` otherwise,
  never with a queue pick. That is how a busy runner still hears a Run now. Its loop polls
  at zero capacity, which is 053's and 058's to wire.
- **A relayed Run now is unattended**, so the claim carries `RunTrigger::Queued`. A person
  who pressed a button in a browser or on another machine is not at the runner, and
  ADR-0031 point 7 says that run "needs ADR-0032's consent and runs as ADR-0012's
  unattended run".
- **The interactive path is the runner's own claim.** `claim(Run { trigger: Manual, … })`,
  sent over a runner's own `rmr_` token, is accepted as an interactive run. Only that
  machine holds the token, so the request is its owner at that machine by construction. 059
  decides whether the connected desktop's own Run now button takes this path or the
  relay. The server accepts both.
- **A runner-side refusal after collection releases** (D31 point 5). This is the
  queue's existing behaviour. For a relayed run it means the card lands on `failed` with no
  run row. That cost is named under Notes, not fixed here.

*The board handlers.* The three handlers are in `crates/core/src/api/board/runs.rs`, and
each is a registry row flipped from `local` to `board` with `Effect::Write`. **One flip per
commit** (D32 point 8). Each commit moves the handler, deletes the `#[tauri::command]` and
its `generate_handler!` entry, and switches the `commands.ts` wrapper from `local<T>` to
`board<T>`. `check-command-wiring.sh` fails a commit that does one without the others.

- **`start_task_run { taskId, runnerId? }` and `retry_task_now { taskId, runnerId? }`.**
  - **Which runner.** When `runnerId` is absent, the caller's door decides. For
    `Door::Shell` it is the solo runner (038's `solo_identity`), so today's invoke payload
    `{ taskId }` is unchanged. For any other door it is `invalid`.
  - **What the board checks first**, in this order, before calling the relay:
    1. the task is in the caller's scope, otherwise `not_found`;
    2. the runner exists and is not unpaired;
    3. the runner belongs to the caller;
    4. for a retry, the task is not pinned to a different runner (043's
       `tasks.pinned_runner_id`).
  - **Then** `host.relay.run_now`.
- **`cancel_task_run { taskId }`.** The task must be in scope, otherwise `not_found`. If
  the task holds no lease, the command is a no-op, which is today's idempotence. Otherwise
  the caller may ask when they own the runner that holds the lease, or are an owner of the
  task's team. D32's appendix leaves "who may ask" to this task.
  - **Why owners too:** stopping a run is a board action on the team's work, like deleting
    the task. It costs the runner's owner nothing they did not already share by taking the
    task. Starting a run is different, because it spends someone's machine and
    subscription at a moment they did not choose. ADR-0031 point 7's refusal is about
    starting.
  - **Why not members:** a member who is neither of these is refused, as D32 point 3 shapes
    role refusals.

*The refusals, exact.* Each one is `Error::invalid` with this sentence, and a test asserts
it byte for byte:

| Case | Sentence |
| --- | --- |
| No runner named, over HTTP | `name the runner to start this task on` |
| Runner unpaired | `this runner has been unpaired and can no longer start runs` |
| Runner belongs to someone else | `only the owner of this runner can start a run on it; assign the task to them, or leave it ready for their queue` |
| Runner offline | `this runner has not been in touch for more than three minutes, so it cannot start a run now` |
| Retry of a task pinned elsewhere | `this task is pinned to another runner, which holds its worktree and session; retry it there` |
| Cancel by a member who owns neither | `only the owner of the runner holding this task, or an owner of its team, can stop its run` |

A runner that does not exist, or that belongs to nobody the caller shares a team with, is
`not_found`, never one of the sentences above (ADR-0029 point 5).

*The wrappers.* `startTaskRun` and `retryTaskNow` in `src/lib/commands.ts` gain an optional
`runnerId`, which they send only when it is given. The runner picker is 061's. Existing
callers and the 31 test files that mock `@tauri-apps/api/core` keep sending `{ taskId }`.

**7. The bundle cap.** `board::service::finish_run` refuses a `FinishRun` whose bundle patch
is longer than 033's `PATCH_CAP_BYTES`, with `invalid`. The runner already caps what it
computes, so this only ever refuses a runner that is buggy or not the one it claims to be.
It lives in the service, not the handler, so both adapters apply it (ADR-0006).

**8. The HTTP harness.** `crates/runner/tests/board_port_http.rs` invokes
`board_contract!(HttpHarness)` (D31 point 13).

- `rimaia-server` becomes a dev-dependency of `rimaia-runner`, the direction ADR-0027 point
  6 allows. 046's check that the server does not depend on the runner must still pass.
- `HttpHarness::start()` sets up, in order:
  - a `TestClock`;
  - a board database from `testing::db`;
  - a `BoardHost` over them, with `RelayedRequests` and 047's real `Authenticate`;
  - one user in one team, and runners `A` and `B` that the user owns, paired through 047's
    core token functions, never through SQL;
  - the real `rimaia-server` router, served with `axum::serve` on a
    `TcpListener::bind("127.0.0.1:0")`;
  - one `HttpBoard` per runner, pointed at that port.
- `Harness::board()` returns a context scoped to that team, for arranging and inspecting.
  `Harness::clock()` is the same `TestClock` the server reads.
- **The cases are not edited.** If a case reaches past `Harness` and fails only over HTTP,
  fix the harness or the adapter, never the case. If the case itself is wrong, stop and
  say so. 036's Notes predicted this.
- Nothing sleeps. A wait is a clock advance. The listener and its server task are dropped
  with the harness.

**9. Records.**
- A dated amendment to **D31**. It records: `append_transcript`'s query carrying the whole
  `LeaseRef`; `wait` honoured by both adapters (point 5); the protocol refusal covering
  runner reads (point 3); `mark_seen` and its throttle; the relay, relayed requests
  collected by `claim(Next)` and cancels delivered on `heartbeat`; and the two Run now
  postures.
- A dated amendment to **D32**. It records: `BoardHost::relay`; the three rows flipped,
  with their `From` cells unchanged; and the cancel rule.
- A row in the seam-contract "How to use this" table for 052: D8, D14, D19, D28, D29, D31,
  D32, D33 and D34.
- CLAUDE.md's Gotchas gains one bullet: "The runner protocol (`/api/v1/runner/*`) is not in
  the command registry. Its registry is `BoardMethod::ALL`, and a method added there gets a
  route, an `HttpBoard` body and a cross-team case in the same commit."

## Out of scope

- **Everything about a network that drops.** That is 053: the heartbeat cadence and loop,
  lease expiry and the sweep that closes runs as `interrupted`, restart grace, sleep
  recovery, the one `Conflict` reaction (D31 point 11), request timeouts and backoff, and
  the runner's reaction to `Heartbeat::cancel`.
- **`report_runner`**, checkouts by remote, doctor reports and credential rekeying: 054. It
  adds its method to `BoardMethod::ALL`, and point 1's `match` then makes it add a route.
- **`run_tool`** and the run-scoped proxy: 055.
- **Storing transcripts on the server**, the outbox, resend idempotence (D31 point 12) and
  `append_transcript`'s body limit: 056. Until then, the server's `append_transcript`
  acknowledges through the service function exactly as the in-process adapter does, and
  keeps nothing. No production HTTP runner exists before 058, so nothing is lost.
- **The push postcondition, "run elsewhere", and releasing pins at unpairing:** 057.
- **The headless binary and `rimaia-runner pair`:** 058. **The connected desktop**, meaning
  `AppState.board = None`, an `HttpBoard` in `lib.rs`, and which path its own Run now
  takes: 059.
- **Any interface change** beyond the optional wrapper argument: 061 picks the runner and
  shows pinned and requested cards.
- **Rate limits on runner routes.** 047 limits the sign-in, pairing and token endpoints
  that ADR-0030 names. A runner route is already behind a 256-bit token.
- **No migration.** D4 reserves none for this task, and nothing here needs a column.

## Acceptance criteria

**The routes and the caller**
- `crates/server/src/runner_api.rs` exists.
  `every_board_method_has_exactly_one_runner_route` iterates `BoardMethod::ALL`: each
  route, called with no credentials, answers `401 unauthenticated`.
  `POST /api/v1/runner/set_run_state` answers `404 {"code":"not_found",…}` with a JSON
  body.
- `a_runner_route_accepts_only_a_runner_token`, over one lease method and one lease-less
  method. A desktop token, a personal token, a session cookie with its CSRF header, and a
  cookie plus a bearer are each `unauthenticated`. The runner's own token is served.
- `a_revoked_runner_token_is_refused_on_its_next_request` and
  `an_unpaired_runners_token_is_refused`.
- `a_runner_sees_only_its_owners_teams`: `preview` of a task in a team the owner does not
  belong to is `not_found`, and so is a `LeaseRef` whose `teamId` names that team.
  `every_runner_route_has_a_cross_team_case` iterates `BoardMethod::ALL` and fails for a
  method with no such case (ADR-0029 point 5's registry test, for this surface).
- `a_runner_request_marks_the_runner_seen_at_most_once_per_heartbeat_interval`. With a
  `TestClock`, two requests 10 s apart write `last_seen_at` once, and a third at 31 s
  writes it again.

**The protocol version**
- On a lease-less route and on a lease route alike, each of these is `426
  upgrade_required` with 046's minimum-version message:
  `a_runner_request_without_the_protocol_header_is_refused`,
  `a_runner_two_minor_versions_behind_is_refused`, `a_runner_ahead_of_the_server_is_refused`.
  `a_runner_one_minor_version_behind_is_served` passes.
- `a_stale_runner_cannot_read_a_run_context_either`: `run_context` and `preview` refuse
  with `upgrade_required` at an unsupported version.

**The adapter**
- `HttpBoard` implements `BoardPort` for every method in `BoardMethod::ALL`.
  `every_runner_request_carries_the_token_and_the_protocol_version` iterates the methods
  against a stub axum router that records headers.
- `the_adapter_rebuilds_every_error_code_from_the_body_not_the_status`: a stub answers each
  of the eight codes with a deliberately mismatched status, and the rebuilt `Error::code()`
  equals the body's `code`.
- `a_reply_that_is_not_an_error_body_is_internal_and_names_the_status`, and
  `an_unreachable_server_is_io_and_names_the_origin`. Neither message, and neither `Debug`
  of the adapter, contains the token string
  (`the_runner_token_is_never_in_an_error_or_debug_output`).
- `the_adapter_refuses_plain_http_to_a_host_that_is_not_loopback`, and accepts
  `http://127.0.0.1:<port>`.
- `the_adapter_ignores_a_reply_field_it_does_not_know`: forward compatibility, since no DTO
  uses `deny_unknown_fields` (D31 point 6).
- `a_tail_published_with_a_full_buffer_is_dropped_and_publish_never_blocks`, and
  `a_published_tail_reaches_a_subscriber_through_the_server`.
- `the_adapter_sends_its_provider_on_preview_claim_and_run_context`, and the catalogue in
  the reply is that provider's.

**The contract suite**
- `crates/runner/tests/board_port_http.rs` invokes `board_contract!` over the real server
  router on `127.0.0.1:0`, and every case passes: 036's lifecycle, 038/039's cross-team
  case, 042's `Next` cases, 043's race, fencing, generation and heartbeat cases, and
  045's eligibility cases, whatever the suite holds at 051's tip. `git diff` shows no change
  to any existing case in `crates/core/src/testing/board_contract.rs`.
- `crates/core/tests/board_port_in_process.rs` still passes, including the new case below.
- New contract case, run by both adapters:
  `a_finish_whose_patch_exceeds_the_cap_is_invalid`. Its patch is `PATCH_CAP_BYTES + 1`,
  and the run stays open.
- `every_lease_method_refuses_a_stale_generation` passes over HTTP with `409 conflict` on
  the wire and `ErrorCode::Conflict` rebuilt by the adapter.

**Run now, Retry now, Cancel**
- The three registry rows are `board`/`Write`. `check-command-wiring.sh` passes. There are
  no `#[tauri::command]` definitions for them in `src-tauri/src/commands/runs.rs`, and
  `commands.ts` calls `board<T>` for all three.
- **Solo is unchanged.** The existing shell-level tests of manual start and retry pass
  through `ShellRelay`, with the same assertions:
  `a_missing_claude_binary_is_refused_before_any_run_state_is_written`,
  `a_repository_that_has_not_opted_in_cannot_start_a_task`, and
  `a_lost_manual_start_answers_with_todays_sentence` with its retry twin. Their names do not
  change. The frontend suite passes with no edit to any `toHaveBeenCalledWith` of the three
  commands.
- Server-side behaviour tests, over HTTP, with a faked clock and no `sleep`:
  - `a_run_now_request_is_collected_by_that_runners_next_claim_and_by_no_other`: runner B's
    `claim(Next)` returns `None`, and A's returns the task.
  - `a_relayed_run_now_is_claimed_unattended`: `Claim::trigger` is `Queued`.
  - `a_runner_claiming_its_own_task_by_hand_is_an_interactive_run`:
    `claim(Run { trigger: Manual })` over A's token yields `Claim::trigger == Manual`.
  - `a_collected_run_now_ignores_the_runners_capacity`: a `Next` with zero free capacity
    returns the relayed task, and without a request returns `None` even with `ready` tasks.
  - `pressing_run_now_twice_records_one_request`.
  - `a_run_now_nobody_collected_is_dropped_after_one_lease_lifetime`: the clock is advanced
    three minutes and a second, and `Next` returns no relayed task.
  - `a_relayed_run_now_that_is_no_longer_eligible_is_dropped_and_selection_continues`.
  - `retry_now_through_a_relay_resumes_the_session`: `Claim::resume` is set.
  - `a_cancel_reaches_the_holding_runner_on_its_next_heartbeat_and_only_once`: B's
    heartbeat never sees it.
  - `cancel_with_nothing_running_is_a_no_op`.
  - `a_team_owner_can_stop_a_members_run`.
  - One test per sentence in the refusal table, asserting the exact string, plus
    `start_task_run_for_a_runner_outside_every_shared_team_is_not_found`.
- `commands.test.ts`: `startTaskRun` and `retryTaskNow` send `runnerId` only when given.

**Records and CI**
- D31 and D32 carry the dated amendments listed in Scope 9. The "How to use this" table has
  the 052 row. CLAUDE.md has the Gotchas bullet.
- `rimaia-runner`'s manifest enables reqwest's `rustls` feature, and the workspace line
  still has `default-features = false`. `cargo tree -d` shows one `axum` and one `reqwest`.
  No crate enters `Cargo.lock` that D34 does not name.
- Both offline caches are regenerated with D33's recipe for the queries this task adds
  (`mark_seen`, the online read and the runner-owner read), and committed.
- Every CI check passes, as CLAUDE.md lists them at 051's tip. That includes
  `cargo test -p rimaia-runner` on all three operating systems, which now builds
  `rimaia-server` and the TLS stack as dev-dependencies.

## Notes

**Read first:**
- [ADR-0031](../docs/adr/0031-runners-claim-work-with-leases.md), all of it, and point 7
  twice;
- [ADR-0034](../docs/adr/0034-one-api-for-the-web-and-the-desktop.md) points 2 and 7;
- [ADR-0037](../docs/adr/0037-hosting-backups-and-version-skew.md) point 4;
- [ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md) points
  5 and 6;
- ADR-0029 point 5, ADR-0030 points 3 and 5, and ADR-0032 points 4 and 5, for the checks
  the claim already applies.

**Seam entries:**
- **D31 in full.** Points 3, 4, 10, 11 and 13 are this task's.
- **D32** points 2, 3, 5, 7 and 8, and the appendix rows for the three run controls.
- **D28:** `runners`, `api_tokens`, `runner_leases` and `tasks.pinned_runner_id`.
- **D29** point 9: `StartRun::kind` crosses the wire.
- **D33:** both caches.
- **D34:** reqwest's feature spelling, and nothing else to add.
- **D8**, **D10**, **D14** (a dropped tail costs nothing) and **D19** point 5 (manual
  starts ignore caps).
- **D4 and D6** as prohibitions.

**Files to start from.** These exist on `main` today:
- `src-tauri/src/commands/runs.rs`: `start_task_run`, `retry_task_now` and
  `cancel_task_run`, whose bodies become `ShellRelay`;
- `src-tauri/src/lib.rs`: `setup()` and the handler list;
- `src-tauri/src/state.rs`;
- `crates/core/src/scheduler/inflight.rs`: `cancel` and `acquire_unbounded`;
- `crates/core/src/error.rs`: `ErrorCode`, and the `Serialize` the adapter reverses;
- `crates/core/src/context.rs`;
- `crates/core/src/testing/context.rs` and `testing/clock.rs`;
- `src/lib/commands.ts` and `src/lib/commands.test.ts`;
- `scripts/check-command-wiring.sh`.

These are created by earlier tasks on this branch:
- `crates/core/src/board/{port,types,service,in_process}.rs` and
  `crates/core/src/testing/board_contract.rs` (036);
- `crates/core/src/runner/start.rs`, the starter (036);
- `crates/runner/` (040);
- `crates/core/src/api/{mod,registry,caller}.rs`, `crates/core/src/api/board/runs.rs`,
  `crates/server/src/{lib,caller}.rs` and `src-tauri/src/commands/board.rs` (046);
- the token and `Authenticate` code (047);
- the tail relay on `BoardHost` (048).

**Migration:** none.

**What the chain hands this task.**
- **036:** the port, `InProcessBoard`, the starter function, and a suite whose cases touch
  only `Harness`.
- **040:** `crates/runner`.
- **042:** `ClaimTarget::Next` and its wait.
- **043:** leases, generation fencing, `Conflict`, pins and `LeaseTerm`.
- **045:** eligibility and consent inside `claim`.
- **046:** the router, the registry, `Caller`, the protocol header and `upgrade_required`.
- **047:** `rmr_` tokens, `Authenticate`, and reqwest's TLS feature in the lockfile.
- **048:** the tail relay.
- **051:** team roles, so "an owner of its team" can be checked.

**What the next tasks expect.**
- **053** hardens what this task serves:
  - timeouts and backoff around `claim(Next)`;
  - a runner loop that polls at zero capacity so it hears Run now;
  - the heartbeat loop and its reaction to `cancel`;
  - expiry, using the same lease-lifetime constant as the online check here;
  - the one `Conflict` reaction.
- **054** adds `report_runner` to `BoardMethod::ALL`, and gets a route, an adapter body and
  a cross-team case from this task's structure.
- **055** adds `run_tool` the same way.
- **056** gives `append_transcript` storage and a body limit.
- **058** builds `HttpBoard` in the headless binary.
- **059** builds it in the connected desktop, sets `AppState.board` to `None`, and chooses
  its own Run now path.
- **061** adds the runner picker that fills `runnerId`.

**Traps.**
- **Do not let the server read `BoardHost.provider` for a runner claim.** Test with a runner
  whose provider differs from the host's.
- **A handler that makes a decision is a second copy of a rule.** If a handler grows an
  `if` about tasks, leases or teams, that code belongs in `board::service` or
  `board::relay` (ADR-0006).
- **Solo must not go through `RelayedRequests`.** A solo Run now that waited for the solo
  runner's next `claim(Next)` would turn today's synchronous refusals into a card that
  silently does nothing.
- **The token is a bearer secret.** It goes in a header, never in a query string, a log line
  or a span. `append_transcript`'s query string carries ids only.

**A known cost, accepted.** A relayed Run now that the runner then refuses on its own side
(its CLI is missing, its slot is taken, its opt-in is off) is released after the claim. The
card lands on `failed` with no run row to explain why, which is how the queue's own
post-claim refusals behave today. Surfacing the runner's reason is 061's, most likely
through the runner's doctor report (054). Do not add a run row, a column or a message field
here to fix it.

**Size.** L, and at the ceiling. Estimated diff:

| Part | Lines |
| --- | --- |
| Routes, `RunnerCaller`, protocol check, `mark_seen` | ~550 |
| `HttpBoard` and the tail drain | ~450 |
| Relay, `RelayedRequests`, `claim_next`, three handlers, `ShellRelay`, wrappers | ~650 |
| HTTP harness, adapter tests, protocol and caller tests | ~900 |
| Run-control tests, contract case, records, caches | ~600 |
| **Total** | **~3,150** |

If it runs past about 4,000 lines, cut at Scope 6. Land the routes, the caller, the
protocol check, the adapter, the bundle cap and the suite over HTTP first, with the three
run controls still `local` rows. Then move Scope 6 into a follow-up task with the next free
number, placed directly after 052 in `tasks/README.md`, and say so in the PR. Do not split
Scope 6 itself: one control flipped without the relay leaves a board command with nothing
honest behind it, which is what D32 point 8 forbids.
