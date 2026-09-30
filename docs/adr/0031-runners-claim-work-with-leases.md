# 31. Runners claim work with leases, and retries stay on the machine that started them

- **Status:** Proposed
- **Date:** 2026-09-30

## Context

ADR-0010 made the scheduler "the only component allowed to move a task into `running`", and
required selection plus transition to be one transaction "so the UI, the MCP server, and the
scheduler cannot double-claim a task".

The code is close to that but not exactly it. `scheduler/claim.rs` walks two separately
committed edges (`idle → queued`, then `queued → running`) and documents the crash window
between them, which can strand a task at `queued`. On one machine that window is narrow and
startup recovery closes it.

What breaks under ADR-0027 is everything around the claim that assumes one process:

- **Who holds a run** lives in an in-memory `HashMap` (`scheduler/inflight.rs`, seam-contract
  D19). A second machine cannot see it.
- **Startup recovery** works on every task in `running` or `queued`, whoever started it
  (`startup::survey`, then `scheduler/reconcile.rs`). Shared, that means one laptop's launch
  closes every other machine's runs.
- **The go signal, the run window and the usage-limit pause** are global settings rows
  (`scheduler/state.rs`, `scheduler/pause.rs`). Shared, one person's usage limit pauses the
  team.
- **Retries resume the agent's session** (ADR-0011: `--resume <session-id>` in the same
  worktree). Both the worktree and the agent's local session history exist on exactly one
  machine.

The last point is the one that does not bend. A retry is not "run this task again somewhere".
It is "continue this conversation in this directory", and only one machine can do that.

## Decision

### 1. The server is the only authority on who is running what

A runner asks for work. The server decides and records the answer in **one** transaction:
both run-state edges and a **runner lease** row.

```
runner_leases(task_id, purpose, run_id, runner_id, generation, expires_at)
```

`purpose` is `implementation`, `strategy` (the ADR-0016 planner, which has no `runs` row, so
`run_id` is null) or `review` (ADR-0017). A planner run and a review phase are claimed,
leased, fenced and pinned exactly like an implementation run, so every process a runner
starts for a task goes through this one path. One transaction closes `claim.rs`'s documented
window instead of carrying it across a network. It needs the change to `set_run_state` that
`claim.rs` already names.

The claim request carries what the server needs to choose:

- the runner's free capacity;
- the team repositories it has checkouts for (ADR-0033);
- its provider (ADR-0026). The server uses this to skip a task whose effective strategy names
  a model the runner's provider cannot run.

The server applies ADR-0010's selection rules (board order, dependencies satisfied,
per-repository cap) and ADR-0032's eligibility rules. It returns what the runner needs to
start: the plan, the extra instructions, the team's base instructions, the effective strategy
(ADR-0016), the base commit (ADR-0033) and the lease generation.

