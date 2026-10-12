---
id: "070"
title: A context that acts for nobody
milestone: v0.5
status: ready
depends_on: ["045", "046", "048", "051", "052"]
adrs: ["0030", "0029", "0019", "0031"]
size: S
---

# A context that acts for nobody

## Goal

Widen `ServiceContext::actor` from a plain `UserId` to an enum with a second answer, so that
the server can build a context for work no person asked for without inventing a user:

```rust
// crates/core/src/context.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor { User(UserId), Server }

impl Actor {
    /// The user id, or "server". For tracing spans and `Debug` only: never bound into a
    /// query, never written to a column.
    pub fn as_str(&self) -> &str;
    /// The user this context acts for. `Error::internal` for `Server`.
    pub fn user(&self) -> Result<&UserId>;
}
```

Every read of the actor, by a writer or a reader, goes through `actor.user()?`. A context
acting for the server can then never write an author column, read a user's settings,
publish to a user's audience or pass a membership check. Each of those is refused
`internal`, because reaching one from a server context is a wiring bug and never something
a person did.

This task adds the type and converts every use. Its first production `Actor::Server`
context outside a test is the server's base context (point 3). Its first caller that acts
on it is 053's lease sweep.

## Why now

[ADR-0030](../docs/adr/0030-identity-people-sign-in-machines-pair.md) point 8 puts the acting
user on `ServiceContext`. Task 038 made it a plain `UserId` on purpose (D10): every writer
through M4 acts for a user, whether that is the solo user, a signed-in caller or a runner's
owner. An enum whose second variant nobody constructs would have been dead weight for fifteen
tasks, and 038 said that the task needing a context that acts for nobody would widen the
field.

That task is 053. Its sweep expires leases whose runners never came back, and its restart
grace extends leases before the server accepts a request. Neither has a caller, a session or
a token, and ADR-0031 point 4 says the server does this on its own. 053 carried the widening
in its own Scope and measured about 4,050 lines with it, which is more than one session
carries. The widening has nothing to do with leases: it is a type change through every
reader and writer that 039 to 052 added, and the sweeper's context is its only reader in
053. So it is cut out here, before 053, as a task of its own.

It goes directly before 053, and not earlier, for two reasons:

- **The set of readers is complete.** 052 is the last task before 053 that adds one: its
  `RunnerCaller` context, through `for_caller`. Done earlier, the conversion would be
  repeated by every task between.
- **055 already writes `Actor::User(owner)`** for a forwarded call's context. It must find
  the type in place.

## Scope

**1. The type.** `Actor` lives in `crates/core/src/context.rs`, beside `ServiceContext`, and
is re-exported wherever `TeamScope` is. `ServiceContext::actor: Actor`, and
`ServiceContext::new` takes `actor: Actor` where it took `actor: UserId`.

- **There is no `Default`, and no `From<UserId> for Actor`.** 038's rule holds: every
  construction site states the actor. Without `From`, each one spells `Actor::User(..)` or
  `Actor::Server`, so `grep -rn 'Actor::Server' crates src-tauri/src` lists every context that
  acts for nobody. On this branch that is the server's base context and tests.
- **`user()`'s error is `Error::internal`**, with the message exactly
  `"this needs a user, and the context acts for the server"`. D8 does not grow a variant for
  it. A person never sees it, because no door builds a `Server` context.
- **`as_str()` returns the id or the literal `"server"`.** User ids are UUIDs (D10), so the
  two cannot be confused in a span. 038's fifteen `#[tracing::instrument]` sites, and any
  added since, keep `user_id = ctx.actor.as_str()` unchanged in spelling. The hand-written
  `impl fmt::Debug for ServiceContext` prints `actor` as `as_str()` does.
- `with_source`, `with_scope` and `for_caller` keep the actor they were given or set, as
  before.

**2. Every use goes through `actor.user()?`.** The check is made before the write's
transaction does anything, or inside it so that the error rolls it back. Either way a refused
call writes nothing and publishes nothing. The uses, as the earlier tasks on this branch
leave them:

- **039:** `db::settings::get_user` and `set_user` over `user_settings`, and through them
  `get_subscription_cost`, `set_subscription_cost`, and 042's read of
  `subscription_monthly_usd` for the queue's selection.
