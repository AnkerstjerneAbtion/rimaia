# 8. Dependencies unblock on successful run, with branch chaining

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Tasks can depend on each other: "add the API endpoint" must land before "call it from the
UI". Rimaia needs to refuse to start a task until its dependencies are satisfied.

The obvious definition — a dependency is satisfied when it reaches `Done` — breaks the
product's core loop. `Done` means *the user reviewed it*, and review happens in the
morning. An overnight queue of five chained tasks would run exactly one task and then
block until the human wakes up. The entire point is that the queue runs while nobody is
watching.

There is also a second question dependencies raise: what does the dependent task branch
*from*? If B depends on A and both branch from `main`, B does its work without A's code
present, and the PRs conflict.

## Decision

**A dependency is satisfied when the dependency's run completes successfully** — that is,
when it reaches `in_review` or `done`. Not when a human approves it.

**A dependent task's worktree branches from its dependency's branch, not from the default
branch.** With multiple dependencies, the task branches from the highest-priority
dependency's branch (board order) and the others are surfaced as an explicit warning that
the user should either merge them or serialize the work.

Supporting rules:

- Edges are stored as `task_dependencies(task_id, depends_on_task_id)`. Cycles are
  rejected at write time, in both the UI and the MCP server, with the offending path
  reported.
- A task whose dependencies are unsatisfied has `run_state = blocked` and is skipped by
  the queue rather than failing. The card shows which task is blocking it.
- If a dependency **fails**, its dependents stay blocked. A failed dependency is surfaced
  on every dependent card, so one morning glance shows the whole stalled chain.
- Deleting a task with dependents is refused; the edges must be removed first.
- Cross-repository dependencies are rejected. A dependency implies a shared branch base.

## Consequences

- Overnight chains actually run. This is the reason for the decision, and it is the whole
  behavioural difference between a working product and a demo.
- **Unreviewed work becomes the foundation for later work.** If A is subtly wrong, B
  builds on it and both need fixing. This is a real cost, accepted deliberately: it is the
  same tradeoff as a human stacking PRs, and the alternative is a queue that stops.
- Branch chaining makes the resulting PRs stacked. Reviewing them in order is natural;
  merging them out of order is not. The review view shows the chain (task 017).
- Post-MVP escape hatch: a per-task `require_review: bool` for work where building on
  unreviewed code is unacceptable. Deliberately not in MVP — adding it before the default
  has been lived with would be guessing.
- Multi-dependency tasks are the sharp edge. Warning plus documented behaviour for MVP;
  an explicit "merge dependencies into base" step is the eventual fix.

## Alternatives considered

- **Satisfied only at `Done`.** Correct in the strictest sense, useless in practice: it
  makes an overnight queue single-task.
- **Per-edge configurable semantics.** Two behaviours, twice the UI, and the user has to
  reason about which one each edge uses. Not worth it before there is evidence both are
  needed.
- **Branch every task from the default branch regardless.** Simpler worktree logic, and
  guarantees that dependent work is written against code that isn't there yet.

---

## Amendment, 2026-09-02 — blocking is derived, and the base ref is recorded

Task 011 implemented this ADR and found one of its supporting rules in direct conflict
with the decision it supports. This amendment resolves that, and takes the four chaining
decisions the body left underspecified. Appended rather than edited into the body above,
the way seam-contract D4's amendments are: everything written against the original text
should inherit both the rule and the exception to it.

### 1. `run_state = blocked` is never written. The condition is computed at read time.

**The conflict.** The Decision says a dependency is satisfied when it reaches `in_review`
or `done`. The supporting rule below it says "a task whose dependencies are unsatisfied has
`run_state = blocked`". Both cannot hold, and the second is the one that gives way:
`TaskSummary.blocked_by_incomplete` is computed by the board's own query, `RunState::Blocked`
keeps ADR-0007's three legal edges, and nothing in the codebase writes it.

Three independent reasons, any one sufficient.

**It is not a reachable state.** `Idle -> Blocked` is not a legal transition, and a `ready`
card that is `idle` with an unsatisfied dependency is not an edge case — it is the *common*
case, and the only case an overnight queue ever meets. Writing `blocked` would mean amending
ADR-0007's state machine and its exhaustive table test to add an edge whose only purpose is
to cache an answer SQL already gives.

**It would be cached against the wrong row.** This is the decisive one. B's blocked-ness is
not a property of B; it is a property of **A's column**. So every `move_task`, `finish_run`,
`set_task_dependencies` and `delete_task` would have to walk *reverse* edges and write other
tasks' `run_state`. That is the second-source-of-truth mistake seam-contract D12 refuses for
a counter column, in a place where the failure is worse than a stale number: the first write
path that forgets the walk leaves a card reading `blocked` forever, and `Blocked -> Idle` is
not a legal edge to repair it with.

