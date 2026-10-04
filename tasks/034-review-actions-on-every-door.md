---
id: "034"
title: Review actions on every door
milestone: v0.4
status: ready
depends_on: ["033"]
adrs: ["0006", "0007", "0008", "0013", "0019", "0021", "0033"]
size: M
---

# Review actions on every door

## Goal

Make the three decisions a morning review ends in — **approve**, **reject** and **needs
changes** — into `rimaia-core` services, and serve each one through a Tauri command and an MCP
tool that are thin adapters over it. Alongside them, add the two reads a reviewer needs before
deciding: which tasks depend on this one and which of those already built on it
(`tasks::dependents_of`, ADR-0008), and an **overnight digest** of what the queue did since the
reviewer last finished a review.

**This task is the backend half of task 017, and only that half.** It adds no screen. Task 017
renders these services, and it renders them as they come back: it does not re-sort the digest,
choose the window it covers, or check a note the service would accept. Every rule here is
written once, in core, so the window, the MCP server and the web client (task 049) cannot
disagree about it.

## Why now

Task 017 was written when the window was the only client, so it owned its actions as UI
behaviour: "move to `done`", "append a note", "keep the worktree". ADR-0021 makes a capability
the UI has and MCP lacks a defect. ADR-0006 makes a rule enforced by one adapter and not the
other a bug. And team mode serves the same actions over HTTP (ADR-0034). If the actions were
written into 017's components, all three of those later tasks would have to pull them back out.

It lands right after task 033 for one reason: reject has to say which dependents built on the
task, and after 033 that is a question about recorded commits (`runs.base_sha`,
`runs.head_sha`) rather than a guess from branch names.

It also lands before task 035 adds run kinds. D29 point 8 plans for that order: the digest
reports **one entry per task**, with the task's outcome taken from its newest row, and 035
adds the loop summary to that entry later.

## Scope

**A new module, `crates/core/src/review/`**, with `mod.rs`, `actions.rs`, `note.rs` and
`digest.rs`. Task 035 later adds the findings store beside them, and task 021 adds the loop.
Every function here takes `&ServiceContext`, never a raw pool. Every mutating one carries
ADR-0019's `#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id =
%id))]` and publishes through `ctx`, as `move_task` does.

**The note: `review::note`.** This is a pure function, and its output is the contract task 021,
the prompt and 017's copy all inherit:

```rust
pub enum Verdict { Rejected, ChangesRequested }
pub fn append(existing: Option<&str>, verdict: Verdict, note: &str) -> String
```

- The block is a header line, then `"\n"`, then `note.trim()`. Newlines inside the note are
  kept byte for byte.
- If `existing.map(str::trim_end)` is non-empty, the result is that trimmed text, then
  `"\n\n"`, then the block. Otherwise the result is the block alone.
- The two header lines, exactly:

  ```
  Review note (changes requested; the reviewed commits were kept, so build on them):
  Review note (rejected; the task restarted on a fresh branch without those commits):
  ```

  They are written in the past tense on purpose. Notes accumulate over several rounds, and a
  header must stay true when a later run reads it after another verdict has followed it.

- A worked example, which the tests assert verbatim. `existing = "Keep the public API
  unchanged.\n"`, `ChangesRequested`, `note = "  The migration must be reversible.\n"` gives:

  ```
  Keep the public API unchanged.

  Review note (changes requested; the reviewed commits were kept, so build on them):
  The migration must be reversible.
  ```

- **A blank note is refused** (`Error::invalid`, D8) by both reject and needs changes. The note
  is the only thing that differs between the reviewed run's input and the next run's.
  Restarting with an unchanged prompt is already one drag to `ready` away, and a blank note
  would make that look like a review.

**The actions: `review::actions`.**

- `approve(ctx, task_id) -> Result<Task>`: `in_review` → the bottom of `done`.
- `request_changes(ctx, task_id, note) -> Result<ReviewOutcome>`: `in_review` → the bottom
  of `ready`, with the note appended to `extra_instructions`. **The worktree, the branch and
  `tasks.branch` are untouched**, so the next run's `worktree::prepare` returns the existing
  worktree through its idempotence rule and the run continues on the reviewed commits. The
  next run starts a fresh session, because a queue start from `idle` always does. It is not a
  retry, and ADR-0011's resume path is not involved.
- `reject(ctx, task_id, note) -> Result<ReviewOutcome>`: `in_review` → the bottom of `ready`,
  with the note appended, and **the task's work set aside, never deleted**:
  1. The worktree directory is removed with `worktree::remove(ctx, id, false,
     ForceRemoval::No)`. The branch is kept in git. `remove` clears `tasks.worktree_path`
     itself, as it does for every caller.
  2. `tasks.branch` is cleared.
  3. The next `prepare` therefore resolves a branch name that does not exist yet: the
     collision suffix (`rimaia/<id>-<slug>-2`, `worktree::naming`) branches from the base the
     graph gives at that moment.

  **Reject has two halves, and the code keeps them apart.** The local half is everything
  that needs this machine's disk: the uncommitted-changes refusal, `worktree::remove`, and
  the `worktree_path` write inside it. It lives in one private function in
  `review::actions`, `set_aside_worktree(ctx, &task)`, and nothing else in `reject` touches
  git or the filesystem. The board half is the note, `branch = NULL`, the move and the
  marker, in the transaction described under Atomicity. Team mode moves the first half to the
  runner that holds the worktree and leaves the second on the board (ADR-0033 point 7; see
  "What the next tasks expect"). One function is what lets 066 move it without re-reading
  `reject`.

  The old branch, and any PR opened from it, stay exactly as they were. `ReviewOutcome` names
  the set-aside branch so the UI can say where the rejected work is.

  Reject keeps the branch because deleting it is the one irreversible act here, and D20 point 6
  keeps irreversible acts off MCP. Reusing the old branch name would have the next run push
  onto a remote branch that holds the rejected commits, which fails as non-fast-forward in the
  middle of an unattended night.