**Prompt composition stays a pure `rimaia-core` function and runs on the runner.** It takes
the provider's vocabulary (its tool handle and fan-out noun) as parameters, which is task 032's
change (PR #33). Composing on the runner keeps that vocabulary out of the server.

### 2. Waiting for work is a long poll

The claim endpoint holds the request for up to 30 seconds and answers as soon as something
eligible appears. The server wakes waiting claims from the same `ChangeEvent` publications
that refresh the board (ADR-0018), so a task dragged into `ready` is picked up within a
second, not at the next poll.

A long poll rather than a WebSocket because a runner on a laptop moves between networks,
proxies and sleep. Plain HTTP requests that simply get retried survive all of that, with no
reconnection state to manage.

### 3. Leases are renewed by heartbeat, and fenced by generation

While it holds anything, a runner sends one heartbeat request every 30 seconds. The request
renews all of that runner's leases, in one transaction, to three minutes from now.

Every report a runner makes (a state transition, a transcript chunk, `finish_run`, a
run-scoped MCP call) carries the lease's `generation`. The server refuses any report whose
generation is not current, with a new `Conflict` error code (seam-contract D8 is amended for
it, alongside ADR-0030's two). A runner that was presumed gone and comes back cannot then
overwrite what the server decided in the meantime. This is standard lease fencing, and it is
what makes expiry safe to act on.

When the server restarts, it extends every lease by one lifetime before its first expiry
sweep. Its own downtime is not evidence that a runner has gone.

### 4. An expired lease means `interrupted`, and the task stays pinned to its runner

When a lease expires, the server closes the run as `interrupted`, the class ADR-0011 already
has for "process died, or app restarted while `running`". Seam-contract D9 still governs what
the word means.

The task is then **pinned** to the runner that held it. Only that runner may claim its next
attempt, because only that runner has the worktree and the agent session that ADR-0011's
resume depends on. The same pinning applies to `waiting_retry`: a usage-limit or transient
retry is claimed back by the runner that hit it.

**A laptop that slept is the common case.** Its leases expired while its agent process was
suspended. On waking, its first heartbeat is refused as `Conflict`. The runner then:

1. stops that agent process with the normal cancel path (SIGTERM, then SIGKILL);
2. keeps the worktree, and does not push from it;
3. claims the task again through its pin, which resumes the session.

Nothing is lost except the minutes the machine was asleep.

A pin is released in two ways:

- **By a human**, who chooses "run this elsewhere" on the card. The next attempt starts a new
  session on another runner, in a fresh worktree created from the task branch's head on the
  remote if it was ever pushed, otherwise from its base (ADR-0033). The card says the agent
  loses its conversation. The released runner's worktree is fenced: its reports are refused,
  it must not push to the task branch, and its owner is told the task moved.
- **By the owning runner being unpaired.** Its pins are released as above.

The server never moves a pinned task on its own. Quietly restarting work elsewhere would throw
away a session its owner expects to continue, and would put two machines' commits on one
branch.

### 5. Each runner recovers only its own runs; solo leases never expire

At startup, a runner reads the leases it held from its own store (ADR-0028) and reconciles
those. It never touches another runner's. Anything whose lease has since expired on the
server is already `interrupted`, and resumes through the pinned claim.

The global `startup::survey` sweep is replaced by two things:

- this per-runner reconciliation;
- server-side expiry of leases no runner renewed.

**In solo mode the server and the runner are one process**, so a lease cannot outlive the
thing holding it, and the server does not expire leases. A solo laptop that sleeps mid-run
wakes with its run still going, exactly as today. The per-runner reconciliation at startup
then does what `survey` and `reconcile` do today.

### 6. Queue control belongs to the runner

These move to the runner's store (ADR-0028) and govern only that runner:

- the go signal (`queue_state`);
- run windows and named schedules (ADR-0010, task 013);
- mode and concurrency (seam-contract D21);
- the usage-limit pause (ADR-0011).

Consequences for the existing rules:

- **Seam-contract D15 ("quitting stops the queue") holds per runner.** Quitting the desktop
  app stops *that* machine's queue. A headless runner's go signal is its service running.
- **ADR-0011's "a usage-limit hit pauses new starts globally" means globally for that
  runner.** A usage limit belongs to a subscription, and a subscription belongs to a user.
  Other members' runners are unaffected.
- **ADR-0010's per-repository cap of 1 applies per runner.** Its reason was two agents
  fighting over ports, test databases and lockfiles on one machine. Two machines do not share
  those.

`InFlight` (seam-contract D19) stays as the runner's local registry: it still stops a manual
start and a queued run starting the same task on the same machine. Its `Lease` type is
renamed (to `LocalSlot`) when this lands, so the in-process slot and the server's runner lease
are never confused in code. The server lease is the cross-machine authority, and the local
slot is the in-process one.

### 7. Manual runs go through the same claim, and only the owner starts them

"Run now" asks the server to claim a task for a *specific* runner:

- **Only the runner's owner can ask.** A teammate can make a task claimable, by assigning it
  and leaving it `ready`. Starting a process on someone else's machine is not a board action.
- **The server refuses** if the runner is offline or the task is not eligible for it under
  ADR-0032.
- **Capacity caps do not apply**, as seam-contract D19 point 5 decided for manual starts.

**Permission posture follows whether the owner is at the machine, not which button was
pressed.** Run now from the desktop app on the runner's own machine, with the app in the
foreground, is ADR-0012 point 6's interactive run and defaults to `acceptEdits`. Run now from
a browser or another machine for a runner the owner is not sitting at is unattended. It needs
ADR-0032's consent and runs as ADR-0012's unattended run. A remote button must not start a run
that stalls on a prompt nobody is there to answer.

## Consequences

- **Two machines can never run one task at once.** The claim is one transaction on one
  authority, and a stale runner is fenced off.
- **A sleeping laptop no longer interrupts anyone else.** It interrupts only its own runs, and
  picks them back up itself.
- **Leases make "who has this" visible.** The board can show which runner holds a task and
  when it was last heard from. Task 017's review can say "Alice's laptop, last seen 04:12"
  instead of "running" for eight hours.
- **Pinning trades throughput for continuity.** A pinned task waits for its machine, even when
  other runners are idle. That is deliberate: resuming the conversation is what makes a retry
  cheap (ADR-0011). The card makes the wait and the "run elsewhere" option visible so a person
  can choose otherwise.
- **The lease protocol joins CLAUDE.md's list of modules that must have tests**: claim races
  between two runners, expiry, fencing, pinning, restart grace and sleep recovery, all against
  a faked clock. The contract suite from ADR-0027 point 5 runs them through both adapters. No
  test sleeps.

## Alternatives considered

- **Any runner may resume any task.** Maximum throughput, and it does not work: the agent's
  session history is local to the machine that ran it, so "resume" elsewhere is a fresh start
  wearing the old session id. That is the seam bug ADR-0026 point 6 warns about.
- **Push-based dispatch (the server picks a runner and sends it work).** Lets the server
  balance load. Needs a live connection to every runner and a model of each runner's capacity
  that is always slightly stale. With pull, a runner asks when it has room, and the answer is
  decided at that moment.
- **No leases: runners report "I am done" and the server trusts them.** Simple, and a runner
  that crashes or loses its network leaves a task `running` forever. Something has to notice
  absence, and a lease is the smallest mechanism that does.
- **Keep the go signal team-wide.** One switch for "the team's queue is running". Rejected: it
  turns one person's evening into everyone's, and one subscription's limit into everyone's
  pause. A team that wants a shared schedule can agree on one, and each runner can adopt it.
- **Let teammates press Run now on each other's runners.** Convenient for a lead dispatching
  work. Rejected: it makes a board action start a process on someone else's machine at a
  moment they did not choose. Assignment plus the owner's own queue achieves the same thing
  with the owner in control.