**Nothing needs it.** `selection::skip_reason` already reads `blocked_by_incomplete` before
it matches on `run_state`, and returns `SkipReason::DependencyNotSatisfied` either way. Its
test `a_blocked_dependency_is_one_reason_whichever_of_its_two_spellings_says_so` has pinned
both spellings since task 009. The card's badge does the same, in `lib/board.ts::cardBadge`.

`RunState::Blocked` therefore stays in the enum, in the schema's `CHECK`, and in
`is_legal_run_state_transition`'s table — as a value the domain admits and no code writes.
Deleting it would need a migration SQLite cannot make (D9's argument), and the reachable
case is named below.

**Where it could still legitimately fire.** The queue claims `Idle -> Queued` against a plan
computed one pass earlier. A dependency that moves out of `in_review` in that window leaves a
task claimed for a run its dependency no longer authorises. Task 011 leaves this unwritten
**deliberately**: with one run at a time the window is one pass wide and the next pass
re-reads everything, and inventing a write path for it now would commit task 012 — which has
real concurrent claims, and is where the window becomes worth closing — to a mechanism chosen
before its constraints were known. Recorded here so 012 inherits the decision rather than
re-deriving it.

The visible consequence: `selection.rs` needed no change to its logic for this task, exactly
as its own header promised. Its only diff is one field added to a test fixture, which is the
unavoidable cost of `TaskSummary` growing a field.

### 2. Satisfaction is the column, and only the column.

`board_column IN ('in_review', 'done')`. **Not** `runs.status = 'succeeded'`. The clause after
the dash in the Decision above is the definition, not a proxy for one, and two ordinary cases
make the difference observable:

- A dependency the user implemented by hand and dragged to `done` has no `runs` row at all.
  Under a run-based predicate its dependents block forever, with no escape hatch short of
  deleting the edge.
- A run succeeds, the card files to `in_review`, and the user drags it back to `ready` for
  another go. The run row still says `succeeded`; a run-based predicate would keep the
  dependency satisfied while the human has explicitly un-satisfied it.

Both directions are wrong in the same way: they let a `runs` row outvote the column the user
put the card in, on a board whose whole premise is that the columns are the user's process.

### 3. Order is column rank, then ascending `position`.

"Highest-priority dependency (board order)" above is now exactly: rank the columns in the
order the board draws them left to right — `not_ready`, `ready`, `in_review`, `done` — and
within a rank take the lowest `position`, which ADR-0007 puts at the *top* of a column.

This is **not** `board_column ASC`, which `list_tasks` uses and which sorts alphabetically:
`done` < `in_review` < `not_ready` < `ready`. That ranks the two *satisfiable* columns
backwards, and the mistake is invisible — the query still runs and still returns a branch.
Between the two, `in_review` wins: its branch is live and unmerged, which is the stack this
ADR describes, while a `done` card is one the user has finished with and whose branch may
already be merged into the default branch and deleted.

The same order picks the title a blocked card names, so the card names the dependency the
worktree would actually chain from. One comparator, in `tasks::dependencies_of`.

A dependency with no branch cannot be a base — a task that has never run has nothing to
branch from — so resolution falls through to the next candidate and, failing all of them, to
the default branch, saying so in the warning rather than silently.

The warning names the others individually rather than counting them, because the remedy this
ADR gives ("merge them or serialize the work") is performed on a specific branch. It is
surfaced on `WorktreeStatus`, next to the base ref it is about, rather than on `TaskDetail`:
the base ref is worktree state, and putting the warning anywhere else would mean two reads
disagreeing about which base a warning refers to.

### 4. `runs.base_ref` is written at the start of a run, and preferred by every later read.

`start_run` records the base the worktree was actually created from, at the *open* of the row
rather than at `finish_run`, so a run that dies without a `result` event still leaves a review
able to say what it was building on.

`worktree::status` and `worktree::diff_summary` prefer that recorded value over a fresh
resolution. `worktree::prepare` never reads it. The two are asking different questions: the
morning review is reading a branch that already exists and wants to know what it was measured
against, while `prepare` is producing the next attempt and wants the graph as it stands. A
task's dependencies can change between attempts, and without this split a diff would appear
to gain or lose commits nobody wrote — the same failure mode this ADR's original wording
avoided by refusing to consult the remote.

`WorktreeStatus.dependency_warning` deliberately does *not* follow that rule: it is always
computed against the current dependency set. It is advice about what to do next, and neither
"merge them" nor "serialize the work" is an instruction about the past.
