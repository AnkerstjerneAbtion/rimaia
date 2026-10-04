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
Lease expiry, restart grace, sleep recovery, the heartbeat cadence and the runner's
reactions to `Conflict` and to a listed cancel are 053's.

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
  the body is the raw bytes. D31 point 10 names only `run_id`, `generation` and `offset`,
  but the service function takes a whole `LeaseRef`, and deriving the task and team from
  `runId` would give this one method a scope no other method has. The body limit stays
  axum's default until 056 chooses its chunk size and raises it on this route alone (D34).
- **Every handler resolves the caller, verifies what the body claims about it, and makes
  one call** into the same `board::service` function that `InProcessBoard` calls. It
  verifies two claims, and decides nothing else:
  - a `LeaseRef`'s `teamId` is in the caller's scope (point 2);
  - the `ProviderId` that `preview`, `claim` and `run_context` carry equals the runner's
    `runners.provider`, recorded at pairing (047, D28). A mismatch is `invalid`, naming
    both. Like the team, the provider is a claim to verify, never an input: a runner that
    reported another provider would be offered tasks under another catalogue and
    ADR-0031 point 1's model rule. A runner whose provider changed is paired again.

  Two calls take one value the server holds: `claim` for `Next` goes through
  `board::relay::claim_next`, and `heartbeat` passes the server's `CancelRequests` (both
  point 6).
- `publish_tail`'s handler is one call to a new core function,
  `board::service::publish_remote_tail(ctx, runner_id, lease, tail)`. It reads 043's
  `current` and, when the lease holds, calls `ServiceContext::publish_tail(lease.team_id,
  tail)`, 048's one entry for every tail. Otherwise it drops the tail and logs at `debug`.
  In process a tail is unfenced (043), and over HTTP 043's Notes give this drop to 052. The
  handler answers `null` either way, and never writes to the tail relay itself.
- **`ServerState` gains two fields**: `lease_term: LeaseTerm`, which the runner routes pass
  to the service, and `relayed: Arc<RelayedRequests>` (point 6). The binary sets
  `LeaseTerm::Renewable(LEASE_LIFETIME)` from 043's lease module, so an HTTP claim writes
  `expires_at` and a heartbeat renews it. Nothing acts on an expired lease until 053's
  sweep.

**2. The caller: `RunnerCaller`.** In `crates/server/src/caller.rs`, next to 046's
extractor (D32 point 7's table).

- `RunnerCaller(Caller)` accepts only `Authorization: Bearer rmr_…`, resolved through
  047's `Authenticate`, and only when the result is `Door::Runner { runner_id }`. Any other
  credential on a runner route is `unauthenticated`: a valid `rmd_` token, an `rmp_` token,
  a session cookie, both a cookie and a bearer, or nothing. A revoked token, or one whose
  runner has `unpaired_at` set, is already refused by 047's `Authenticate`.
- Every runner handler takes `RunnerCaller` as its first extractor. There is no other way to
  mount a runner route, which is D32 point 7's rule carried over to this table.
- **The runner's identity comes from the token and nowhere else** (D31 point 3). Every
  `board::service` call receives `runner_id` from `Door::Runner`. No body carries a runner
  id. 043's fence, `current(conn, lease, runner_id)`, and the heartbeat's "held by the
  calling runner" therefore compare against the token's runner, so a `LeaseRef` copied
  from another runner's lease is `Conflict`. A runner sees every task in its owner's teams,
  and a generation is a small integer, so the token is all that tells two holders apart.
- **The context.** It is `ServiceContext::for_caller(&caller)`: the scope is every team the
  runner's owner belongs to, read from `team_memberships` on this request (ADR-0030 point
  2), the actor is the owner, and the source is `System` (D32 point 7's mapping). A
  method that takes a `LeaseRef` then runs under `TeamScope::one(lease.team_id)`, after
  checking that the team is in the caller's scope. A `teamId` outside the scope is
  `not_found`. The service's own check that the lease's task is in that team is
  unchanged.
- **Last seen.** Every authenticated runner request calls `board::runners::mark_seen`, the
  one writer of `runners.last_seen_at` (053 Scope 5 keeps it). It is one conditional
  `UPDATE … WHERE last_seen_at IS NULL OR last_seen_at < ?`, so the column is written at
  most once per `HEARTBEAT_INTERVAL` (point 5). A runner publishing a tail several times a
  second must not become a stream of writes that Litestream replicates (ADR-0037 point 2).
  It publishes no change event. The in-process adapter never calls it: in solo nobody reads
  presence, and a `wait: ZERO` loop would write the row on every pass.

**3. The protocol version.** Every runner route calls 046's `protocol::check(header,
Effect::Write)`, after `RunnerCaller`. That is the order 046's board routes use
(`authentication_is_checked_before_the_protocol`), so both surfaces answer a request with
neither credential nor header with `401`. A missing, unparsable, older or newer version is
046's `upgrade_required`, whose message names the oldest version accepted.

- **Every runner method is checked as a write, reads included.** D32 point 7 answers `Read`
  board commands at any version, because a person must still be able to see their board.
  Nothing on this table is read by a person. `preview` and `run_context` exist only to
  compose a run that is about to be claimed or reported. A stale runner that could read
  but not report would compose and spawn a run whose result it could not deliver, which is
  what ADR-0037 forbids ("stops taking work rather than taking it and misreporting it").
- The window is 046's `check_against`. There is no second copy. At `PROTOCOL_VERSION`
  `"1.0"` no header is one minor behind, so "the previous minor is served, the one before
  it is not" is covered by 046's unit tests against a synthetic current version, and this
  task's router tests cover what a real header can reach.

**4. The adapter: `HttpBoard`.** `crates/runner/src/board/http.rs` (D31 point 10).

- `HttpBoard::new(origin, token: Secret, provider)` holds one `reqwest::Client`, the server
  origin, the runner token as 047's `identity::secret::Secret`, and the runner's
  `ProviderId`. It spawns the tail drain (below), so it must be called inside a Tokio
  runtime, and its doc says so. `rimaia-runner`'s manifest turns on reqwest's TLS feature,
  spelled `rustls` on the 0.13 line, at this use site and never on the workspace line
  (D34).
- **It refuses to send a token in clear.** An `http://` origin is `Error::invalid` at
  construction unless its host is loopback: an IP address for which `is_loopback()` holds,
  or the literal `localhost`. The test harness and a same-machine server use plain HTTP.
