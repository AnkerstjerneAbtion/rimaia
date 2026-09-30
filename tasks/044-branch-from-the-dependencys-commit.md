---
id: "044"
title: Branch from the dependency's commit
milestone: v0.5
status: ready
depends_on: ["043"]
adrs: ["0033", "0008"]
size: S
---

# Branch from the dependency's commit

## Goal

A dependent task's worktree starts at the **commit** its chosen dependency's latest
successful implementation or fix run ended on (`runs.head_sha`), not at the tip of a local
branch name. This is
[ADR-0033](../docs/adr/0033-repositories-belong-to-the-team-checkouts-to-the-runner.md)
point 5, and it applies in every mode, solo included, so there stays one code path.

Four things do not change, and this task must not move them:

- **Satisfaction is the column, and only the column** (ADR-0008 amendment point 2). A
  dependency in `in_review` or `done` is satisfied whether or not it has a run.
- **The choice of dependency** is still column rank, then ascending `position`
  (ADR-0008 amendment point 3, `tasks::dependencies_of`).
- **A dependency that cannot be a base falls through** to the next candidate and then to
  the default branch, with a warning. "Has no branch" becomes "has no successful
  `head_sha`".
- **`runs.base_ref` keeps holding a name**, and task 033's `runs.base_sha` holds the
  commit. For a chained run that commit is the dependency's `head_sha` exactly. Together
  the two columns are ADR-0033's "records the commit, not only the branch name". From this
  task on, `base_sha` is the authoritative one of the two; see "What `base_ref` means now".

The resolution also moves to the board side of the port. D31 point 6 gives this task
`RunContext::base`: the board decides the base, and the runner creates the worktree from
it without reading the board's dependency graph itself.

## Why now

Task 053 and task 059 are the first to put two machines on one board. From then on, a
dependency A can run on one runner and its dependent B on another, and B's runner has
never seen A's local branch. The commit is the only thing both can name, and 057's push
postcondition is what puts it on the remote. Changing the rule before any of that exists
means it is proven here, in solo mode, against real repositories in a `TempDir`, instead
of being debugged across a network.

The rule is also more honest in solo mode, where it is already observable. A branch moves.
If A is retried after B has started, the branch name `rimaia/a` now points at A's newer
work, while B was built on the older commit. A branch name cannot record that. A commit
can, and so can a successful run that ended on it.

033 has written `runs.head_sha` at every finish since it landed, and 036 put `RunContext`
on the claim. This task only reads what those two provide. It adds no column and no
migration.

## What this task expects to find

Three earlier tasks leave `worktree::prepare` in a shape this task finishes. Check each
before writing code.

- **After 041, `prepare` still takes a board `&ServiceContext`**, and uses it for exactly
  three reads: `fetch_task`, `repo::get` and `base_ref::resolve`. Its clone path and
  worktree root come from 041's `Checkout` through `MachineContext`, and its branch write
  goes through `BoardPort::record_branch`. `run_task` and `plan_claimed` still hold that
  board context for one reason only, which is to pass it to `prepare`.

  041's criterion "`run_task` takes no `ServiceContext`" cannot hold until this task lands,
  because `prepare` cannot give up its board reads before `RunContext::base` exists. 041
  carries that carve-out, and this task removes it. **After 044, 041's criterion holds as
  written**, for `run_task` and for the planner's `plan_claimed`.

  If 041 left `prepare` taking no board context at all, it read the dependency graph
  through some other door, and this task does not know which. If `run_task` holds a board
  `ServiceContext` for anything other than `prepare`, 036 and 041 left a use D31 did not
  plan for. In either case, **stop and ask** before building on it.
- **`RunContext` is built in one place**, `board::service`, for `preview`, `claim` and
  `run_context` (036). If it is built in more than one, stop and ask rather than resolving
  the base in several places. D31's premise is that each decision is made once, board-side.
- **The loop tests live in `crates/runner/tests/queue.rs`** (042 point 9). Every
  queue-level test below goes there. If 042 put them somewhere else, use wherever it left
  them.

## Scope

**`runs::latest_successful_head`**, in `crates/core/src/runs/mod.rs`, with the query from
D29 point 5:

```sql
SELECT id, head_sha FROM runs
 WHERE task_id = ?1 AND status = 'succeeded'
   AND kind IN ('implementation', 'fix')
   AND head_sha IS NOT NULL AND trim(head_sha) <> ''
 ORDER BY attempt DESC LIMIT 1
```

It returns `Result<Option<SuccessfulHead>>`, where `SuccessfulHead { run_id: String,
head_sha: String }`.

- **Implementation and fix rows only.** Those are the two kinds whose job is to produce
  commits. A succeeded review is left out. A reviewer that leaves `HEAD` alone records the
  head it was handed, which is already the head of the implementation or fix row before
  it, so leaving reviews out changes nothing in the clean case. A reviewer that moved
  `HEAD` has made commits nobody reviewed. Task 021 treats that as a failed review
  (`review_changed_branch`, "HEAD moved → Unreviewed"), but the row's `status` is still
  `succeeded`, so without the filter a dependent would build on that reviewer's commits.
  D29 point 5 carries this rule. A failed fix is skipped even if it committed.
- **It returns the run's `id` as well as its `head_sha`.** Task 045 needs the run that
  produced the base, because the base commit's author is the owner of that run's runner.
- **A blank `head_sha` is excluded in the `WHERE`, exactly like a NULL.** The sqlite3 CLI
  is a writer (ADR-0003), and two ways to spell "absent" is one too many. Filtering blank
  after the fetch would make the two spellings behave differently: a NULL falls through to
  an older row, while a blank would end the search.
- `ORDER BY attempt`, never `ended_at`, for D29 point 2's reason: `attempt` is one
  sequence per task across kinds.
- It reads under the scoped context, like every board read after 039. It adds no
  unscoped pool function.

**The rule, in `crates/core/src/worktree/base_ref.rs`.** The file stays where it is,
because its module doc is where the rule is explained. What changes:

- `choose` stays pure. It takes the dependencies in `dependencies_of` order, each paired
  with its `Option<SuccessfulHead>`. The chosen dependency is the first that is
  **satisfied** (`BoardColumn::satisfies_a_dependency`) **and has a successful head**.
  `has_branch` goes. A branch name no longer decides anything; it only labels.
- `resolve` reads `dependencies_of` and then one `latest_successful_head` per
  dependency. A task has a handful of dependencies at most, and one query per edge keeps
  D29's single function as the only place "successful head" is defined. `resolve` becomes
  `pub(crate)`, because `board::service` now calls it.
- The result, `ResolvedBaseRef` today, becomes the public DTO `RunBase` in
  `crates/core/src/board/types.rs`, because it now crosses the port. Both new types are
  `#[serde(rename_all = "camelCase")]` like every other DTO there:

  ```rust
  pub struct RunBase {
      /// The label: what `runs.base_ref` records and the panel shows.
      pub base_ref: String,
      /// `None` when the base is the repository's default branch.
      pub dependency: Option<BaseDependency>,
      /// ADR-0008's warning, with the wording below.
      pub warning: Option<String>,
  }
  pub struct BaseDependency {
      pub task_id: String, // the chosen dependency
      pub title: String,   // for the refusal below
      pub run_id: String,  // the run whose head_sha is `commit` (045 reads its runner)
      pub commit: String,  // that head_sha, in full, never abbreviated
  }
  ```

  The four facts about the dependency are one `Option`, because they are present or absent
  together. Four separate `Option`s would let a base carry a commit with no run that
  produced it, and 045 would have to handle a state that cannot happen.
- **`base_ref`, the label.** With a dependency chosen, it is the dependency's
  `tasks.branch` when that is recorded and non-blank. When the branch has been cleaned up
  (task 016, D20: `worktree::remove` with `delete_branch` clears `tasks.branch`) or is
  blank, it is the full commit, because a label nobody can resolve is worse than a hash.
  The board never runs git, so "cleaned up" means only that the column is NULL or blank.
  With no dependency chosen, the label is the repository's default branch, as today.