```rust
pub struct ReviewOutcome {
    pub task: Task,
    pub dependents: Vec<Dependent>,        // every direct dependent, see below
    pub set_aside_branch: Option<String>,  // reject only; None for request_changes
}
```

**Refusals, identical for every door, all `Error::invalid` with a sentence naming the task:**

| Condition | approve | request changes | reject |
| --- | --- | --- | --- |
| Column is not `in_review` | refuse | refuse | refuse |
| Task is archived | refuse | refuse | refuse |
| `run_state` is `queued`, `running` or `waiting_retry` | refuse, no override | refuse, no override | refuse, no override |
| `run_state` is `failed` or `cancelled` | allowed | refuse: the queue would skip it (`SkipReason::NeedsAttention`); say to use Retry | refuse, same sentence |
| `run_state` is `idle` or `blocked` | allowed | allowed | allowed |
| `plan` is empty (the move into `ready` would fail `ensure_ready_has_a_plan`) | — | refuse, with that function's sentence | refuse, same sentence |
| Note is blank | — | refuse | refuse |
| Worktree has uncommitted changes | — | allowed (kept) | refuse, with the count (sentence below) |

The "no override" rows reuse `worktree::cleanup::is_live` (plus `queued`) and follow D20
point 1's reasoning: a process is, or is about to be, writing in that directory. `idle` and
`blocked` are allowed because neither has a process in the directory. `blocked` only says a
queue start was waiting on a dependency, and nothing ties it to a column, so an `in_review`
task can hold it. The row exists so the three doors cannot diverge on it. No action writes
`run_state`, so a task sent back to `ready` while `blocked`
stays `blocked`, and `skip_reason` reports `DependencyNotSatisfied` until the dependency
succeeds. That is ADR-0008's rule, and not something a review should skip.

Reject has no force for the dirty-worktree case. The refusal is what makes reject safe to
offer over MCP. The user commits, or removes the worktree through the cleanup UI, which is
the human-only door D20 point 6 built for that. The count and the sentence's first half come
from `worktree::cleanup`, not from a second copy. Split `ensure_committed` into a
`pub(crate)` count (the existing "nothing on disk is nothing to lose" check, then
`git::dirty_file_count`) and a `pub(crate)` sentence builder that takes the next-step clause
as a parameter. `ensure_committed` passes its existing clause, so its sentence stays
byte-identical, and reject passes its own. For one change on a task titled `Add login`,
reject's sentence is exactly:

```
"Add login" has 1 uncommitted change in its worktree, committed nowhere else. Removing it would discard it for good — commit the work, or remove the worktree under Settings → Storage, before rejecting.
```

Reject offers no "confirm that you want to", because no door has a force for it.

**Order of checks.** Every refusal in the table, plan presence included, is checked
**before** any git runs, from one read of the task. The actions then re-check the column,
the archive, the run state and the plan inside the transaction, because the task can change
between the two reads. So a reject that is going to be refused never removes a worktree
first. The only refusal that can still arrive after git is a race lost in that window, and
it leaves the state the retry rule below describes.

**Atomicity.** All of an action's own database writes happen in **one** `BEGIN IMMEDIATE`
transaction: the note, `branch = NULL` (reject), the move and the digest marker described
below. `worktree::remove` is not part of it: it clears `worktree_path` in its own write and
publishes its own `Tasks` event, as it does for every caller, and it is not reimplemented
here. A refused move therefore leaves `extra_instructions` unchanged, and a retried action
never appends the note twice. `move_into_column`'s body is split so it can run inside a
transaction the caller holds. The public `move_task` and `move_task_to_bottom` keep their
signatures and behaviour.

For reject, the local half goes first and the transaction second. If the transaction then
fails, the task is still `in_review` with its branch recorded and no worktree path, and
reject retried completes, because `worktree::remove` is idempotent and the dirty-tree check
passes when there is nothing on disk.

Approve runs `auto_remove_on_done` after its commit, exactly as a drag to `done` does (D20
point 3). The auto-removal policy is a rule of the transition, and approve is that transition.

**Dependents: `tasks::dependents_of` and `review::dependents`.**

- `tasks::dependents_of(ctx, task_id) -> Result<Vec<Task>>`, public, in
  `crates/core/src/tasks/dependencies.rs`. It returns direct dependents, archived ones
  included (their edges still exist), ordered by `compare_dependency_order`, the same ADR-0008
  comparator `dependencies_of` uses. It reads through a private executor-generic query that
  `delete_task` also calls **inside its own transaction**, which replaces `delete_task`'s
  inline query. `delete_task` sorts the titles itself, so its refusal message stays
  byte-identical.