- **045:** the author columns (`tasks.created_by`, `plan_updated_by`, `assigned_by`,
  `review_instructions_updated_by`, `team_settings.updated_by`), `set_trust`,
  `list_trusted`, `accept`, and the ownership check in `set_runner_eligibility`.
  `written_during_run(conn, actor)` takes `&UserId`, and its callers pass
  `ctx.actor.user()?`. Its signature is otherwise unchanged.
- **047:** any account row that reads `ctx.actor` rather than `request.caller`. The ones
  that read the caller are unchanged.
- **048:** `ChangeEvent::user_settings(user_id)` is called with `ctx.actor.user()?`, and
  so is the SSE route's check of `Audience::User`.
- **051:** `create_team`'s owner membership, `teams::require_owner`, `leave_team` and
  `delete_account`.
- **Anything else that the compiler finds.** Changing the field's type makes every remaining
  use a type error, so none is missed silently. A use that wants the id as a string for a
  span uses `as_str()`. Every other use calls `user()?`.

**No production code outside `context.rs` matches on `Actor`.** A `match ctx.actor` or
`if let Actor::User(id)` elsewhere would be a second way to read the id, and the one that
forgets the `Server` arm is the bug this task exists to rule out. Construction sites spell a
variant, and nothing else names one. Tests may compare with `assert_eq!`.

**3. The construction sites.** `grep -rn 'ServiceContext::new\|ServiceContext {' crates
src-tauri/src` lists them. Each becomes `Actor::User(..)` with the same id, except one:

- `src-tauri/src/lib.rs`, `testing/context.rs`, `context.rs`'s own tests and
  `crates/core/tests/repo_service.rs`: `Actor::User` with the solo user (038).
- `ServiceContext::for_caller` (046): `Actor::User(caller.user_id)`. It is the only place a
  `Caller` becomes an actor. No `Door` maps to `Server`, which covers 052's `RunnerCaller`
  (the owner) and, later, 060's `/mcp` (the token's user).
- 051's harness, one context per acting user: `Actor::User` for each.
- **046's server base context becomes `Actor::Server`.** 046 had to give
  `BoardHost.context` some user id, because the field had no other answer, and a server board
  has no solo user to give. That placeholder goes. `dispatch` still replaces the base with
  `for_caller` before any handler runs (D32 point 2), so this changes no request's actor. The
  base now says what it is: a context that acts for nobody, which is the one 053's sweeper
  re-scopes per lease.

`mcp::build` and `scheduler::build` re-source a clone of the shell's context and inherit its
`Actor::User`. They are unchanged.

**4. The records.**

- **A dated amendment under D32**, beneath point 7's "one place turns a caller into a
  context": `for_caller` always sets `Actor::User`; `Actor::Server` is never derived from a
  `Caller`, and is built only from `BoardHost.context` by the server's own work (053's sweep
  and restart grace); and every read of the actor is `actor.user()?`. Nothing already in D32
  is edited.
- **The "How to use this" table** gains the 070 row: D8 · D10 · D32 · D33, and D4 and D6
  as prohibitions.
- **The offline caches.** No query's text changes: an argument expression that was
  `ctx.actor` becomes `ctx.actor.user()?`, and the cache is keyed by the SQL. Neither
  `.sqlx` cache changes. If one does, the diff changed a query it should not have.
- **No migration, crate, dependency or CI step** is added, and CLAUDE.md is unchanged.

## Out of scope

- **The sweep, restart grace and the sweeper's loop.** 053. This task constructs no
  `Actor::Server` context that does any work.
- **A `Server` door, or any way for a request to act for nobody.** No `Caller` becomes
  `Server`, and none should.
- **Recording the server as an author.** `created_by` and its siblings stay user ids or
  NULL (a deleted account). A server that needed to author content would need an ADR-0030
  amendment, not a sentinel id.
- **Any change to `MutationSource`.** The sweeper's `System` source is ADR-0019's, and is
  already a variant.
- **Converting the actor to a newtype or changing `UserId`.** D10 stands.

## Acceptance criteria

- `ServiceContext::actor` is an `Actor` with exactly the two variants and two methods in
  the Goal. There is no `Default` for `ServiceContext` or `Actor`, and no
  `From<UserId> for Actor`.
- **`Actor` unit tests** in `context.rs`:
  `user_answers_the_id_for_a_user_actor`;
  `user_is_internal_for_the_server_actor`, asserting `ErrorCode::Internal` and the exact
  message; and `as_str_is_the_id_or_server`.
- `debug_prints_the_actor`: `format!("{ctx:?}")` contains the solo user's id for a user
  context and `server` for a server context.