- **The token never appears in `Debug`, in an error message or in a span.** `Secret`'s
  `Debug` already prints `Secret(…)`.
- Every request sends `Authorization: Bearer rmr_…` and `Rimaia-Protocol:
  <rimaia_core::api::PROTOCOL_VERSION>`. Bodies are `serde_json::to_vec` with an explicit
  `Content-Type`, so reqwest's `json` feature stays off (D34).
- It adds the `ProviderId` to the `preview`, `claim` and `run_context` bodies, as a field
  of the wire body and not of the trait's arguments. The server checks it against the
  runner's row and resolves the catalogue for it (D32 point 2: the runner protocol never
  reads `BoardHost.provider`).
- **Errors are rebuilt from the body's `code`, never from the status.** That covers all
  eight codes: `invalid`, `not_found`, `database`, `io`, `internal`, `conflict`,
  `unauthenticated` and `upgrade_required`. A reply that is not an error body, such as a
  proxy's HTML 502, is `internal`, and its message names the status and the origin. A
  connection that cannot be made is `io` and names the origin. No message contains the
  token.
- **It sets no request timeout and does not retry** (D31 point 10). 053 gives every method
  its timeout, the waiting claim's included. 056's outbox resends reports, and callers ask
  again for reads.
- **`publish_tail` is synchronous and cannot fail** (D14, D31 point 4). It `try_send`s into
  a bounded channel of 64 messages, and one drain task posts them one at a time. A full
  channel drops the message. A failed post is logged at `debug` and dropped. The drain task
  ends when the adapter drops.

**5. `claim` over HTTP answers what `claim` in process answers.** The claim route calls
the same function for `Next` that the in-process adapter calls, including 042's wait on
`ServiceContext::subscribe` and `Clock::sleep_until`. This task adds two constants to 043's
`board/lease.rs`, beside `LEASE_LIFETIME`: `CLAIM_WAIT_MAX` (30 seconds, ADR-0031 point 2)
and `HEARTBEAT_INTERVAL` (30 seconds, point 3), which `mark_seen` throttles by.
`board::service::claim` clamps `wait` to `CLAIM_WAIT_MAX`, so both adapters clamp. 053 uses
both constants and restates neither. The suite compares the two adapters, and a wait
honoured by one and ignored by the other is exactly the difference it exists to find.
What 053 adds is everything that keeps a held request alive across a real network: client
timeouts, backoff, what a server shutdown does to held requests, and restart grace.

**6. Run now, Retry now and Cancel, for one runner.** ADR-0031 point 7, and D32 appendix
rows `start_task_run`, `retry_task_now` and `cancel_task_run`.

*The relay.* A new core module, `crates/core/src/board/relay.rs`, holds one trait:

```rust
pub trait RunnerRelay: Send + Sync + 'static {
    /// Start `request.task_id` on `request.runner_id`, or refuse with a sentence.
    fn run_now<'a>(&'a self, ctx: &'a ServiceContext, request: RunNowRequest)
        -> BoardFuture<'a, ()>;
    /// Stop `task_id`'s run. `lease` is its current lease, if it holds one. Idempotent.
    fn cancel<'a>(&'a self, ctx: &'a ServiceContext, task_id: &'a str,
                  lease: Option<LeaseRef>) -> BoardFuture<'a, ()>;
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
only through the task that needs it, and this is that task. It has two implementations.

- **Solo: `ShellRelay`, in `crates/runner/src/relay.rs`**, beside 042's loop, which already
  spawns `run_task`. `src-tauri` constructs it in `setup()`. It holds no Tauri type, because
  `src-tauri` has no test step in CI. If starting a run needs one, stop and ask.
  - `run_now` is the body of today's `start_task_run` and `retry_task_now` commands:
    036's starter (`preview`, D19's slot, the opt-in, `negotiate`, `probe_cli`,
    `authorize_start` with `OwnerPresence::AtRunner`, then `claim(Run { trigger: Manual,
    continue_session })`), followed by spawning `run_task`. It answers synchronously, with
    today's sentences, byte for byte. Task 008's and ADR-0026's "refused before any run
    state is written" still hold.
  - `cancel` is `InFlight::cancel(task_id)`, always, whatever `lease` says. That also stops
    a manual start that holds its slot and has not claimed yet, during `probe_cli`, which
    is what today's command does.
- **Server: `RelayedRequests`, in `crates/core/src/board/relay.rs`.** It holds pending Run
  nows in memory, keyed by runner, and a `CancelRequests` (below).
  - **`run_now` refuses an offline runner.** A runner is online when `last_seen_at` is
    within 043's `LEASE_LIFETIME`.
  - **It then refuses what the claim would refuse.** It runs the claim's own checks
    read-only, for the named runner and the purpose the claim would take: the run-state
    edge, and `board::lease::eligible` (043, extended by 067 and 045). A refusal is the
    claim's own sentence: 043's for a pin, 067's for the model rule, 045's for consent or
    assignment, and the starter's lost-start sentence (036) for a task with no edge. The
    claim and this check call one function. If 043 left the checks inline in the claim
    transaction, extract them first, with no behaviour change. Without this, a press that
    can never be collected would answer `Ok` and do nothing.
  - **Otherwise it records one request per task.** A second press replaces the first and
    does not add another.
  - **A request expires.** One that no claim collected within one `LEASE_LIFETIME` is
    dropped, measured on the injected clock.
  - **`cancel` with no lease is a no-op**, which is today's idempotence. With a lease it
    calls `CancelRequests::request(task_id, lease.generation)`.

*`CancelRequests`, in `crates/core/src/board/cancel.rs`.* This is the registry 053 first
described, built here because this is the task whose Cancel needs it. It is an in-memory
map keyed by `(task_id, generation)`. `board::service::heartbeat` takes `&CancelRequests`
and lists a task in `Heartbeat::cancel` while its request matches a current lease that the
calling runner listed in `held`. It keeps listing it until the lease ends, so a lost reply
costs one more heartbeat and not the cancel. An entry whose lease has ended is dropped by
the next heartbeat that looks at it. `InProcessBoard` passes an always-empty registry,
because a solo Cancel reaches `InFlight::cancel` directly (D31 point 4). The runner's
reaction to a listed cancel is 053's.

*Why both stores live in memory and not in a column.* Each is a button press that a runner
online at that moment collects within seconds, or within one heartbeat. If the server
restarts first, the press is lost, the card is unchanged, and the person presses again. A
persisted Run now would start work hours later, on a machine whose owner has since moved
on. D4 reserves no migration for this task.

*Collection.* `board::relay::claim_next(ctx, relayed, target)` is what the claim route
calls for `Next`, with `state.relayed`. Before any queue selection, it takes this runner's
oldest unexpired request and claims it through the ordinary `claim(Run { trigger: Queued,
continue_session })` path, in one transaction, with every check that path applies. If the
claim is lost or refused, which after `run_now`'s check means a race after the press, the
request is dropped and selection proceeds as usual.

- **Capacity does not apply** (D19 point 5, ADR-0031 point 7). A `Next` with zero free
  capacity is answered with a relayed request if there is one, and with `None` otherwise,
  never with a queue pick. That is how a busy runner still hears a Run now. Its loop polls
  at zero capacity, which is 053's and 058's to wire.
- **A relayed Run now is unattended**, so the claim carries `RunTrigger::Queued`, which is
  what `authorize_start` returns for `OwnerPresence::Remote` (067). A person who pressed a
  button in a browser or on another machine is not at the runner, and ADR-0031 point 7 says
  that run "needs ADR-0032's consent and runs as ADR-0012's unattended run".
- **The interactive path is the runner's own claim.** `claim(Run { trigger: Manual, … })`,
  sent over a runner's own `rmr_` token, is accepted as an interactive run. Only that
  machine holds the token, so the request is its owner at that machine by construction. 059
  decides whether the connected desktop's own Run now button takes this path or the
  relay. The server accepts both.
- **A runner-side refusal after collection releases** (D31 point 5). This is the queue's
  existing behaviour. For a relayed run it means the card lands on `failed` with no run
  row. That cost is named under Notes, not fixed here.

*Where the relay is held.* `ServerState.relayed` and `host.relay` are one
`Arc<RelayedRequests>`, built once. The sites that build a `BoardHost` or a `ServerState`
gain it in the same commit: the server binary (`crates/server/src/main.rs`), the solo shell
(`src-tauri/src/lib.rs`, with `ShellRelay`), `testing::api::two_teams_state`, and every test
in `crates/server/tests/` from 046, 047 and 048 that builds its own. Test hosts use a real
`RelayedRequests`, never a fake, so the registry cases below exercise the real refusals.
`TwoTeams` marks each fixture runner seen through `mark_seen` when it pairs it, so a case
can start a run on it.

*The board handlers.* The three handlers are in `crates/core/src/api/board/runs.rs`, and
each is a registry row flipped from `local` to `board` with `Effect::Write`. **One flip per
commit** (D32 point 8). Each commit moves the handler, deletes the `#[tauri::command]` and
its `generate_handler!` entry, switches the `commands.ts` wrapper from `local<T>` to
`board<T>`, and adds the row's `BoardCase` to 046's `BOARD_CASES` in `testing/api.rs`,
which `tenant_isolation.rs` and the server's `commands.rs` both run (046 Scope 11, 047). A
local case for the command, if 039 wrote one, leaves `tenant_isolation.rs`'s local table in
the same commit. `check-command-wiring.sh` fails a commit that does one without the others.