- The module doc's four-point rule and `resolve`'s doc ("a local branch name, never
  `origin/<branch>`") are rewritten to ADR-0033 point 5's rule and reasons, including the
  kind filter and the paragraph below on what `base_ref` means now. The "Why the satisfied
  pair ranks `in_review` before `done`" section stays.

**What `base_ref` means now.** D28 part 6 gives 033's migration the comment "`base_ref`
keeps the branch name it has always held; `base_sha` is what that name resolved to". After
this task that is no longer exact. B's `base_ref` is A's branch name, and B's `base_sha` is
A's last successful head, which can be behind that branch's tip: the failed-fix and
hand-commit tests below exist to prove it. **`base_ref` is the label, and `base_sha` is
authoritative and may be behind the label's tip.** A migration that has run cannot be
edited, so the refined meaning is written where readers will find it: in `base_ref.rs`'s
module doc, and in D29 point 5, which already says so.

**The warning.** The sentence for a chosen dependency is unchanged, with `base_ref` as the
label:

> This task branches from "a" (rimaia/a). "b" is also a dependency and is not in that base
> — merge into it what you need, or run this task again once the rest have landed.

The sentence for no chosen dependency changes. "Has never run" is no longer the criterion,
and a dependency can now fail to be a base for two different reasons with two different
remedies: it is not satisfied, or it is satisfied and has no successful head. Each
dependency is named under its own reason:

```text
This task branches from {default_branch}: none of its dependencies can be built on yet.
{clauses}.
```

The two lines are one sentence pair joined by a space. `{clauses}` holds up to two
clauses, in this order and joined by `", and "`:

- the unsatisfied dependencies: `"a" is not in review or done`, or in the plural
  `"a", "b" are not in review or done`;
- the satisfied dependencies with no successful head: `"c" has no successful run to build
  on`, or in the plural `"c", "d" have no successful run to build on`.

A clause with no names is left out. Names keep `dependencies_of` order within each clause.
The plural follows today's inflection, with the verb inflected as well as the noun. Written
out exactly:

> This task branches from main: none of its dependencies can be built on yet. "a" is not in
> review or done.

> This task branches from main: none of its dependencies can be built on yet. "a" has no
> successful run to build on.

> This task branches from main: none of its dependencies can be built on yet. "a", "b" are
> not in review or done, and "c", "d" have no successful run to build on.

The pure tests in `base_ref.rs` and the integration tests assert these as exact strings.

**The base crosses the port.** `RunContext` (D31 point 2) gains `base: RunBase`.
`board::service` builds it wherever it builds `RunContext` today (`preview`, `claim`,
`run_context`) by calling `base_ref::resolve` under the context's scope. The runner then
uses the base it was handed:

- **`worktree::prepare` takes no board `ServiceContext`.** It reads the task and the
  repository from the `RunContext` it is given, the base from `RunContext::base`, and the
  checkout from `MachineContext` as 041 left it. Its branch write stays
  `BoardPort::record_branch`. `fetch_task`, `repo::get` and `base_ref::resolve` are no
  longer called from `prepare`, and `run_task` and `plan_claimed` drop the board context
  they held only for it.
- **Both production callers pass the claim's context**: `run_task`'s `prepare_worktree` in
  `runner/process.rs`, and the planner's `plan_claimed` in `runner/strategy.rs`.
- **The base a run records is always the one handed to `prepare`**, taken from the claim.
  036 composes the prompt from a `run_context` read taken after `prepare`, and that read
  resolves the base again. If the dependency graph changed between the claim and that read,
  the two can disagree. The re-read's `base` is never used for the worktree or for the run
  row. Only the prompt comes from that read, and the prompt does not contain the base.
- Whatever `prepare` calls the revision, whether for `ensure_base_ref_exists`, for
  `git worktree add <path> -b <branch> <revision>`, or for 033's
  `git merge-base <revision> HEAD` that yields `base_sha`, it is
  `dependency.commit` when there is one and `base_ref` otherwise. A fresh worktree off a
  dependency therefore records `base_sha == head_sha` byte for byte. A reused worktree
  records the fork point against that commit, which is where the retry case below comes
  from.
- `Worktree` carries the `RunBase` through to `board.start_run`, so
  `StartRun::base_ref` is the label and `StartRun::base_sha` is the commit.
- **Review and fix rows are unaffected.** They copy the implementation row's `base_ref` and
  `base_sha` (D29 point 4, task 021), and this task does not change where those come from.
- `worktree::status` and `worktree::diff_summary` keep their current rule: prefer the
  recorded `base_ref`, and otherwise resolve fresh. They are the only other callers of
  `resolve`. After this task `base_ref::resolve` has exactly three callers: the board's
  `RunContext` builder, `status` and `diff_summary`.

**A recorded commit the clone does not have is refused, not skipped.** `prepare` already
runs a best-effort `git fetch --prune` before it checks the base. If `dependency.commit` is
still not in the object store after that fetch, `prepare` fails with `Error::invalid` (D8:
no new code) before any branch, directory, worktree record or column is written. The
message is exactly:

```text
"{repository}" does not have {commit}, the commit "{dependency}" last finished on. Fetch it
into this clone, or remove "{dependency}" from this task's dependencies and run it again.
```

on one line, where `{repository}` is `RunContext::repository`'s name, `{commit}` is
`dependency.commit` in full, and `{dependency}` is `dependency.title`. The remedy does not
name the default branch: with another satisfied dependency, removing this one makes the
task build on that one instead, and a sentence promising the default branch would be
wrong.

The run then fails the way a mistyped default branch fails today. It does not fall
through, because the board chose this base and the board never runs git (D31 point 6,
ADR-0033 point 7). If the runner fell back to the default branch on its own, it would be
overruling the board's decision based on a fact the board cannot see, and B would be built
without A's code while the record said it was built on A.

**ADR-0008 gets a pointer.** One dated line under ADR-0008's amendment says that
ADR-0033 point 5 replaces "the dependency's branch" with "the dependency's latest
successful implementation or fix `head_sha`", and that satisfaction is unchanged. The line
is appended, and no existing sentence is edited. This is the pattern 038 uses for
ADR-0029.

**The seam contract.** Add 044's row to "How to use this" if Phase 0 did not: D4 · D5 ·
D8 · D18 · D20 · D28 · D29 · D31 · D32 · D33.

**The offline cache.** `latest_successful_head` is a new `query!`. Regenerate
`crates/core/.sqlx/` with D33's recipe. No crate, migration or CI step is added, so
CLAUDE.md and `ci.yml` do not change.

**The tests' callers of `prepare`.** About 77 test call sites reach `worktree::prepare`
(`tests/worktree.rs`, `tests/worktree_cleanup.rs`, `tests/archive.rs`), and each gains the
context argument. They do not each build a `RunContext` by hand:

- A caller with no dependencies, or one that does not care about the base, goes through one
  testing helper in `crates/core/src/testing/`, which reads
  `TestContext::board().preview(task_id)` and passes that context. If 041 already left a
  helper that wraps `prepare`, extend it rather than adding a second. The call expression
  changes and nothing else in the test does.
- A chaining test takes its context from `preview` or `claim` explicitly, because the base
  is what it is testing.

## Out of scope

- **Any change to satisfaction.** `TASK_SUMMARY_SELECT`'s predicate,
  `selection::skip_reason` and `lib/board.ts::cardBadge` are untouched. A dependency with
  no run at all still unblocks its dependents (ADR-0008 amendment point 2).
- **A branch-name fallback for runs from before 033.** Those rows have no `head_sha`, so a
  dependency whose only successful run predates 033 is not a base. Its dependent falls
  through to the default branch with the warning. That is ADR-0033 point 5's single code
  path. **If this is judged unacceptable for an installation with a live chain at
  upgrade time, stop and ask. Do not add a second resolution path.**
- **Special-casing a `done` dependency whose branch has been merged.** B still branches
  from A's head. If the merge kept A's commits, B's PR against the default branch shows
  only B's work. If A was squash-merged, B's PR also shows A's commits. That is the
  stacked-PR cost ADR-0008 accepted, and the ranking already prefers `in_review`.
- **Connected mode.** Fetching a specific commit from the remote, the push postcondition
  that puts `head_sha` there (057), checkouts by remote (054), and a runner on another
  machine (053, 059). The best-effort fetch `prepare` already runs is all this task
  relies on.
- **Measuring `status` or `diff_summary` against `base_sha`.** Since 033, the morning
  review renders the recorded bundle, which is already measured from `base_sha`. The live
  path is its fallback and keeps measuring against the recorded name. Where a dependency
  retry fast-forwards A's branch (the normal case), both give the same diff.
- **The multi-dependency merge step** ADR-0008 calls "the eventual fix". Other satisfied
  dependencies are still only named in the warning.
- **Any interface change.** The panel already shows `base_ref`, and `base_ref` is still a
  name. No command, no MCP tool and no field in `src/types.ts` changes, so
  `check-command-wiring.sh` is unaffected.

## Acceptance criteria

- `runs::latest_successful_head` exists with D29 point 5's query, returns the run's `id`
  and `head_sha`, and `crates/core/.sqlx/` is regenerated and committed. No migration is
  added in either set.
- **The query, against the harness database with a `TestClock`:**
  - `the_latest_successful_head_skips_running_failed_cancelled_and_interrupted_rows`
  - `the_latest_successful_head_is_the_highest_attempt_among_implementation_and_fix_rows`:
    a succeeded fix after a succeeded implementation wins, and the returned `run_id` is
    the fix's.
  - `a_succeeded_review_that_moved_head_is_not_a_base`: an implementation succeeds at H1,
    and a review row then succeeds with `head_sha` H2. The result is H1 and the
    implementation's `run_id`.
  - `a_blank_head_sha_is_no_head_and_an_older_row_still_counts`: the newest succeeded row
    has a blank `head_sha`, and the result is the older row's head, exactly as for a NULL.
- **The pure rule** (`base_ref.rs` unit tests, no pool):
  - `one_satisfied_dependency_with_a_successful_head_is_the_base_and_needs_no_warning`:
    `base_ref` is the dependency's branch, and `dependency` is `Some` with the dependency's
    `task_id` and `title`, the head's `run_id`, and `commit` equal to the head.
  - `an_unsatisfied_dependency_is_never_a_base_even_with_a_successful_head`: asserts the
    "not in review or done" sentence exactly.
  - `a_satisfied_dependency_with_no_successful_head_cannot_be_a_base`: asserts the "has no
    successful run to build on" sentence exactly.
  - `a_no_base_warning_names_each_dependency_under_its_reason`: four dependencies, two
    unsatisfied and two satisfied with no head, in interleaved order. Asserts the plural
    sentence above exactly.
  - `the_first_satisfied_dependency_with_a_head_wins_over_earlier_ones_without`
  - `two_dependencies_base_off_the_first_and_warn_about_the_other`: asserts the unchanged
    sentence exactly.
  - `a_dependency_whose_branch_is_gone_is_named_by_its_commit`: the branch is NULL, and
    `base_ref` equals `dependency.commit`.
  - The existing `a_blank_branch_is_the_same_as_no_branch` is renamed
    `a_blank_branch_is_named_by_its_commit_like_a_missing_one`. Its dependency gains a
    successful head, and it asserts that `base_ref` is that commit and not the default
    branch.
  - The existing `a_task_with_no_dependencies_branches_from_the_default_branch` keeps its
    name and inputs, and compares against
    `RunBase { base_ref: "main", dependency: None, warning: None }`.
- **Real git in a `TempDir`** (`crates/core/tests/worktree.rs`). Runs are opened and
  finished through the same board services production uses, with the `head_sha` read
  from the worktree by `git rev-parse HEAD`. A test moves a card to another column only
  where the case needs it to be somewhere a finish would not have put it. Git is never
  mocked, and no test sleeps.
  - `a_dependent_branches_from_its_dependencys_successful_head_and_git_merge_base_proves_it`
    replaces today's `a_dependent_branches_from_its_dependency_and_git_merge_base_proves_it`.
    B's `HEAD` equals A's `head_sha`. `git merge-base` of the two branches is that
    commit. B's run row records `base_ref` = A's branch and `base_sha` = A's `head_sha`.
  - **`a_dependent_branches_from_the_last_succeeded_head_and_skips_a_failed_fix`**
    (D29's required test). A's implementation succeeds at H1, and a fix run then commits
    H2 on A's branch and fails. A is in `in_review`, and A's branch tip is H2. B's `HEAD`
    is H1, and the fix's file is absent from B's checkout.
  - `commits_added_to_a_dependencys_branch_after_its_run_are_not_in_the_base`. A succeeds
    at H1. Someone then commits H2 on A's branch by hand and drags A to `done`. B starts
    at H1.
  - `a_dependency_with_a_branch_but_no_successful_run_is_not_a_base`. A has a worktree
    and commits but no successful run, and is dragged to `in_review` by hand. B starts at
    the default branch's tip, and the warning is the "has no successful run to build on"
    sentence exactly. This is the deliberate behaviour change from today, and the test's
    comment says so.
  - **`a_dependency_retried_after_its_dependent_started_leaves_the_dependent_on_the_earlier_commit`**.
    A succeeds at H1. B is prepared and its run opened: `base_sha` = H1. B's run ends
    with a usage limit, so its worktree stays. A goes back to `ready`, runs again and
    succeeds at H2. Then:
    - B is prepared again on its reused worktree, and B's second run row records
      `base_sha` = H1, not H2;
    - B's checkout does not contain A's second-run file;
    - a new task C that depends on A starts at H2.
  - `a_dependency_commit_missing_from_the_clone_is_refused_before_anything_is_created`.
    A succeeds at H1 and is dragged to `done`. Its branch is then deleted the way task 016
    deletes it, which clears `tasks.branch`, and the reflog is expired and the object
    pruned (`git reflog expire --expire=now --all`, `git gc --prune=now`), so H1 is gone
    from the clone. This is the squash-merged-and-cleaned-up case under "Known edges".
    Preparing B:
    - fails with `Invalid` and the refusal sentence above, exactly, with `{commit}` = H1 in
      full;
    - leaves no worktree directory and no `rimaia/…` branch;
    - leaves B's `tasks.branch` NULL, and writes no `worktrees` row for B in the runner
      store (041 moved the path there, so `tasks.worktree_path` is no longer evidence of
      anything).
  - `a_dependency_that_has_never_run_cannot_be_a_base` asserts the "has no successful run
    to build on" sentence exactly.
  - `an_unsatisfied_dependency_leaves_the_dependent_on_the_default_branch` gives A a
    successful run, then drags A back to `ready`, and asserts the "not in review or done"
    sentence exactly.
  - `two_dependencies_base_off_the_higher_one_and_warn_about_the_other`,
    `the_resolved_base_is_recorded_on_the_run` and
    `a_task_that_has_never_run_reports_a_freshly_resolved_base` still hold. Each is
    rearranged to record a successful run where it needs a base, and asserts the new
    sentence where it asserted the old one.
- **Through the port and the queue:**
  - `the_run_context_carries_the_base_the_board_resolved`: `preview` and `claim` through
    `TestContext::board()` return `base.base_ref` = A's branch, `base.dependency` with
    `task_id` = A's id, `run_id` = A's succeeded run and `commit` = that run's `head_sha`,
    and the expected `warning`. `preview` writes nothing.
  - `the_base_a_run_records_is_the_claims_even_when_a_later_read_disagrees`: B is claimed
    with A's head as its base. A is then dragged back to `ready`, so `run_context` now
    returns the default branch. B's worktree is prepared from the claim's context, and the
    `StartRun` it produces carries A's branch as `base_ref` and A's head as `base_sha`.
  - In the loop tests (`crates/runner/tests/queue.rs`, see "What this task expects to
    find"), `a_to_b_to_c_run_in_dependency_order_in_one_queue_pass`, with `FakeCli`
    replaying fixture streams, also asserts that B's `base_sha` is A's `head_sha` and C's
    is B's. Its existing `base_ref` assertions hold unchanged.
  - In the same file,
    `a_hand_finished_dependency_still_unblocks_and_its_dependent_builds_on_the_default_branch`:
    through the queue, A is dragged to `done` with no run. B is claimed, not skipped as
    `DependencyNotSatisfied`, and its run records `base_ref` = the default branch.
- `crates/core/tests/tasks.rs`'s satisfaction tests
  (`a_dependency_in_review_satisfies_and_one_in_ready_does_not`,
  `a_hand_finished_dependency_in_done_satisfies_without_any_run`,
  `a_dependency_dragged_back_out_of_in_review_blocks_its_dependents_again`) pass
  **unchanged**, which shows satisfaction did not move.
- `worktree::prepare` takes no board `ServiceContext`. It reads the task and repository
  from the `RunContext` it is given, and the base from `RunContext::base`. `run_task` and
  `plan_claimed` take no board `ServiceContext`, so 041's "`run_task` takes no
  `ServiceContext`" holds with no carve-out.
- `base_ref::resolve` has exactly three callers: `board::service`'s `RunContext` builder,
  `worktree::status` and `worktree::diff_summary`. Nothing under `crates/core/src/runner/`
  or `crates/runner/src/` calls it or `tasks::dependencies_of`.
- `base_ref.rs`'s module doc states ADR-0033 point 5's rule, the kind filter, and that
  `base_ref` is the label while `base_sha` is authoritative. No comment in `worktree/`
  still says the base is "a local branch name, never `origin/<branch>`".
- ADR-0008 carries the dated pointer to ADR-0033 point 5, and no existing sentence of
  ADR-0008 is edited.
- "How to use this" in `docs/seam-contract.md` has 044's row.
- Every CI check passes, run with `SQLX_OFFLINE=true` exported: `npm run typecheck`,
  `npm run test`, `npm run build`, `cargo test -p rimaia-core`,
  `cargo test -p rimaia-runner`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo clippy -p rimaia-runner --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.

## Notes

**Read first.** ADR-0033 point 5 (the rule) and point 4 (why `head_sha` exists in every
mode). ADR-0008's amendment points 2, 3 and 4. ADR-0017's review exits, for why a review
row is not a base. Seam entries:

- **D29** point 5 (the query, its kind filter, what `base_ref` means now, the required
  test) and point 4 (a review or fix row copies its implementation row's `base_ref` and
  `base_sha`, which is why `recorded_base_ref` stays right);
- **D31** point 2 (`RunContext`, `StartRun`), point 4 (`preview` is advisory, and a run is
  composed from its claim), point 6 (044's field, and "`resolve` moves board-side") and
  point 7's `base_ref::resolve` row;
- **D28** part 6's comment on 033's file, which this task refines as described under
  "What `base_ref` means now";
- **D20**, which owns the branch cleanup behind "a branch that is gone is named by its
  commit";
- **D32**, because `status` and `diff_summary` are local commands;
- **D18** (NULL is "not recorded"), **D8** (no new error code), **D33** (the cache
  recipe, amending D5), and the **D4 amendment** (no migration here: a task that finds it
  needs one stops and asks).

**Files to start from.** These are on `main` @ 728a049:

- `crates/core/src/worktree/base_ref.rs`: `resolve`, `choose`, `has_branch`, `warn_about`
  and their tests.
- `crates/core/src/worktree/mod.rs`: `prepare`, `existing_worktree`,
  `ensure_base_ref_exists`, `recorded_base_ref`, `status` and `diff_summary`.
- `crates/core/src/worktree/git.rs`: `commit_exists` and `worktree_add`.
- `crates/core/src/tasks/dependencies.rs`: `dependencies_of` and
  `compare_dependency_order`.
- `crates/core/src/runs/mod.rs`, where `latest_successful_head` goes.
- `crates/core/src/runner/process.rs` (`prepare_worktree`) and
  `crates/core/src/runner/strategy.rs` (`plan_claimed`'s `worktree::prepare`).
- `crates/core/src/db/models.rs`: `BoardColumn::satisfies_a_dependency`.
- `crates/core/tests/worktree.rs`: the "Branch chaining" section and its `Fixture`,
  `commit_in` and `file_for_review` helpers.
- `crates/core/tests/tasks.rs`: the satisfaction tests.
- `crates/core/src/testing/repo.rs`: `TempRepo`.
- `docs/adr/0008-dependency-semantics-and-branch-chaining.md`.

These exist only once earlier tasks on this branch have landed:

- `crates/core/src/board/{port,types,service}.rs` and `TestContext::board()` (036);
- `worktree::prepare`'s `base_sha` computation and `NewRun::base_sha` (033);
- `RunKind` (035), which the query and the fix-row and review-row tests use;
- the fix phase (021), if a test opens a fix row through the loop rather than directly
  through `start_run`;
- `MachineContext`, the `worktrees` rows and `BoardPort::record_branch`'s production
  caller (041);
- `crates/runner/tests/queue.rs` and its `a_to_b_to_c_…` test (042).

**What earlier tasks provide.**

- **033:** `runs.head_sha`, written at every finish whenever `HEAD` resolves, for every
  kind; `runs.base_sha` as `git merge-base <base> HEAD`, written at the open. This task
  changes only which revision that merge-base is taken against.
- **035:** `runs.kind`, and D29's rule that `attempt` is one sequence across kinds.
- **036:** `RunContext`, `board::service` as the one place it is built, and `run_task`
  taking a `Claim`.
- **038 and 039:** the scoped `ServiceContext`, every board read filtered by team, and a
  rebuild of `runs` that keeps `head_sha` and `base_sha`.
- **041:** `prepare`'s signature as described under "What this task expects to find", and
  the runner-side worktree record.
- **042:** the loop tests' new home.
- **043:** the lease that fences the `finish_run` that records `head_sha`.

**Known edges, accepted.**

- **A reused worktree after a rewritten dependency.** A reused worktree's `base_sha` is the
  fork point against the dependency's *current* successful head. When A's retry builds on
  its own branch, which is the normal case, that fork point is the earlier head, and the
  retry test pins it. When A's history is rewritten instead, the fork point moves toward
  the default branch, and B's recorded base understates what B was built on.

  That is not rare. **Task 034's reject does it as a normal product action**: it sets A's
  branch aside and clears `tasks.branch`, and A's next run starts on a fresh branch from
  the base. If B has a waiting retry on a reused worktree, B's next row records `base_sha`
  as roughly the default branch, and B's bundle presents A's rejected commits as B's work.

  This is accepted, for two reasons. The human is told at the moment it matters: 034's
  reject lists B among the dependents that built on A (`built_on` matches B's earlier row,
  whose `base_sha` is A's rejected head), and the remedy for a dependent built on rejected
  work is the human's, not the runner's. And the fix would contradict ADR-0008 amendment
  point 4 ("`prepare` never reads it"), because it means reading the previous row's
  `base_sha` in `prepare`. **If this is judged unacceptable, stop and ask about amending
  ADR-0008 point 4. Do not read the previous row in `prepare` without that amendment.**
  033 accepts the same edge for a rebased run.
- **A `done` dependency that was squash-merged and cleaned up.** A was squash-merged, its
  branch was deleted (task 016, or the remote's auto-delete followed by a prune), and its
  commits were garbage-collected. Before this task, B fell through to the default branch,
  because `tasks.branch` was NULL. After it, A still has a successful head, so A is chosen,
  and `prepare` refuses because the clone no longer has the commit. The refusal's first
  remedy, "Fetch it into this clone", cannot work here: the commit exists nowhere. The
  second one does, and it is what the user should do, because A's work is already on the
  default branch. This is an accepted behaviour change. A silent fallback would be the
  runner overruling the board, which the refusal exists to prevent, and the refusal test
  pins this exact case.

**What the next tasks expect.**

- **045** adds the authorship facts to `RunContext` beside `base`. It builds the base
  commit's consent piece from `RunBase::dependency`: `task_id` is the piece's task,
  `commit` its revision, and the runner recorded on the run `run_id` names its author.
- **052** serialises `RunContext::base` over HTTP unchanged. `RunBase` and
  `BaseDependency` are already camelCase DTOs with no `deny_unknown_fields`.
- **057** makes `head_sha` a commit on the remote for a connected run. The refusal in this
  task is what a connected runner would hit if that postcondition were ever skipped. 057's
  comparison with `origin` reads `SuccessfulHead::head_sha`.
- **059** runs `prepare` on a connected desktop, whose best-effort fetch is what brings a
  teammate's commit into the local clone. **059 (or 049) also owns one read this task
  leaves behind.** `worktree::status` and `worktree::diff_summary` are D32 local commands
  (`get_worktree_status`, `get_diff_summary`), and after this task they still read
  `dependencies_of` and `latest_successful_head` through a board `ServiceContext`, for
  their fresh-resolution fallback and their warning. That works in solo, where the board is
  local. A connected desktop has no local board, so connected mode must replace that read
  with a board call or with the recorded run's base.
- **061** feeds this task's choice of base as a function over rows. `choose` is that
  function: it takes dependencies paired with their `SuccessfulHead` and runs no git.

**Size.** S. Roughly 1,000–1,400 lines, most of them tests: the rewritten chaining tests in
`tests/worktree.rs`, the new retry, refusal and claim-versus-reread cases, `base_ref.rs`'s
unit tests, and a one-line change at each of the test callers of `prepare`. The production
change is one query, two DTOs, one field on `RunContext`, `prepare`'s board reads replaced
by its context argument, and one refusal message. If the diff grows much past 1,600 lines,
look for something that belongs elsewhere: an interface change (out of scope), a
connected-mode fetch (059), or a `status`/`diff_summary` rework (out of scope).