- **A server context reaches no user's state.** Each case builds a context from
  `TestContext` with `Actor::Server` and the solo team's scope, subscribes before the call,
  and asserts `ErrorCode::Internal`, no row written or changed, and no `ChangeEvent`
  received:
  - `a_server_context_cannot_write_an_author`: `create_task`;
  - `a_server_context_cannot_assign_a_task`: `assign_task`;
  - `a_server_context_has_no_user_settings`: `get_subscription_cost` and
    `set_subscription_cost`;
  - `a_server_context_cannot_accept_or_trust`: `accept` and `set_trust`;
  - `a_server_context_owns_no_team`: `create_team`, and `set_member_role` on the solo team.
- `for_caller_sets_a_user_actor`: for each `Door` 046 defines, `for_caller` gives
  `Actor::User(caller.user_id)`. 046's `for_caller_sets_source_scope_and_actor` passes, with
  its assertion on the actor updated to `Actor::User`.
- `the_server_base_context_acts_for_nobody`: the `BoardHost` that `crates/server`'s
  startup builds has `Actor::Server`, and 046's test that `dispatch` replaces the base
  context before a handler runs passes unchanged.
- **One way to read the actor.** No file under `crates/*/src` or `src-tauri/src` other than
  `crates/core/src/context.rs` contains `match` on an `Actor`, `if let Actor::`, or
  `let Actor::`. A structural test in `crates/core/tests/` scans the sources and fails
  naming the file and line. It skips `testing/` and `#[cfg(test)]` modules, as 039's
  `no_service_takes_a_pool_without_a_scope` does.
- **Nothing else changes behaviour.** Every existing test passes with no change other than
  `Actor::User(..)` at a construction site and the `for_caller` assertion above: 039's
  tenant-isolation cases, 045's consent and mark cases, 048's audience cases, 051's role and
  deletion cases, and 052's HTTP contract suite.
- The D32 amendment exists, dated, and no existing sentence in D32 is edited. The "How to
  use this" table has the 070 row.
- **No migration is added**, and both `.sqlx` caches are byte-for-byte unchanged.
- **Every CI check passes**, with `SQLX_OFFLINE=true` exported: the full command block in
  CLAUDE.md as 052 leaves it.

## Notes

**Read first.** ADR-0030 point 8 (who did what is recorded) and ADR-0029's note on why
`ServiceContext` may grow. ADR-0019 for the precedent: `source` has no default because every
default is wrong somewhere, and `actor` follows it. ADR-0031 point 4 for why the server acts
on its own at all. Seam entries:

- **D32**, points 2 and 7: `BoardHost`, `dispatch` replacing the base context, and
  `for_caller` as the one place a caller becomes a context.
- **D10**: `UserId` is a `String` alias, and a UUID, which is why `"server"` cannot collide.
- **D8**: `internal` is an existing code, and the error type does not grow.
- **D33**: why an argument change leaves the caches alone.
- **D4** and **D6**: no file and no dependency here.

**Files to start from.** `crates/core/src/context.rs` (038's `scope`, `actor`, `with_scope`
and the `Debug` impl; 046's `for_caller`); `crates/core/src/db/settings.rs` (039's
`get_user`, `set_user`); `crates/core/src/consent/` and `crates/core/src/tasks/` (045);
`crates/core/src/events.rs` (048's `Audience`); `crates/core/src/teams/` (051);
`crates/core/src/api/` (046's `BoardHost`, `Caller`); `crates/server/src/main.rs` (046's
startup). All of these exist only once the tasks named have landed on this branch.

**What the next tasks expect.**

- **053** builds the sweeper's context from `BoardHost.context`, which is already
  `Actor::Server`, with `MutationSource::System`, and re-scopes it per lease. Nothing in
  053's expiry path may call `actor.user()`, and 053's contract cases prove a sweep under a
  server context succeeds.
- **055** builds a forwarded call's context with `Actor::User(<the runner's owner>)`, which
  045's mark reads through `actor.user()?`.
- **060**'s `/mcp` reaches `for_caller`, and so `Actor::User`, like every other door.

**Size.** S. About 350 lines of mechanical churn through 039, 045, 046, 048 and 051's
readers and writers (053's own estimate for this part), about 250 lines of tests, and a
short amendment. If the conversion finds a reader no earlier task names, convert it the same
way and say so in the PR body. If it finds one that genuinely needs to act without a user,
stop and ask: that is a decision, not churn.
