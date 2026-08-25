# 18. Change events from core to the shell

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

The board is not the only writer. A task is created by a Tauri command, by an MCP tool
call from a Claude Code session in another window (ADR-0006), by a run writing back to its
own task (ADR-0016), or by the scheduler moving it through its run states. Whichever door
a mutation comes through, every open view has to see it: task 004 requires a task created
over MCP to appear on the board *without a poll*, and task 005 refreshes on
`tasks:changed`.

Only the shell can emit a Tauri event, and `rimaia-core` cannot import `tauri` (ADR-0015).
That boundary is compiler-enforced on purpose — it is what keeps business rules out of a
layer the MCP server cannot reach. So the service that performs a mutation cannot be the
code that tells the UI about it. The notification has to cross the seam, and the shape of
that crossing is something every service in tasks 004–010 depends on.

## Decision

`rimaia-core` owns a `tokio::sync::broadcast::Sender<ChangeEvent>`. Services publish on
it; the shell subscribes once and re-emits into Tauri.

### The sender travels with the pool and the clock

Services take a context struct rather than a bare `&SqlitePool`:

```rust
pub struct ServiceContext {
    pub pool: SqlitePool,
    pub clock: Arc<dyn Clock>,
    pub changes: broadcast::Sender<ChangeEvent>,
}
```

This amends task 004's note: the constraint it was expressing — no `AppHandle`, no
`tauri::State`, nothing the MCP server cannot construct — is unchanged and still enforced
by the crate split. Publishing is an ambient capability of *being a service*, like the
clock, not a parameter each caller decides to pass.

Two rules on publishing:

1. **Publish after the transaction commits.** A subscriber's reaction is to re-read, and
   under WAL an uncommitted write is invisible to the other connections in the pool. A
   notification sent inside the transaction is a subscriber reading the old row and never
   being told again.
2. **Ignore the send result.** `broadcast::Sender::send` returns `Err` when there are no
   receivers, which is the normal state of a `cargo test -p rimaia-core` run and of an app
   shutting down. A mutation that committed must never be reported as failed because
   nothing was listening.

### The event carries ids, never rows

```rust
/// Something in the store changed. Carries which ids, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeEvent {
    Tasks(Arc<[TaskId]>),
    Repositories(Arc<[RepositoryId]>),
    Runs(Arc<[RunId]>),
    Settings,
}
```

`Arc<[_]>`, not `Vec<_>`: broadcast clones the value once per receiver, so a fan-out
should be refcount bumps rather than an allocation per subscriber.

Ids only, because a payload would be a second source of truth. A client handed a row has
to decide whether it is newer than the row it already holds, and it is wrong the moment
the next writer commits. An id says "re-read this", which is always safe, and lets the UI
and the MCP server project the same change differently.

### The shell subscribes once, in `setup()`

`setup()` calls `subscribe()` and spawns one forwarding task on Tauri's runtime for the
life of the app. It maps variants to event names and calls `app.emit`, which already
reaches every window — there is no per-window or per-command subscription.

| Variant | Tauri event | Payload |
| --- | --- | --- |
| `Tasks` | `tasks:changed` | array of task ids |
| `Repositories` | `repositories:changed` | array of repository ids |
| `Runs` | `runs:changed` | array of run ids |
| `Settings` | `settings:changed` | `null` |

**Never publish an empty id list.** An empty array on the wire means "re-read this entity
wholesale", and the forwarder is the only thing that sends one: when the receiver reports
`RecvError::Lagged`, it emits each event once with an empty array and logs the drop count.
That is the whole recovery story — a lagged subscriber re-reads, it does not replay.

Buffer capacity is a constant in core (256 is ample for one desktop user); lag here is a
correctness-neutral hiccup, not a tuning emergency.

### The MCP server is a peer subscriber

Task 010 calls `subscribe()` on the same sender it publishes to. It is not downstream of
the shell: a task written over MCP and the board watching it learn from the same
publication, in whatever order the runtime schedules them. Adding the scheduler's own view
later is another `subscribe()` and no coordination with anyone.

### What does not ride this channel

ADR-0013's live run tail — recent tool calls and assistant messages while a run is active
— is high-frequency and payload-bearing. `runs:changed` means "a run row changed", once
per transition, not once per token. Task 008 picks the mechanism for the tail; it must not
turn `ChangeEvent` into a data channel by adding a payload-carrying variant.

## Consequences

- Core is testable without the shell: a test subscribes to the sender, calls the service,
  and asserts `ChangeEvent::Tasks([id])`. Task 004's "`tasks:changed` fires for every
  mutation" is a `cargo test -p rimaia-core` assertion, not something needing a window.
- A service that mutates without publishing is a card that silently stops refreshing.
  There is no compiler check for it; the mitigation is that mutations go through few
  services and each publication is asserted by a test.
- Broadcast drops messages for a lagged receiver. Accepted deliberately: because events
  carry ids, a drop costs a stale card until the next event, never a wrong one. The
  alternative — an unbounded queue — trades that for unbounded memory in a process that
  runs all night.
- Two names for one thing, the core variant and the Tauri event string. The mapping table
  above is the entire translation layer, and keeping it in one forwarding function means
  those strings appear in exactly one place in the shell.
- A new entity means a variant, a table row and a listener. Subscribers that do not care
  ignore the variant — no registration, no ordering, no lifecycle.
- No new dependency: `tokio` is already a core dependency with `full` features.

## Alternatives considered

- **Emit from the Tauri command that called the service.** Least code, and the command
  knows exactly what it changed — but MCP and scheduler writes stay invisible, which is
  the requirement this ADR exists to satisfy.
- **A callback trait injected into core** (`trait ChangeSink`). Works, and makes every
  service generic over a sink or carrying another `Arc<dyn _>`, with the shell and the MCP
  server registering through separate paths. That is "same invariant, two
  implementations", the failure ADR-0006 exists to prevent. It also puts a fake sink in
  core's tests where a real subscriber does the job.
- **`mpsc` instead of `broadcast`.** One consumer, and there are at least three. It would
  force the shell to be the fan-out point, making the MCP server downstream of the UI
  layer — the inversion ADR-0015 just removed.
- **Polling from the frontend.** Explicitly rejected by task 004. Refresh latency traded
  against constant queries against the single SQLite writer.
- **Give core an `AppHandle`.** Shortest path, and it breaks ADR-0015 at the compiler
  level: core would not build without `tauri`, and `cargo test -p rimaia-core` would need
  WebKit again.
- **SQLite update hooks, or a `changes` table the UI tails.** The database does know what
  changed, at row granularity, with no idea which service operation produced it — and a
  table that is tailed is polling with extra steps.
