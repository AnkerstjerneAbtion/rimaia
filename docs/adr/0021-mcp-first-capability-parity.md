# 21. MCP-first: the tool surface is the whole product, not a subset of it

- **Status:** Accepted
- **Date:** 2026-08-31

## Context

ADR-0006 embedded an MCP server and fixed a **closed table of ten tools**, chosen
for one use: an agent writing plans into the board from another session. Everything
else — starting the queue, registering a repository, reading a run, configuring how
runs are executed — reached the database only through a Tauri command, which means
only through the window.

Task 020 made the gap concrete. It shipped nine commands for execution strategy
(`get_strategy_catalogue`, `set_strategy_defaults`, `accept_task_strategy`,
`plan_task_strategy`, and the rest) and exactly one matching MCP tool. An agent could
be *told* what strategy to use and could write a proposal back, but could not read the
catalogue it was choosing from, could not set a repository's default, and could not ask
for a task to be planned. The UI could do all three.

That asymmetry is not a missing feature. It is a statement about who the product is
for, and it is the wrong one. Rimaia's premise is that agents do the work; a surface
where an agent is a second-class operator of the tool built to run agents undermines
its own argument. It also makes the product untestable from outside itself and
unscriptable by the person who owns it.

## Decision

**Every capability is reachable over MCP. The UI is one client of the service layer,
never the only one.**

Concretely:

1. **Capability parity is a rule, not an aspiration.** A Tauri command without an MCP
   tool is a defect, in the same way ADR-0006 already says a business rule enforced in
   one adapter and not the other is a defect. Both are adapters over the same
   `rimaia-core` function; neither is privileged.
2. **ADR-0006's tool table stops being closed.** Its ten entries were the v1 planning
   surface and remain correct as *that*. They are no longer the boundary of what the
   server exposes. This ADR supersedes that specific constraint and nothing else in
   ADR-0006 — the transport, the loopback boundary, the port, `snake_case`, and D16's
   choices all stand.
3. **Parity does not mean uniform authority.** ADR-0006's trust boundary is unchanged,
   and task 020's `RunScope` is what expresses the difference: the **operator**
   endpoint reaches everything, while a **run-scoped** handle reaches only what a run
   has business doing. Adding a tool therefore means making a scope decision, and
   `every_registered_tool_has_a_run_scope_decision` is what stops one being forgotten.
4. **Two capabilities stay off the run-scoped surface permanently**, and the reasons
   are worth naming because they are the shape of every future refusal:
   - **Anything that spawns a run** (`plan_task_strategy`, and any future queue or run
     control). A run that can start runs is a fork bomb with a billing account, and
     ADR-0016 already refuses to let Rimaia orchestrate agents.
   - **Anything that reconfigures the installation** (the catalogue, defaults, the
     approval flag). A run editing the settings that govern runs is a loop nobody
     asked for.
   Both remain fully available to the **operator** endpoint. This is a scope decision,
   not a parity exception.
5. **`delete_task` stays absent from both.** ADR-0006 excluded it because deletion is
   the one irreversible thing an agent could do by mistake, and parity does not
   overturn that argument — it is a decision about destructiveness, not about which
   client is privileged.

### Known gap: `plan_task_strategy`

Eight of task 020's nine commands ship as tools here. `plan_task_strategy` — "plan this
task now" — does not, and the reason is worth writing down rather than leaving as an
oversight.

It is the only one that **spawns a process**, and spawning needs two things the MCP
server does not have. It needs the runner's `AppPaths` and `RunnerConfig`, which live in
the shell. And it needs the double-start protection `RunRegistry` provides — the check
that stops "Plan now" and "Run now" claiming the same worktree — which exists in
`src-tauri` and has no equivalent in `rimaia-core`.

Wiring the first without the second would ship a tool that can start a second Claude Code
process in a worktree that already has one. That is worse than the gap it closes.

The real question underneath is **who owns "is this task already running" outside the
Tauri shell**, and that is a core design decision, not a wiring task — it is also what
tasks 012 and 014 will need when concurrency and retries arrive. So it waits for them, and
until then "plan this task now" is reachable only from the window. Parity is the rule; this
is the one place it is knowingly unmet, and it has a reason and an owner rather than a
shrug.

#### Amendment, 2026-09-02 — the question is answered; the gap is now only wiring

**`rimaia_core::scheduler::inflight::InFlight` owns "is this task already running".**
Task 012's first change moved it there: one registry, holding a `Lease` per task with
the repository it belongs to, the `CancelSignal` that stops it, and whether the queue or
a human asked for it. The shell holds a clone of the same value, `scheduler::build` is
given the same value, and both doors now read one map.

That retires the arrangement this section describes. `RunRegistry` no longer exists;
`src-tauri`'s `attach_queue` back-reference and its `cancels` map are gone, and with them
the last business rule this codebase kept in the shell. The refusal a second "Plan now"
gets is `LeaseRefused`, rendered by core, so the button and any future tool describe it
identically rather than similarly.

Two consequences worth stating plainly.

**A bug this closed on the way past.** Before the merge, "Plan now" claimed in the shell
and the queue claimed on the database row — two registries that could not see each other
— so a planner and a queued implementation run genuinely could both be started for one
task. Task 023's Notes name that hazard and treat fixing it as a precondition; it is
fixed, not by a check but by there being one registry to check.

**What is still missing is wiring, and only wiring.** The MCP server would still need
`AppPaths` and `RunnerConfig` to spawn, and those still live in the shell. That is a
smaller and more ordinary problem than the one this section named, and it is no longer a
design question. Tasks 012 and 014 deliberately do **not** ship the tool: the answer they
owed was the ownership, and shipping a process-spawning tool is a separate decision with
its own scope argument (point 4 above already says any such tool is operator-only).

## Consequences

- The tool count is no longer a fixed number anyone should assert. The two tests that
  pinned it at ten, then eleven, become tests that the registered set and the scope
  table agree — which is the property that actually matters.
- Rimaia becomes scriptable and testable from outside the window. The end-to-end check
  for task 020 already reads the planner's decisions back over MCP, which is only
  possible because `get_task` exposes them.
- Every new Tauri command now carries an obligation. That is real ongoing cost, and it
  is the point: the cost is what keeps the surfaces from drifting apart.
- The server's surface grows faster than ADR-0006 anticipated, so **tool descriptions
  matter more than they did**. A large, badly-described tool list degrades selection
  more than a small one does — D16.6's context-bomb argument applies to the list
  itself, not only to what a tool returns.
- Nothing here widens the network boundary. Loopback only, unchanged.

## Alternatives considered

- **Keep the closed table and add tools case by case.** What was happening already. It
  produces exactly task 020's outcome — the surface tracks whoever last needed
  something, rather than tracking the product — and it makes every addition a
  negotiation instead of a default.
- **Expose the Tauri command list mechanically as MCP tools.** Guarantees parity and
  removes the judgement, but the two surfaces genuinely differ: MCP is `snake_case`,
  its errors are a different type, D16.5 makes `update_task` clear fields through an
  explicit list, and D16.6 makes `list_tasks` omit plan text. Generating one from the
  other would erase decisions that were made for good reasons.
- **A generic `call_command` escape hatch.** One tool, total parity, no descriptions —
  and therefore no way for a model to discover what it can do. It optimises for the
  author, not the caller.