- `review::dependents(ctx, task_id) -> Result<Vec<Dependent>>`, where `Dependent { id, title,
  column, run_state, archived_at, built_on: bool }`, in `dependents_of`'s order. `built_on` is
  true when the dependent has at least one `runs` row that started from this task's work:

  ```sql
  r.base_ref = <this task's tasks.branch>
  OR r.base_sha IN (SELECT head_sha FROM runs
                    WHERE task_id = <this task>
                      AND head_sha IS NOT NULL
                      AND base_sha IS NOT NULL
                      AND head_sha <> base_sha)
  ```

  The first clause covers rows recorded before 033, whose `base_sha` is `NULL`. The second
  covers every row since 033 and keeps working after task 044 bases dependents on `head_sha`
  (ADR-0033 point 5) and after a reject renames the branch. A dependent that chained from a
  **different** dependency matches neither clause and is not marked.

  The `head_sha <> base_sha` condition keeps only rows whose branch carried commits of this
  task's. 033 records `head_sha` whenever it resolves, so a run that committed nothing on a
  fresh branch has `head_sha` equal to its fork point, a default-branch commit. Without the
  condition, a dependent that later branched from the default branch at that same commit
  would be marked, and one way that happens is a reject: `branch` is then `NULL`, and
  ADR-0008 falls through to the default branch. The condition holds exactly when 033 wrote a
  review bundle for the row, but the query reads the two columns and does not join
  `review_bundles`. A later run that continued on a reviewed branch and committed nothing
  still qualifies, because its `head_sha` is the earlier run's commit, not its fork point.

  Reject and request changes compute this **before** their writes, in the same transaction,
  while `tasks.branch` still names the reviewed branch.

**The digest: `review::digest`.**

- `digest(ctx) -> Result<Digest>` covers runs whose `ended_at` falls in the half-open window
  `(since, until]`, plus runs still open. `until` is `ctx.clock.now()`. `since` is the marker.
  With no marker, it is `until - DIGEST_DEFAULT_WINDOW`, which is 24 hours.
- **One entry per task, never per run** (D29 point 8). Archived tasks are left out. A task
  with a row in the window takes its outcome from its **newest** row, by `attempt`:

  | Newest row | Task `run_state` | Outcome |
  | --- | --- | --- |
  | `running` | any | `Running` |
  | `succeeded` | any | `Completed` |
  | `failed` or `interrupted` | `waiting_retry` | `WaitingRetry` |
  | `failed` | otherwise | `Failed` |
  | `interrupted` | otherwise | `Interrupted` |
  | `cancelled` | any | `Cancelled` |

  `interrupted` is D9's: a row status, never a task `run_state`. The task of an interrupted
  row is `waiting_retry` or `failed`, which is why the table reads the pair.

- **Run-backed entries and context entries.** The entries above are **run-backed**: the task
  has a row that ended in the window, or a row still open. A `ready` task with no row in the
  window can also appear, as a **context** entry. If `TaskSummary::blocked_by_incomplete` is
  set, it appears as `Blocked`, carrying `blocking_title` (D12's 2026-09-02 amendment).
  Otherwise, if `scheduler::selection::skip_reason` returns `UnattendedRunsNotAllowed`,
  `NeedsAttention` or `WaitingForRetry`, it appears as `Skipped`, carrying that reason.
  `skip_reason` is called, not re-implemented.

  **Context entries are included only when the digest has at least one run-backed entry.**
  Neither kind of context depends on the window. A repository that has not opted into
  unattended runs (ADR-0012), or a ready task behind a `not_ready` dependency, would
  otherwise put an entry in every digest for good, and the marker could never empty it. With
  the rule, a quiet board gives an empty digest, and a night with activity says what it did
  not reach. A failed task from an earlier night still shows as `Skipped` (`NeedsAttention`)
  in the next digest that has activity, which is when it matters to the day's plan. 017's
  opening rule keys on `totals.runs > 0`, which stays correct: it now differs from "the
  digest has entries" only while a run is still open.
- **Attention-first order**, as the service returns it and as 017 renders it: `Failed`,
  `Blocked`, `WaitingRetry`, `Interrupted`, `Cancelled`, `Running`, `Completed`, `Skipped`.
  Within one outcome, ADR-0008's comparator orders the entries (column rank, then `position`,
  `created_at`, `id`), so completed work still waiting in `in_review` comes before work
  already approved.
- **`DigestTotals` has run totals and entry counts, and says which is which.** The run
  totals count every row that ended in the window: `runs`, `run_seconds` (the sum of
  `ended_at - started_at`), `span_seconds` (the earliest `started_at` to the latest
  `ended_at`), `cost_usd` (the sum of the recorded costs) and `runs_without_cost`. A `NULL`
  cost is counted as missing and never summed as zero (D18). Open runs are in no run total.
  `counts` is different: it counts **entries** per `DigestOutcome`, one per variant with
  zeros kept, because `Blocked` and `Skipped` entries have no runs to count.
- Each entry carries `task_id`, `title`, `repository_id`, `column`, `outcome`, `runs`,
  `run_seconds`, `cost_usd`, `last_run_id`, `error_message`, `pr_url`, `blocking_title` and
  `skip_reason`. It carries no plan text (D16.6). The three numbers cover the same rows the
  run totals do, the entry's rows that ended in the window:
  - `runs` is how many there are, `0` for a context entry and for an entry whose only row is
    still open.
  - `run_seconds` is their summed duration, `None` when there are none.
  - `cost_usd` is the sum of their costs, and `None` when there are none **or when any of
    them has a `NULL` cost**. A sum that silently leaves out a missing cost understates it,
    which is D18's objection. The totals can report the gap as `runs_without_cost`, but an
    entry has no such field, so it reports "not recorded" instead.