- **`start_task_run { taskId, runnerId? }` and `retry_task_now { taskId, runnerId? }`.**
  The task is read in the caller's scope, otherwise `not_found`. Then:
  - **Which runner.** For `Door::Shell` it is the solo runner (038's `solo_identity`), so
    today's invoke payload `{ taskId }` is unchanged. For any other door it is `runnerId`;
    for a retry without one, the task's `pinned_runner_id` (043) when it is set, since the
    D32 appendix makes a retry "a claim through the task's pin". Otherwise the command is
    refused (table below).
  - **Who may start.** Every door but `Door::Shell` calls 067's
    `board::service::authorize_start(ctx, runner_id, OwnerPresence::Remote)`. It answers a
    runner outside the caller's team as `not_found`, and an unpaired runner and a runner
    the caller does not own with 067's sentences. A defaulted pinned runner goes through
    it like a named one. `Door::Shell` does not call it here: the starter inside
    `ShellRelay` already calls it with `OwnerPresence::AtRunner`.
  - **Then** `host.relay.run_now`.
- **`cancel_task_run { taskId }`.** One new service function,
  `board::service::authorize_cancel(ctx, task_id) -> Result<Option<LeaseRef>>`, then
  `host.relay.cancel(ctx, task_id, lease)`. A task outside the scope is `not_found`, and a
  task with no lease is `None`. Otherwise the caller may ask when they own the runner that
  holds the lease, or are an owner of the task's team (051). D32's appendix leaves "who may
  ask" to this task.
  - **Why owners too:** stopping a run is a board action on the team's work, like deleting
    the task. It costs the runner's owner nothing they did not already share by taking the
    task. Starting a run spends someone's machine and subscription at a moment they did not
    choose, and ADR-0031 point 7's refusal is about starting.
  - **Why not members:** a member who is neither is refused, as D32 point 3 shapes role
    refusals.

*The new refusals, exact.* Each is `Error::invalid` with this sentence, and a test asserts
it byte for byte:

| Case | Sentence |
| --- | --- |
| No runner named, over HTTP, and no pin to default to | `name the runner to start this task on` |
| Runner offline | `this runner has not been in touch for more than three minutes, so it cannot start a run now` |
| Cancel by a member who owns neither | `only the owner of the runner holding this task, or an owner of its team, can stop its run` |

Every other refusal of these commands is 043's, 067's or 045's sentence, unchanged. This
task adds no second wording for an unpaired runner, a runner someone else owns, or a pinned
task.

*The wrappers.* `startTaskRun` and `retryTaskNow` in `src/lib/commands.ts` gain an optional
`runnerId`, which they send only when it is given. Existing callers and the 31 test files
that mock `@tauri-apps/api/core` keep sending `{ taskId }`.

*050's browser gates.* The `cancel_task_run` flip commit deletes 050's Cancel gate, because
Cancel needs no runner, and checks the browser scenarios with `npm run screenshot` (028):
the only change is Cancel on a running card. Run now and Retry keep their gates, with the
comments naming 061 in place of 052, and 050's gate table says the same. Without a runner
picker the browser's only answer to either would be `name the runner to start this task
on`. 061 adds the picker and deletes both gates.

**7. The bundle's bounds.** 033 caps the patch and leaves `files` and `commits` unbounded,
and hands this task the choice: a larger body limit on `finish_run`, or a cap at the
runner. At about 100 bytes a path and 250 bytes a commit, a run that regenerates ~10,000
files, or a branch that merged ~4,000 commits of base history, produces a `FinishRun` over
axum's default limit of 2 MiB (2,097,152 bytes). Over HTTP that run could then never be
finished, and from 056 its outbox would resend it forever. **This task caps the whole
encoded bundle at the runner, and `finish_run` keeps axum's default limit**, as D34 has it
("raised on `append_transcript` alone").

- **`BUNDLE_CAP_BYTES = 1536 * 1024`** (1.5 MiB), defined beside `PATCH_CAP_BYTES` in
  `runs/bundle.rs`, with one function `runs::bundle::encoded_len(&ReviewBundle) -> usize`:
  the length of its `serde_json` encoding, counted through a writer that allocates nothing.
  The cap is on the bytes that cross the wire, not on a count of entries, because a path
  has no fixed size. That leaves 512 KiB of the limit for the rest of a `FinishRun`: an
  outcome, two shas, a timestamp and a transcript length.
- **The runner degrades in two steps**, in a new `worktree::bundle::bound(bundle:
  ReviewBundle, cap: usize) -> Option<ReviewBundle>` that 033's `capture` calls last with
  `BUNDLE_CAP_BYTES`. It is a pure function, so its tests build a `ReviewBundle` value and
  never a 20,000-file repository. `capture` keeps 033's signature and delegates to a
  private `capture_with_cap`, which its own tests call with a small cap.
  1. Over the cap, it drops the patch the way 033's budget drops a section: every
     `Included` file becomes `TooLarge`, `patch` is empty and `patch_truncated` is true.
     `files`, `commits` and `diff` are kept. That is a state 033 already renders, "the cap
     cut something", and it tells the truth.
  2. Still over the cap, it returns `None`. `capture` then returns `RunCapture { head_sha,
     bundle: None }`, which is 033's own path for a bundle that could not be built. 033's
     `get_run` reads it as `NotRecorded`, the reading that is never false. `head_sha` is
     kept, because 044's chaining needs it whether or not there is a bundle.

  Each step logs a `tracing::warn!` naming the file count, the commit count and the
  encoded size, so a missing bundle has a reason in the runner's log.
- **`board::service::finish_run` refuses**, with `invalid`, a bundle whose patch is longer
  than `PATCH_CAP_BYTES` or whose `encoded_len` exceeds `BUNDLE_CAP_BYTES`, and the run
  stays open. The runner already bounds what it computes, so these only ever refuse a
  runner that is buggy or not the one it claims to be. They live in the service, not the
  handler, so both adapters apply them (ADR-0006), and the over-cap case reaches the
  service over HTTP as `invalid` rather than dying in the transport, because 1.5 MiB and
  a byte is still under the limit.
- **A body over the limit is still the error shape.** A runner route whose body exceeds
  axum's limit answers D8's `{ code: "invalid", message }`, the message naming the limit,
  and never axum's plain-text `413`, the same rule Scope 1 applies to a malformed body.
- **Why not a larger limit.** Any finite limit still needs a cap behind it, or some run
  fails the transport, so a raise only moves where the cap bites. It also raises how much
  one request may make the server buffer, and how much a night of runs adds to what
  Litestream replicates (033's storage argument), for runs too large to review in the app
  anyway.
- **Why not a cap on each list with a flag.** D28's DDL has no column that says a list was
  cut, and D4 reserves no migration for this task. `files_changed` would expose a
  shortened `files`, but nothing counts the commits, so a shortened `commits` would read as
  the whole history. Dropping the patch, then the bundle, uses only states 033 already
  stores and renders honestly.
- Solo changes too, deliberately. `capture` is shared, so a solo run past the cap now
  stores the degraded bundle, or none, where on `main` it stored all of it. The contract
  suite compares the two adapters, and two answers to one oversized run is the difference
  it exists to find.

**8. The catalogue crosses the wire tolerantly.** 036 left `Catalogue`, `CatalogueEntry`
and `PlannerBudget` with `deny_unknown_fields`, the one exception to D31 point 6, and
gave this task the decision. `RunContext::catalogue` carries them. Over HTTP a newer board
that adds a field to an entry would make every `run_context` reply unreadable to an older
runner, and the runner would stop working for a reason the protocol header was meant to
own (ADR-0037 point 4).

- **The attribute moves to the parser of the hand-edited document.** `Catalogue`,
  `CatalogueEntry` and `PlannerBudget` lose `deny_unknown_fields`. `RawCatalogue`, which is
  already the one parser of the stored setting (`strategy/catalogue.rs`, `parse_raw`), keeps
  it, and gains two private strict twins, `RawCatalogueEntry` and `RawPlannerBudget`, with
  the same fields and serde attributes plus `deny_unknown_fields`. `resolve` converts them.
  A misspelled key anywhere in the stored setting is therefore still refused on write and
  still a warning on read, exactly as today. A value the board produced, on the wire or on
  the tool surface, is read tolerantly like every other DTO.
- **Why not a wire mirror.** A mirror would be a second spelling of the type the runner
  actually uses, on the side every reader sees, with conversions both ways and a
  `WireRunContext` to carry it. The private twins are a second spelling too, but only
  inside the one module that parses what a person types, which is where strictness is
  wanted. The doc comments in `catalogue.rs` that argue against a projection are updated
  to say where the strict twins live and why.
- `schemars` drops `additionalProperties: false` from the catalogue in
  `get_strategy_catalogue`'s output schema. Nothing reads that as a contract: the tool's
  output is produced by the board, and `set_strategy_catalogue` takes the catalogue as a
  string the service parses (`mcp/requests.rs`), so its input schema does not change.

**9. The HTTP harness.** `crates/runner/tests/board_port_http.rs` invokes
`board_contract!(HttpHarness)` (D31 point 13).

- `rimaia-server` becomes a dev-dependency of `rimaia-runner`, the direction ADR-0027 point
  6 allows. 046's check that the server does not depend on the runner must still pass.
- **`HttpHarness` implements every constructor the `Harness` trait has at 051's tip, and
  each builds exactly the fixture its in-process twin builds.** That is `start`,
  `start_with(term)` (043), which builds `ServerState` with that `lease_term`, and
  `start_shared` (045), which mints `B`'s token for the second member. Users, team roles,
  the second author 045's consent cases need, runner owners and runner providers all
  match: where the in-process `B` is on the Ledger provider for 067's model-rule case, the
  HTTP `B` is paired as Ledger too. Teams are made through 038's and 051's services, and
  runners are paired through 047's core token functions, never through SQL.
- Each builds, in order: a `TestClock`; a multi-connection board in a `TempDir`, as 043's
  in-process harness has (the race case needs real concurrency); a `BoardHost` with a real
  `RelayedRequests` and 047's real `Authenticate`; the real `rimaia-server` router, served
  with `axum::serve` on a `TcpListener::bind("127.0.0.1:0")`; one `HttpBoard` per runner,
  pointed at that port.
- **`Harness::board()` is `host.context.with_scope(…)` over the server host's context**,
  so it shares the change and live channels the routes publish on. A harness that built a
  second context would leave 042's
  `a_waiting_next_claim_returns_as_soon_as_a_task_becomes_startable` waiting forever.
  `Harness::clock()` is the same `TestClock` the server reads.
- **The cases are not edited.** If a case reaches past `Harness` and fails only over HTTP,
  fix the harness or the adapter, never the case. If the case itself is wrong, stop and
  say so. 036's Notes predicted this.
- Nothing sleeps. A wait is a clock advance. The listener and its server task are dropped
  with the harness.

**10. Records.**
- A dated amendment to **D31**. It records: `append_transcript`'s query carrying the whole
  `LeaseRef`; `BUNDLE_CAP_BYTES`, the runner's two-step degrade, the service's two
  refusals and `finish_run` staying at the default limit (point 7), which answers 033's
  hand-off; the catalogue types read tolerantly, strictness kept in `RawCatalogue`'s
  twins, which closes 036's named exception to point 6 (point 8); `CLAIM_WAIT_MAX`,
  `HEARTBEAT_INTERVAL` and the clamp in the service (point 5); every runner method
  checked as a write (point 3); the runner id taken only from the token; the body's
  provider verified against `runners.provider`; the tail fenced over HTTP only;
  `mark_seen` and its throttle; the relay, `run_now`'s pre-check, relayed requests
  collected by `claim(Next)`, and `CancelRequests` read by `heartbeat`; and the two Run
  now postures.
- A dated amendment to **D32**. It records: `BoardHost::relay` and `ServerState`'s two
  fields; the three rows flipped, with their `From` cells unchanged; the retry's default
  to the pin; and the cancel rule.
- A row in the seam-contract "How to use this" table for 052: D8, D10, D14, D19, D28, D29,
  D31, D32, D33 and D34.
- CLAUDE.md's Gotchas gains one bullet: "The runner protocol (`/api/v1/runner/*`) is not in
  the command registry. Its registry is `BoardMethod::ALL`, and a method added there gets a
  route, an `HttpBoard` body and a cross-team case in the same commit."

## Out of scope

- **Everything about a network that drops.** That is 053: the heartbeat cadence and loop,
  lease expiry and the sweep that closes runs as `interrupted`, restart grace, sleep
  recovery, the one `Conflict` reaction (D31 point 11), the adapter's timeouts and
  backoff, and the runner's reaction to `Heartbeat::cancel`.
- **`find_repositories` and `report_runner`**, checkouts by remote, doctor reports,
  credential rekeying, and the mapping check before a Run now: 054. It adds its methods to
  `BoardMethod::ALL`, and point 1's `match` then makes it add the routes.
- **`run_tool`** and the run-scoped proxy: 055.
- **Storing transcripts on the server**, the outbox, resend idempotence (D31 point 12) and
  `append_transcript`'s body limit: 056. Until then, the server's `append_transcript`
  acknowledges through the service function exactly as the in-process adapter does, and
  keeps nothing. No production HTTP runner exists before 058, so nothing is lost.
- **The push postcondition, "run elsewhere", and releasing pins at unpairing:** 057.
- **The headless binary and `rimaia-runner pair`:** 058. **The connected desktop**, meaning
  `AppState.board = None`, an `HttpBoard` in `lib.rs`, and which path its own Run now
  takes: 059.
- **Any interface change** beyond the optional wrapper argument and the Cancel gate: 061
  picks the runner and shows pinned and requested cards.
- **Rate limits on runner routes.** 047 limits the endpoints ADR-0030 names. A runner route
  is already behind a 256-bit token.
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
- `a_runner_cannot_report_on_a_lease_another_runner_holds`: runner B belongs to a second
  member of a team that A's owner shares. A sends B's current `LeaseRef` to `finish_run`
  and to `release`. Each answers `409 conflict`, and `tasks`, `runs` and `runner_leases`
  are unchanged.
- `a_body_provider_that_differs_from_the_paired_one_is_invalid`, on `preview`, `claim` and
  `run_context`.
- `a_runner_request_marks_the_runner_seen_at_most_once_per_heartbeat_interval`: two
  requests 10 s apart write `last_seen_at` once, and a third at 31 s writes it again. No
  write publishes a `ChangeEvent`.
- `a_runner_is_online_for_one_lease_lifetime_after_its_last_request`: A sends a heartbeat at
  2 min. A Run now on A at 4 min is accepted, and one at 5 min and 1 s is refused with the
  offline sentence.
- `a_runner_route_ignores_the_rimaia_team_header` (050's D entry): a `Rimaia-Team` naming
  one of the owner's teams changes no answer.
- `a_tail_under_a_stale_generation_is_dropped_over_http`: the route answers `null`, and a
  subscriber receives nothing.

**The protocol version**
- On a lease-less route and on a lease route alike, each of these is `426
  upgrade_required` with 046's minimum-version message:
  `a_runner_request_without_the_protocol_header_is_refused`,
  `a_runner_ahead_of_the_server_is_refused` (`1.1`), and
  `a_runner_on_another_major_version_is_refused` (`2.0`). `1.0` is served.
- `a_stale_runner_cannot_read_a_run_context_either`: `run_context` and `preview` refuse
  with `upgrade_required` at `1.1`.

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
  `http://127.0.0.1:<port>`, `http://[::1]:<port>` and `http://localhost:<port>`.
- `the_adapter_ignores_a_reply_field_it_does_not_know`: forward compatibility, since no DTO
  uses `deny_unknown_fields` (D31 point 6).
- `a_tail_published_with_a_full_buffer_is_dropped_and_publish_never_blocks`, and
  `a_published_tail_reaches_a_subscriber_through_the_server`.
- `the_adapter_sends_its_provider_on_preview_claim_and_run_context`, and the catalogue in
  the reply is that provider's.

**The contract suite**
- `crates/runner/tests/board_port_http.rs` invokes `board_contract!` over the real server
  router on `127.0.0.1:0`, and every case passes: 036's lifecycle, 038/039's cross-team
  case, 042's `Next` cases, 043's race, fencing, generation, heartbeat and `Never` cases,
  067's model-rule cases, and 045's eligibility and consent cases, whatever the suite holds
  at 051's tip. `git diff` shows no change to any existing case in
  `crates/core/src/testing/board_contract.rs`.
- `crates/core/tests/board_port_in_process.rs` still passes, including the new case below.
- New contract cases, run by both adapters:
  - `a_finish_whose_patch_exceeds_the_cap_is_invalid`. Its patch is `PATCH_CAP_BYTES + 1`,
    and the run stays open.
  - `a_finish_whose_file_list_exceeds_the_bundle_cap_is_invalid`. The patch is under
    `PATCH_CAP_BYTES`, and `files` is long enough that `encoded_len` is
    `BUNDLE_CAP_BYTES + 1`. Over HTTP the answer is `400 invalid` from the service, not a
    transport failure, and the run stays open.
  - `a_finish_whose_bundle_is_exactly_at_the_cap_is_recorded`. The bundle's `encoded_len`
    is `BUNDLE_CAP_BYTES`, the outcome carries a 4 KiB `error_message`, and the run closes
    with the bundle stored. Over HTTP this is what proves the default body limit holds a
    capped `FinishRun`.
- `every_lease_method_refuses_a_stale_generation` passes over HTTP with `409 conflict` on
  the wire and `ErrorCode::Conflict` rebuilt by the adapter.

**The bundle's bounds and the catalogue**
- In `worktree/bundle.rs`, over constructed `ReviewBundle` values with no repository:
  - `a_bundle_under_the_cap_is_left_exactly_as_built`.
  - `a_bundle_over_the_cap_drops_its_patch_and_keeps_its_lists`: every `Included` file is
    `TooLarge`, `patch` is empty, `patch_truncated` is true, and `files`, `commits` and
    `diff` are unchanged.
  - `a_bundle_whose_lists_alone_exceed_the_cap_is_dropped`: 20,000 files. `bound` returns
    `None`.
- `capture` over a real repository whose bundle `bound` drops keeps `head_sha`
  (`an_oversized_bundle_is_dropped_and_head_sha_is_kept`), and `get_run` on the finished row
  reads `NotRecorded`. The test calls `capture_with_cap` with a cap a few hundred bytes
  wide, so the repository stays small and the test is quick on all three CI systems.
- `a_runner_body_over_the_transport_limit_is_invalid_in_the_error_shape`: a 2 MiB and one
  byte body to `finish_run` answers JSON with `code: "invalid"` and a message naming the
  limit.
- `a_run_context_whose_catalogue_carries_an_unknown_field_parses_in_http_board`: a stub
  server answers `run_context` with an extra key on the catalogue, on one model entry and
  on the planner budget. `HttpBoard` returns the `RunContext`, and its catalogue equals
  the one without the extra keys.
- In `strategy/catalogue.rs`, the stored setting stays strict:
  `a_misspelled_key_in_a_stored_catalogue_entry_is_refused_on_write` and
  `a_misspelled_key_in_a_stored_planner_budget_is_refused_on_write` each answer with the
  parser's own message, and
  `a_stored_catalogue_with_a_misspelled_entry_key_falls_back_with_a_warning` reads the
  provider's default. The module's existing tests pass with no edit.
- `grep -n deny_unknown_fields crates/core/src/strategy/catalogue.rs` matches only
  `RawCatalogue` and its two private twins.

**Run now, Retry now, Cancel**
- The three registry rows are `board`/`Write`. `check-command-wiring.sh` passes. There are
  no `#[tauri::command]` definitions for them in `src-tauri/src/commands/runs.rs`, and
  `commands.ts` calls `board<T>` for all three.
- Each of the three has a `BoardCase` in `testing/api.rs` with `Foreign::Ids`, and 046's
  `every_board_command_has_a_case`, `a_team_cannot_see_another_teams_ids` and
  `both_transports_answer_every_case_identically` pass with them, as does
  `tenant_isolation.rs`.
- **Solo is unchanged.** 036's starter tests keep passing with no edit:
  `a_missing_claude_binary_is_refused_before_any_run_state_is_written`,
  `a_repository_that_has_not_opted_in_cannot_start_a_task`, and
  `a_lost_manual_start_answers_with_todays_sentence` with its retry twin. In
  `crates/runner`, `a_solo_start_reaches_the_starter_through_the_shell_relay` dispatches
  `start_task_run` as `Door::Shell` and gets the starter's answer, and
  `a_solo_cancel_stops_a_start_still_in_preflight` cancels a start held in `probe_cli` by a
  fake CLI that does not answer, and nothing is claimed. The frontend suite passes with no
  edit to any `toHaveBeenCalledWith` of the three commands.
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
  - `run_now_for_a_runner_the_task_is_not_eligible_for_is_refused_with_the_claims_sentence`:
    a plan edited by a second author and not accepted. The message equals 045's
    `run_now_on_an_unconsented_task_refuses_with_the_reason` string, and A's next
    `claim(Next)` returns no relayed task.
  - `a_relayed_run_now_that_is_no_longer_eligible_is_dropped_and_selection_continues`: the
    plan is edited after the press.
  - `retry_now_through_a_relay_resumes_the_session`: `Claim::resume` is set.
  - `retry_without_a_runner_goes_to_the_pinned_runner`, and without a pin is refused with
    the first sentence in the table.
  - `a_cancel_is_listed_on_the_holding_runners_heartbeats_until_its_lease_ends`: listed on
    two consecutive heartbeats from A, never on B's, and not after A's `finish_run`.
  - `a_cancel_for_an_ended_lease_is_dropped`.
  - `cancel_with_nothing_running_is_a_no_op`.
  - `a_team_owner_can_stop_a_members_run`.
  - One test per sentence in the table, asserting the exact string, plus
    `start_task_run_for_a_runner_outside_every_shared_team_is_not_found`.
  - `the_http_start_refusals_are_the_in_process_sentences`: a runner someone else owns, an
    unpaired runner and a task pinned elsewhere are refused over HTTP with messages equal,
    byte for byte, to 043's and 067's.
- `commands.test.ts`: `startTaskRun` and `retryTaskNow` send `runnerId` only when given.
- 050's Cancel gate is gone, and its Run now and Retry gates name 061. 050's
  `it("sends no local command from any view in browser mode")` passes. The screenshot
  comparison is listed in the PR body.

**Records and CI**
- D31 and D32 carry the dated amendments listed in Scope 10. The "How to use this" table has
  the 052 row. CLAUDE.md has the Gotchas bullet.
- `rimaia-runner`'s manifest enables reqwest's `rustls` feature, and the workspace line
  still has `default-features = false`. `cargo tree -d` shows one `axum` and one `reqwest`.
  No crate enters `Cargo.lock` that D34 does not name.
- Both offline caches are regenerated with D33's recipe for the queries this task adds
  (`mark_seen`, the online read, the provider read, `authorize_cancel`'s reads), and
  committed.
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
- **D28:** `runners`, `api_tokens`, `runner_leases` and `tasks.pinned_runner_id`, and
  `review_bundles`, whose DDL has no column to say a list was cut (Scope 7).
- **D29** point 9: `StartRun::kind` crosses the wire.
- **D33:** both caches.
- **D34:** reqwest's feature spelling, and nothing else to add. `finish_run` keeps axum's
  default body limit, as D34 already says (Scope 7).
- **D8**, **D10**, **D14** (a dropped tail costs nothing) and **D19** point 5 (manual
  starts ignore caps).
- **D4 and D6** as prohibitions.

**Files to start from.** These exist on `main` today:
- `src-tauri/src/commands/runs.rs`: `start_task_run`, `retry_task_now` and
  `cancel_task_run`, whose bodies become `ShellRelay`;
- `src-tauri/src/lib.rs`: `setup()` and the handler list;
- `crates/core/src/scheduler/inflight.rs`: `cancel` and `acquire_unbounded`;
- `crates/core/src/error.rs`: `ErrorCode`, and the `Serialize` the adapter reverses;
- `crates/core/src/context.rs`, `testing/context.rs` and `testing/clock.rs`;
- `src/lib/commands.ts`, `src/lib/commands.test.ts` and `scripts/check-command-wiring.sh`;
- `crates/core/src/strategy/catalogue.rs`: `Catalogue`, `RawCatalogue`, `parse_raw` and
  `resolve`; and `crates/core/src/mcp/requests.rs`'s `SetStrategyCatalogueRequest`.

These are created by earlier tasks on this branch:
- `crates/core/src/board/{port,types,service,in_process}.rs`,
  `crates/core/src/testing/board_contract.rs` and `crates/core/src/runner/start.rs` (036);
- `crates/runner/` (040) and its runner loop (042);
- `crates/core/src/board/lease.rs`: `LeaseTerm`, `LEASE_LIFETIME`, `eligible`, the fence
  and `authorize_start` (043, 067, 045);
- `crates/core/src/api/{mod,registry,caller,protocol}.rs`, `api/board/runs.rs`,
  `crates/core/src/testing/api.rs`, `crates/server/src/{lib,caller}.rs` and
  `crates/server/tests/common/mod.rs` (046);
- `identity::secret::Secret`, the token code and `Authenticate` (047);
- the tail relay on `BoardHost` (048);
- `crates/core/src/runs/bundle.rs` (`ReviewBundle`, `PATCH_CAP_BYTES`) and
  `crates/core/src/worktree/bundle.rs` (`build`, `capture`) (033);
- 050's capability gates on Run now, Retry and Cancel.

**Migration:** none.

**What the next tasks expect.**
- **053** hardens what this task serves: the adapter's timeouts, backoff, a runner loop
  that polls at zero capacity so it hears Run now, the heartbeat loop and its reaction to a
  listed cancel, `app_version`, expiry using `LEASE_LIFETIME`, and the one `Conflict`
  reaction. It uses this task's `CLAIM_WAIT_MAX` and `HEARTBEAT_INTERVAL`.
- **054** adds `find_repositories` and `report_runner` to `BoardMethod::ALL` (D31's
  2026-10-04 amendment), and gets routes, adapter bodies and cross-team cases from this
  task's structure. It also adds a fifth check to "What the board checks first": the target
  runner maps the task's repository. **055** adds `run_tool` the same way.
- **056** gives `append_transcript` storage and a body limit. Its outbox can resend a
  `FinishRun` without a size check of its own: Scope 7 keeps every one under the default
  limit.
- **058** builds `HttpBoard` in the headless binary. **059** builds it in the connected
  desktop, sets `AppState.board` to `None`, and chooses its own Run now path.
- **061** adds the runner picker that fills `runnerId`, and deletes the Run now and Retry
  gates.

**Traps.**
- **Do not let the server read `BoardHost.provider` for a runner claim.** Test with a runner
  whose provider differs from the host's.
- **A handler that makes a decision is a second copy of a rule.** If a handler grows an
  `if` about tasks, leases, runners or teams, that code belongs in `board::service` or
  `board::relay` (ADR-0006). That includes Run now's owner check, which is 067's
  `authorize_start`.
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

**A second known cost.** A run past `BUNDLE_CAP_BYTES` shows no in-app patch, and one
whose lists alone pass it shows no bundle at all, only `NotRecorded`. Both are runs that
touched thousands of files, which nobody reviews line by line in an overlay, and the forge
holds the whole diff. Do not add a column to say why; the runner's log names the counts.

**Size.** L, and at the ceiling. Estimated diff:

| Part | Lines |
| --- | --- |
| Routes, `RunnerCaller`, protocol check, `mark_seen` | ~500 |
| `HttpBoard` and the tail drain | ~450 |
| Relay, `RelayedRequests`, `CancelRequests`, `claim_next`, three handlers, `ShellRelay`, wrappers | ~750 |
| HTTP harness, adapter tests, protocol and caller tests | ~900 |
| Run-control tests, registry cases, contract case, records, caches | ~650 |
| Bundle bounds, catalogue strict twins, their tests and contract cases | ~300 |
| **Total** | **~3,550** |

If it runs past about 4,000 lines, cut at Scope 6. Land the routes, the caller, the
protocol check, the adapter, the bundle's bounds, the catalogue and the suite over HTTP
first, with the three run controls still `local` rows. Then move Scope 6 into a follow-up
task with the next free number, placed directly after 052 in `tasks/README.md`, and say so
in the PR. Do not split Scope 6 itself: one control flipped without the relay leaves a
board command with nothing honest behind it, which is what D32 point 8 forbids. Scopes 7
and 8 stay in the first half: the contract suite over HTTP cannot pass without them.