- **The marker is `review_digest_seen_through`**, an RFC 3339 instant owned by
  `review::digest` in D3's shape: the accessor owns the key, and `db::settings` owns the
  storage. Its placement is **User**. Following D28 part 4 ("a task that adds a key before 038
  lands states at its accessor which placement the key has"), this task appends a dated
  amendment to D28 that adds the key to part 4's User row and to both key lists in part 6's
  adoption SQL. No migration writes it (D28: no migration before 038 may insert a `settings`
  row).
- **The marker's storage.** `db::settings::set` takes `&ServiceContext` and autocommits, so
  it cannot write inside a review action's transaction. `db::settings` gains an
  executor-generic `set_in(executor, key, value)` that writes and does not publish. `set`
  becomes `set_in` over the pool followed by its publish, with its behaviour unchanged. Both
  marker paths below write through `set_in`, so there is one statement for the key.
- **Two things advance the marker, and nothing moves it backwards.**
  1. `mark_seen(ctx, through)`. It stores `max(current, through)`, reading and writing in one
     `BEGIN IMMEDIATE` transaction so that two concurrent calls cannot interleave and store
     the smaller value. A `through` later than `ctx.clock.now()` is refused. Callers pass the
     `until` of the digest they showed, so a run that ends between that read and the mark is
     never skipped.
  2. The review action that **leaves no unarchived task in `in_review`** sets the marker to
     `max(current, now)`, inside its own transaction. This is what empties the digest when a
     morning's review is finished: every row that ended is now at or before the marker, and
     with no run-backed entry left, the context entries go too. 017 opens on the review view
     while `totals.runs > 0`, and stores no marker of its own. Without this rule the digest
     would never empty. An action that leaves other tasks in `in_review` does not move the
     marker, so the digest stays whole while a review is in progress.

  Whichever path writes the marker publishes `ChangeEvent::Settings` after its commit. A
  review action that advanced it publishes `Settings` as well as `Tasks`, so 017's digest
  re-reads on the event it listens for.

  **Only the three review verdicts advance the marker.** Emptying `in_review` by dragging the
  last card to `done` (`move_task`) or by archiving it does not. This is not a rule one door
  enforces and another skips: `move_task` and `archive_task` behave the same on every door,
  and so do the three verdicts. It is a rule about which transition means "the review is
  finished". `move_into_column` cannot tell a person's verdict from a machine's move. Task
  021's loop moves cards out of `in_review` unattended, and an MCP client can drag a card at
  3 a.m. Either would advance the marker in the middle of the night and hide the runs that
  ended before it. A board emptied by drags keeps its digest until the next verdict or a
  `mark_seen`.

**The doors.**

- **Tauri commands**, in a new `src-tauri/src/commands/review.rs`, registered in **both**
  `generate_handler!` lists in `src-tauri/src/lib.rs`: `approve_task`, `reject_task`,
  `request_task_changes`, `get_task_dependents`, `get_review_digest` and
  `mark_review_digest_seen`. Each is one line over its service, with arguments named the way
  `commands/tasks.rs` names them (`taskId`, `note`, `through`).
- **MCP tools** with the same six names (snake_case fields, D16.1), handled in
  `mcp/server.rs` over the same six functions, with `Tool` variants in `mcp/scope.rs`.
  Responses are projections in `mcp/responses.rs`, with no plan text (D16.6). The
  descriptions say plainly how reject differs from request changes. ADR-0021's consequence
  about tool descriptions applies: the difference between the two is the thing a model is
  most likely to get wrong.
- **RunScope: all six are `RunAccess::Refused`** (ADR-0021 point 3). Approve, reject and
  request changes are refused because a run deciding a review is a run marking its own
  homework (D30 point 5). Once task 035 re-keys the table by grant, they are ✘ for every
  grant. The digest and dependents reads are refused because they enumerate other tasks, which
  is D16.6's objection and the reason D20 point 6 refuses `list_worktrees`. The marker write
  is refused on ADR-0021 point 4's reconfiguration ground.
- **The frontend's half of the wire**: six typed wrappers in `src/lib/commands.ts`, and the
  mirrored types in `src/types.ts` (`ReviewOutcome`, `TaskDependent`, `ReviewDigest`,
  `DigestEntry`, `DigestOutcome` and `DigestTotals`, reusing the existing `SkipReason`
  mirror). No component. Task 017 is written against exactly these names.
- **Six fixture rows in `src/dev/fixtures/`** (task 028). 028's coverage test reads every
  `call<…>("name"` literal out of `commands.ts`. A wrapper with no row fails it, and 028
  put the rule in `CLAUDE.md` so that the task adding a command adds its row. Each row is
  typed against the new mirrors in `src/types.ts`, so a row that drifts from the Rust shape
  fails `npm run typecheck`. Every row answers. None is a refusal, because each command has
  an honest picture to give, and 028 keeps refusals for commands that have nothing to show.
  Following 028, a write answers without changing the seed, and a row is not a second
  implementation of the service:

  | Command | Answer, in every scenario |
  | --- | --- |
  | `get_review_digest` | An empty digest: no entries, the run totals `a_board_with_only_blocked_or_unopted_ready_tasks_has_an_empty_digest` asserts, copied, every `DigestOutcome` in `counts` at `0`, and any instant it carries an offset from `FIXTURE_NOW` (the 24-hour no-marker window ending there) |
  | `get_task_dependents` | The seeded tasks with a dependency edge to `taskId`, as `TaskDependent`s in the seed's board order, every `built_on` `false`. No seeded run records a commit, so nothing can have built on anything |
  | `approve_task` | The seeded task with `column: "done"` |
  | `request_task_changes` | A `ReviewOutcome`: the seeded task with `column: "ready"` and a changes-requested block appended to its `extraInstructions` (a literal in the seed, not a TypeScript port of `review::note`); `dependents` as `get_task_dependents` gives them; `setAsideBranch: null` |
  | `reject_task` | As `request_task_changes`, with `branch` and `worktreePath` `null` on the task and `setAsideBranch` the seeded task's `branch` |
  | `mark_review_digest_seen` | What the command returns, and nothing else |

  A `taskId` the seed does not hold is refused with `invalid` and a sentence naming the
  id, the code the service would use (D8). That is the only refusal. The rows do not
  re-implement the refusal table, the note's blank check or the digest's order. A fixture
  is a picture, and the service's tests are where those rules are proved.

  The digest is empty in every scenario on purpose. No screen reads it until 017, and an
  empty digest is the one answer no scenario can contradict, `busy` included. 017 adds the
  `review-*` scenarios (its "Fixture data for the screenshot script") and gives this row a
  per-scenario answer then. This task adds no scenario and no row to
  `screenshots/views.shot.ts`.
- **D32's appendix** gains six rows, appended under a dated sub-heading ("added after
  728a049"), as D32 point 8 requires of any command added before 046. All six are `board`,
  `From` 046. The effect is `Read` for the two reads and `Write` for the other four, and each
  row cites ADR-0021 point 3 and this task. The appendix's counts describe `main` at 728a049,
  and are left as they are. Two rows carry a Note in the style of the `archive_task` and
  `move_task` rows, so that 046 migrates them without judging them (D32 point 8):

  | Command | Note |
  | --- | --- |
  | `reject_task` | The board handler writes the note, `branch = NULL`, the move and the marker. The uncommitted-changes refusal and the worktree removal (`review::actions::set_aside_worktree`, with the `worktree_path` write inside it) run on the runner that holds the worktree, not in the handler (ADR-0033 §7). In connected mode the refusal is a runner-side check, and a synchronous refusal to the caller is not guaranteed (task 034) |
  | `approve_task` | As `move_task`: D20.3's auto-removal on `done` becomes the runner's reaction to the change event once `worktree_auto_cleanup` is a runner setting (ADR-0028 §2) |

  The other four rows (`request_task_changes`, `get_task_dependents`, `get_review_digest`,
  `mark_review_digest_seen`) touch no disk and need no Note beyond the citation.

## Out of scope

- **Every screen**: the digest, the review queue, the keys and the warnings are task 017's.
  So are the `review-*` fixture scenarios, the per-scenario digest answers, and every
  screenshot. This task's fixture rows are the coverage floor, not a picture of review.
- **Forge actions.** Nothing here closes, comments on or merges a PR, or deletes a remote
  branch. A rejected task's old PR stays open, and `set_aside_branch` is how the UI tells the
  user about it.
- **Deleting the set-aside branch.** That stays a human act in the worktree cleanup UI
  (D20 point 6).
- **Undo.** Every action here is an ordinary board write that a drag reverses, except the
  worktree reject removed. The next run recreates that worktree on a fresh branch, which is
  the point of rejecting.
- **Findings, loops and kinds.** The findings tools and the per-grant scope table are task
  035's (D30 point 5). The loop summary on a digest entry is 035's too (D29 point 8). What a
  finished review does to a card is task 021's.
- **Roles.** Who on a team may approve is task 051's. Team scoping of every function here is
  039's.
- **Consent.** Appending a note edits `extra_instructions`, and ADR-0032 point 3 makes that a
  plan revision. Bumping `plan_revision` is task 045's. See Notes.
- **A `require_review` flag** (ADR-0008's post-MVP escape hatch). It is not asked for.
- **A migration.** The marker is a settings key, not a column (D4).

## Acceptance criteria

Rust tests use the harness's `TestClock` and never sleep. Git runs against real repositories
in a `TempDir`. The runs that tests drive are real child processes replaying recorded fixture
streams through `testing::cli`. Prompt and note strings are asserted in full, never by
substring.

**The note.**

- `a_review_note_on_empty_extra_instructions_is_the_block_alone`,
  `a_review_note_is_separated_from_existing_instructions_by_one_blank_line`,
  `trailing_whitespace_is_trimmed_before_a_note_is_appended` and
  `two_review_notes_append_in_the_order_they_were_given`. Each asserts the exact `String`,
  including the worked example in Scope, byte for byte, for both header lines.
- `a_blank_note_is_refused_by_reject_and_by_request_changes`, for `""` and for `"  \n"`.
  Nothing is written.

**Needs changes, end to end.**

- `needs_changes_keeps_the_worktree_and_branch_and_the_next_run_continues_there_with_the_note`.
  A task runs against a real repository and reaches `in_review` with a commit on its branch.
  `request_changes` with a note puts it at the bottom of `ready`, with the same
  `worktree_path` and `branch`. The next run starts in that same directory, on that same
  branch, and the reviewed commit is an ancestor of its `HEAD`. The prompt the stand-in CLI
  received on stdin equals the full `compose_prompt` output, asserted as one literal. Its
  `# Extra instructions` section ends with the changes-requested block.

**Reject, end to end.**

- `reject_sets_the_branch_aside_and_the_next_run_starts_on_a_fresh_branch_from_the_base`.
  After reject, `tasks.branch` and `tasks.worktree_path` are `NULL`, the worktree directory is
  gone, the old branch still exists with its commit, and `ReviewOutcome::set_aside_branch`
  names it. The next run creates `rimaia/<id>-<slug>-2`. Its branch does not contain the
  rejected commit, and its prompt ends with the rejected block.
- `reject_refuses_a_worktree_with_uncommitted_changes_and_names_the_count`. The refusal is
  asserted as the full sentence in Scope, for one change and for several. The directory, the
  columns, the column and `extra_instructions` are all unchanged. The existing cleanup tests
  that assert `ensure_committed`'s sentence pass unedited.
- `a_rejected_task_whose_worktree_was_already_removed_is_rejected_without_git_errors`.

**Approve.**

- `approve_moves_the_task_to_the_bottom_of_done`.
- `approving_with_auto_cleanup_on_removes_the_worktree_exactly_as_a_drag_to_done_does`.

**Refusals and atomicity.**

- `review_actions_refuse_a_task_that_is_not_in_review` (every other column),
  `review_actions_refuse_an_archived_task`, and
  `review_actions_refuse_a_queued_running_or_waiting_task`, each for all three actions. Each
  asserts that nothing changed.
- `reject_and_request_changes_refuse_a_failed_task_and_say_to_retry`.
- `review_actions_accept_an_idle_or_blocked_task`, for all three actions. A `blocked` task
  sent back to `ready` is still `blocked` afterwards.
- `a_refused_move_leaves_extra_instructions_unchanged`. An `in_review` task with no plan is
  refused by `request_changes`, and its `extra_instructions` equals what it was before.
- `a_refused_reject_leaves_the_worktree_and_extra_instructions_unchanged`. The same task, with
  a worktree holding a commit, is refused by `reject` for its missing plan. The worktree
  directory still exists, `tasks.worktree_path` and `tasks.branch` are unchanged, and so is
  `extra_instructions`. This is the test that every refusal is checked before git runs.

**Dependents.**

- `dependents_of_returns_direct_dependents_in_board_order`, and it includes an archived one.
- The two existing delete-refusal tests in `crates/core/tests/dependencies.rs`
  (`…1 other task depends on it…` and
  `the_delete_refusal_inflects_for_more_than_one_dependent`) pass **unedited**.
- `a_dependent_that_ran_on_this_tasks_branch_is_marked_built_on`, for a run row recorded
  both with and without a `base_sha`.
- `a_dependent_that_chained_from_another_dependency_is_not_marked_built_on`.
- `a_dependency_whose_only_run_committed_nothing_does_not_mark_a_default_branch_dependent`.
  The dependency's run records `head_sha` equal to its `base_sha`. After a reject clears its
  branch, a dependent's run branches from the default branch at that same commit, and is not
  marked.
- `reject_returns_every_dependent_and_marks_the_ones_that_built_on_it`, computed before the
  branch was cleared.

**The digest.**

- `a_digest_after_six_tasks_reports_each_outcome`. The six tasks are: one succeeded and in
  `in_review`; one failed, after **two** rows in the window (a first attempt that failed
  transiently and was retried, then a second that failed for good); a dependent of the
  failed one, blocked, whose entry names its blocker; one skipped because its repository
  does not allow unattended runs; one cancelled; and one succeeded with no recorded cost. The
  blocked and skipped tasks never run. The test asserts the full `Digest` value: five runs in
  the totals (1 + 2 + 1 + 1, none from the blocked or skipped task), `runs_without_cost: 1`,
  the failed entry's `runs: 2` with the second row's outcome, and `counts` of two `Completed`
  entries, one each of `Failed`, `Blocked`, `Skipped` and `Cancelled`, and zero for the other
  three outcomes. This is 017's six-task scenario, and 017 renders the same shape.
- `failures_and_blocked_chains_lead_the_digest`: the order is exactly the one in Scope.
- `a_task_with_three_runs_in_the_window_is_one_entry_with_the_newest_rows_outcome`. The entry
  has `runs: 3`. When one of the three has a `NULL` cost, the entry's `cost_usd` is `None`
  while the totals sum the other two and report `runs_without_cost: 1`.
- `a_board_with_only_blocked_or_unopted_ready_tasks_has_an_empty_digest`. No row has ended
  in the window and none is open. A ready task blocked by a `not_ready` dependency and a
  ready task in a repository without unattended opt-in exist. The digest has no entries,
  and its `DigestTotals` is asserted in full; the `get_review_digest` fixture row copies
  that value.
- `a_run_that_ended_at_or_before_the_marker_is_not_in_the_digest`, which covers the boundary
  instant.
- `without_a_marker_the_digest_covers_the_last_24_hours`.
- `archived_tasks_are_not_in_the_digest`.
- `marking_the_digest_seen_never_moves_the_marker_backwards`, and
  `marking_the_digest_seen_through_a_future_instant_is_refused`.
- `the_review_that_empties_the_queue_advances_the_marker`, for each of the three actions. It
  asserts that `Settings` and `Tasks` were both published after the commit.
- `the_review_that_empties_the_queue_leaves_an_empty_digest`, on a board that also holds a
  skipped task and a blocked task.
- `a_review_that_leaves_tasks_in_review_does_not_advance_the_marker`.
- `emptying_in_review_by_a_drag_or_an_archive_does_not_advance_the_marker`.

**Every door.**

- `every_registered_tool_has_a_run_scope_decision` passes with the six new tools, and
  `review_tools_are_refused_to_a_run` in `crates/core/tests/mcp_scope.rs` asserts `Refused`
  for each of them.
- `a_reject_over_mcp_writes_what_the_service_writes`, in `crates/core/tests/mcp_tools.rs`.
  A reject through the operator endpoint leaves `extra_instructions`, the column, the
  position and the worktree columns identical to a direct service call on a twin task. This is
  ADR-0006's one-rule-two-adapters property, tested.
- Both `generate_handler!` lists name all six commands, and
  `./scripts/check-command-wiring.sh` passes. `src/lib/commands.test.ts` asserts the exact
  command name and argument object that each of the six wrappers sends, by mocking
  `@tauri-apps/api/core`, not the wrappers.
- **The fixture rows.** `src/dev/fixtures/` has a row for each of the six commands, each
  answering as Scope's table gives it. In `src/dev/fixtures/fixtures.test.ts`, 028's
  `has an answer or an explicit refusal for every command commands.ts sends` and `never
  reaches invoke or listen in fixture mode` pass with no edit to either test. They pick the
  six names up from `commands.ts` themselves, and an edit to make them pass would mean the
  extraction no longer matches the wrapper shape. Two new tests sit beside them:
  - `it("answers the review verdicts without changing the seed")`: `approve_task`,
    `reject_task` and `request_task_changes` are each called on a seeded task, and
    `list_tasks` then answers exactly what it answered before.
  - `it("answers an empty review digest in every scenario")`: for every scenario name, the
    `get_review_digest` answer has no entries and a `counts` entry of `0` for each of the
    eight `DigestOutcome` variants.
- D32's appendix has the six rows, with the `reject_task` and `approve_task` Notes as Scope
  gives them. D28 has the dated amendment for `review_digest_seen_through`.
  `docs/seam-contract.md`'s "How to use this" table has a row for 034, added by this task if
  the backlog's authoring pass did not already add one, and naming the entries under Notes.
  No other seam entry is edited.

**Unchanged.**

- No migration is added. `.sqlx/` is regenerated for the new query macros and committed (D5).
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.

## Notes

**Seam entries to read.**

- D3 (who owns a settings key) and D5 (the `.sqlx` cache), as D33 amends it: this task
  lands before 040, so there is still one cache at the workspace root and D5's recipe is
  unchanged.
- D8 (refusals are `invalid`, and no new code).
- D9 (`interrupted` is a row status, not a run state), which the digest's outcome table
  reads.
- D12 and its 2026-09-02 amendment (`blocked_by_incomplete`, `blocking_title`).
- D16.1 and D16.6 (snake_case, and no plan text in responses).
- D18 (`NULL` is "not recorded").
- D20 points 1, 3 and 6: the unoverridable guard, auto-removal on `done`, and why
  irreversible acts have no tool. Reject is designed to stay on the right side of point 6.
- D28 part 4, for the marker's placement and the amendment this task appends.
- D29 points 4, 5 and 8: the newest row, and one digest entry per task.
- D30 point 5: the review actions are refused for every grant.
- D32 point 8 and its appendix.
- D4 and D6 apply as prohibitions: no migration, and no new dependency.

**Files to start from.** All of these exist on `main` @728a049.

- `crates/core/src/tasks/service.rs`: `move_into_column` (split it so it can run in a caller's
  transaction), `move_task_to_bottom`, `delete_task`'s inline dependents query,
  `ensure_ready_has_a_plan`, and the post-commit `auto_remove_on_done` call. `archive_task`
  is the model for a refusal sentence and for a live-task guard.
- `crates/core/src/tasks/dependencies.rs`: `dependencies_of` and `compare_dependency_order`.
  `dependents_of` belongs beside them.
- `crates/core/src/worktree/mod.rs`: `remove`, `prepare`'s idempotence rule, and
  `resolve_branch`, whose collision suffix is what gives a rejected task a fresh branch. See
  also `worktree/naming.rs`.
- `crates/core/src/worktree/cleanup.rs`: `is_live` and `ensure_committed`. Split the second
  as Scope says, rather than writing a second dirty-tree sentence.
- `crates/core/src/runner/prompt.rs`: `compose_prompt` and `EXTRA_INSTRUCTIONS_HEADING`.
  This task changes nothing in `prompt.rs`. The note reaches the prompt as ordinary extra
  instructions, and that is what makes the note format the prompt contract.
- `crates/core/src/scheduler/selection.rs`: `skip_reason` and `SkipReason`.
- `crates/core/src/analytics/mod.rs`: `runs_in`. Read it before writing the digest's run
  query, and do not widen it for the digest's sake. Its readers are D29 point 7's.
- `crates/core/src/db/settings.rs` and `crates/core/src/mcp/settings.rs`: the D3 accessor
  shape, and `set`, which `set_in` is split out of.
- `crates/core/src/mcp/scope.rs` (`Tool`, `as_str`, `run_access`), `mcp/server.rs`,
  `mcp/requests.rs` and `mcp/responses.rs`.
- `src-tauri/src/commands/tasks.rs` and `src-tauri/src/commands/mod.rs`, and both lists in
  `src-tauri/src/lib.rs`.
- `src/lib/commands.ts`, `src/lib/commands.test.ts` and `src/types.ts`.
- `src/dev/fixtures/` (the seed, its command table and `constants.ts`'s `FIXTURE_NOW`) and
  `src/dev/fixtures/fixtures.test.ts`, all task 028's. 028 is not in `depends_on`; it
  comes before 033, which this task depends on, and 033 already edits the seed's `get_run`
  row. If `src/dev/fixtures/` does not exist when this task starts, 028 has not landed:
  stop and say so rather than building the fixture mode here.
- Tests to extend: `crates/core/tests/tasks.rs`, `dependencies.rs`, `prompt.rs`,
  `runner_process.rs` (the pattern for driving a real run end to end), `mcp_scope.rs` and
  `mcp_tools.rs`. The digest's and the actions' own tests go in a new
  `crates/core/tests/review.rs`.

**Migration: none.**

**What 033 provides.** `runs.head_sha` written at `finish_run`, `runs.base_sha` written at
`start_run`, and the review bundle on `get_run`. This task reads only the two columns, for
`built_on`. It reads no bundle and runs no git to build a digest. The digest is rows only,
because in team mode the board has no worktree (ADR-0033 point 7).

**What the next tasks expect.**

- **017** calls the six wrappers by these names and renders `ReviewDigest`, `ReviewOutcome`
  and `TaskDependent` as returned. It warns before reject and request changes from
  `get_task_dependents`, because both moves take the task out of `in_review` and so block
  every dependent (ADR-0008's amendment point 2). It shows `set_aside_branch` after a reject.
  017 as written adds no control that calls `mark_review_digest_seen`. The marker's automatic
  rule is what empties the digest. A night that produced only failures leaves the review queue
  empty, so the digest stays until the next finished review, or until something calls
  `mark_review_digest_seen`, an operator over MCP for example. If that turns out wrong in real
  mornings, the fix is a key in 017 calling the existing wrapper, not a second rule.
  017 finds the six fixture rows in place and replaces only the `get_review_digest` and
  `get_task_dependents` answers, per scenario, for its `review-*` scenarios. When it does,
  `answers an empty review digest in every scenario` stops being true for those scenarios,
  and 017 narrows that test to the scenarios it did not seed rather than deleting it.
- **035** re-keys `run_access` by grant, and must keep all six tools ✘ for every grant. It
  adds `kind` to the digest's newest-row read. It adds a loop summary field to `DigestEntry`,
  a field added and not a shape changed.
- **021** decides what a finished review or fix does to a card. It must not route its exits
  through these actions: they are the human's verdict, and the loop's exits are not one.
- **039** scopes every function here by team. The functions already take `&ServiceContext`,
  so there is no raw pool to convert. It also decides whether "leaves no task in `in_review`"
  means the caller's team's queue, which it should.
- **045** bumps `tasks.plan_revision` when a note is appended (ADR-0032 point 3: every edit to
  `plan` or `extra_instructions`). Route the append through one private function in
  `review::actions` that writes `extra_instructions`, so 045 has exactly one place to add it.
- **036, 066 and 059: reject's local half.** Reject writes the board and the disk, and
  team mode puts those on different machines. 036's `BoardPort` is the runner's voice, and
  reject is a human's action, so reject does not go through the port. What 036 must know is
  that `set_aside_worktree` is not a board write. When 066 takes `worktree_path` off the
  board, it splits reject along the line Scope draws. The dirty-tree check,
  `worktree::remove` and the `worktree_path` write (becoming
  `MachineStore::forget_worktree`) are the runner's. The note, `branch = NULL`, the move
  and the marker are the board's. 066's "no code writes `tasks.worktree_path`" includes
  reject. In solo mode both halves still run in one process, and the order in Scope still
  holds. In connected mode (059) the dirty-tree refusal is a check on the runner that holds
  the worktree, and a synchronous refusal to the person who clicked is not guaranteed. Two
  designs keep the invariant that reject never discards uncommitted work. Either the board
  waits for the runner's check before it commits, or the runner declines the removal and
  keeps the worktree, recorded against the set-aside branch. Whichever task moves the check
  chooses between them, and says so in its own Scope. It may not choose a third design that
  removes a dirty tree.
- **046** migrates the six appendix rows into the registry without judging them. The Notes
  on `reject_task` and `approve_task` are what make that possible.
- **051** decides who may approve.

**Where this could go wrong.** The quiet failure is a note appended twice, or appended to a
task that did not move. It looks like a flaky prompt weeks later. The one-transaction rule and
`a_refused_move_leaves_extra_instructions_unchanged` exist for it. The second is a reject
that removes a worktree and is then refused, which is why every refusal is checked before git
runs. The third is `built_on` overcounting after a reject renames the branch. The `base_sha`
clause, not the branch clause, is what keeps it right, and the end-to-end reject test should
chain a dependent to prove it. The fourth is a digest that never empties. Context entries
are gated on run-backed ones for that reason, and
`a_board_with_only_blocked_or_unopted_ready_tasks_has_an_empty_digest` guards it.

**Size.** About 700 lines of core (actions 250, note 50, dependents 100, digest 300), 250 of
adapters (commands, tools, projections, wrappers, types), 80 of fixture rows and their two
tests, and 1,300 of tests. That is roughly 2,400 lines, still inside one session. If it runs
over, cut in this order: the `span_seconds` total, then `mark_review_digest_seen` as a
command. Keep it as a service and an MCP tool, because 017's automatic rule does not depend
on the command. Cutting the command also removes its wrapper and its fixture row, since the
coverage test only asks for rows that `commands.ts` sends. The fixture rows themselves are
not on the cut list: without them `npm run test` fails. The note, the three actions,
`built_on` and the digest's order are the task.
