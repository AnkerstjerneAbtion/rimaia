---
id: "033"
title: Record the commit a run ended on, and a review bundle
milestone: v0.4
status: ready
depends_on: ["015", "030"]
adrs: ["0013", "0022", "0033", "0036"]
size: M
---

# Record the commit a run ended on, and a review bundle

## Goal

When a run ends, record what it left behind as data rather than as a place to go and look:

- the commit its worktree ended on (`runs.head_sha`);
- the commit its branch forked from (`runs.base_sha`);
- a **review bundle** for any run whose branch carries commits: the per-file diff stat,
  the commits, and the patch up to a stated size cap.

`get_run` then returns the recorded bundle and runs no git at all, for any row. It is a
board read (D32's appendix), and the board never runs git (D31 point 6, ADR-0033 point 7).
A row that recorded nothing reads as `NotRecorded`: a run from before this task, a run
still in flight, a run a crash left open, or a run whose capture failed. For those rows
`RunDetailOverlay`, a desktop view, asks the existing local command `get_diff_summary` for
the branch's current state and labels it as such. The fallback lives in the one view that
has a machine behind it, not in the board command every client reads.

After this task, deleting a worktree, its branch or even the repository's clone no longer
removes the ability to review a run that already finished (ADR-0033's consequences).

## Why now

Four later tasks read what this one writes, and each would otherwise make up its own
version:

- **017 and 034** render the morning review. On `main` that review is
  `worktree::diff_summary`, which runs git against the repository every time the view
  opens. It shows the branch as it is *now*, not as the run left it, and it errors once the
  clone has moved.
- **036** puts `StartRun::base_sha`, `FinishRun::head_sha` and `FinishRun::bundle` on the
  board port (D31 point 6). The port is meant to move behaviour without changing it, and
  it can only do that if the behaviour already exists.
- **044** branches a dependent from its dependency's latest succeeded `head_sha` (ADR-0033
  point 5, D29 point 5). Rows written before this task have none, so the sooner the
  column is written, the fewer dependencies fall back to the default branch.

The migration order is also fixed: `20261001120000` is the first of team mode's files
(D4's amendment in D28). ADR-0022's argument applies too: history cannot be backfilled. A
run that ends before this lands will never have a bundle. All it can ever show is its
branch's current state, and only while that branch exists.

## Scope

**The migration**, `src-tauri/migrations/20261001120000_run_head_and_review_bundles.sql`.
Its statements are exactly D28 part 6's: `runs.head_sha`, `runs.base_sha` and the
`review_bundles` table, column for column. The task writes the file's header comment, in
the voice of the existing migrations. `runs.base_ref` stays as it is: it keeps the branch
*name*, and `base_sha` is what that name resolved to.

D28's DDL overrides this task's original brief in one respect. The brief put base, head
and PR URL in the bundle. They live on `runs` (`head_sha`, `base_sha`, and the existing
`pr_url`), and `review_bundles` does not repeat them. The file is frozen once this task
lands on the branch (D4's amendment), and the D28 rebuild in 038 copies both columns and
redeclares the table exactly as written here.

**The model.** `Run` (`db/models.rs`) gains `head_sha: Option<String>` and
`base_sha: Option<String>`. Every `SELECT *` reader picks them up through `FromRow`. The
two explicit `query_as!` column lists add them: `fetch_run_row` in `runner/outcome.rs` and
`fetch_last_run` in `tasks/service.rs`. `src/types.ts`'s `Run` mirrors both as
`string | null`. NULL means "not recorded" (D18), and neither column is ever backfilled.

**`base_sha`, written at the open.** `worktree::prepare` returns the fork point on
`Worktree` as `base_sha: Option<String>`, computed as `git merge-base <base_ref> HEAD` in
the worktree:

- On a fresh worktree this equals the base's tip.
- On a resumed worktree it is the fork point against the base *this attempt* resolved,
  not wherever that base has moved overnight. `prepare` re-resolves `base_ref` on every
  attempt, and `start_run` already records that fresh name in `base_ref`, so `base_sha`
  is what the row's own `base_ref` resolved to. That is the value ADR-0033 point 5 means
  by "exactly what the run started from". When the resolved *name* changed between
  attempts, the fork point is against the new name (see "A known edge, accepted").

`NewRun` gains `base_sha`, and `start_run` binds it next to `base_ref`, which is written at
the open for the same reason. A fork point that cannot be computed is `None` with a
`tracing::warn!`. It never fails the start.

**The bundle types**, in a new `crates/core/src/runs/bundle.rs`, because this is the board
record:

- `ReviewBundle`: `diff: DiffStat`, `files: Vec<BundleFile>`,
  `commits: Vec<CommitSummary>`, `patch: String`, `patch_bytes: i64` and
  `patch_truncated: bool`. This is the value a runner produces and D31's
  `FinishRun::bundle` carries. `patch_bytes` is `i64` like every count in `DiffStat`:
  SQLite's `INTEGER` is signed, and a `u64` would need a checked cast at both ends.
- `BundleFile`: `FileDiffStat`'s three fields plus `patch: PatchInclusion`, where
  `PatchInclusion` is `Included | TooLarge | NotUtf8 | Binary`, serialized in
  `snake_case`.
- `StoredBundle`: what a read returns. It is its own struct, not a flattened
  `ReviewBundle`, because one field changes type. Its fields are `diff`, `files`,
  `commits`, `patch: Option<String>` (D28 lets the column be NULL once pruned),
  `patch_bytes`, `patch_truncated`, `patch_pruned_at: Option<DateTime<Utc>>` and
  `created_at: DateTime<Utc>`.

`DiffStat`, `FileDiffStat` and `CommitSummary` (`worktree/mod.rs`) derive only `Serialize`
today. They gain `Deserialize`: reading `files` and `commits` back needs it, and so does
052 accepting `FinishRun::bundle` off the wire. Their `rename_all = "camelCase"` stays, so
the stored JSON's keys are the wire's (`shortSha`, `committedAt`), and `BundleFile` uses
the same casing. One casing for both is deliberate. A second set of serde attributes for
storage would be two formats to keep in step, for no reader that wants them to differ.

`files` and `commits` are stored as JSON produced by these serde types, and only through
them (D28). Their field names are therefore a storage format, on disk as on the wire. The
module header says that renaming one, in any of the four structs, is a data migration, and
each of the three `worktree/mod.rs` types gains a line pointing there.

**Computing the bundle**, in a new `crates/core/src/worktree/bundle.rs`, because this is
git. It is two functions, so that a bundle that cannot be built never takes `head_sha`
with it:

- `build(worktree_path: &Path, base_sha: &str, head_sha: &str)
  -> Result<Option<ReviewBundle>>` does the git work and returns its failures, so the
  tests can see them.
- `capture(worktree_path: &Path, base_sha: Option<&str>) -> RunCapture` never fails.
  `RunCapture { head_sha: Option<String>, bundle: Option<ReviewBundle> }` mirrors D31's
  `FinishRun` fields so 036 can move them without reshaping them.

The steps:

1. `head_sha` is `git rev-parse HEAD` in the worktree. It is recorded whenever it
   resolves, including when nothing was committed. D29 point 5 says a succeeded review's
   `head_sha` is "the commit it cleared", and a review commits nothing, so "only if this
   run committed" would leave every review row empty. If it does not resolve, `capture`
   logs a `tracing::warn!` and returns `RunCapture::default()`.
2. A bundle is produced **iff** `base_sha` is known and `base_sha..head_sha` has at least
   one commit. That is how this task reads ADR-0033 point 4's "a run that ends with
   commits": the *branch* ends with commits, whichever attempt authored them. A retry
   that added nothing after its predecessor committed still gets a bundle, which is what
   D29 point 5's "017 renders the newest row's bundle" needs. If `build` returns an
   error, `capture` logs it and returns `RunCapture { head_sha, bundle: None }`. The
   `head_sha` is kept because 044's chaining and D29 point 5 need it whether or not a
   bundle could be built.
3. **One argument vector for every diff.** The numstat and the patch are two invocations
   over the same range, `base_sha...head_sha`, with the same options, defined once as a
   constant in `worktree/git.rs`:

   ```text
   --no-color --no-ext-diff --no-textconv --submodule=short
   --src-prefix=a/ --dst-prefix=b/ --no-relative -M
   ```

   Each flag overrides an operator config key that would otherwise change the stored
   bytes or break the pairing in step 4:
   - `color.ui = always` would put ANSI escapes into the patch.
   - `diff.external` would replace the patch with whatever the tool prints, often
     nothing.
   - A textconv driver would store a converted rendering instead of the file's content.
   - `diff.submodule = diff` expands one submodule change into several `diff --git`
     sections against one numstat row.
   - `diff.noprefix` and `diff.mnemonicPrefix` change the `a/` and `b/` prefixes, and
     `git apply`'s default `-p1` then no longer applies the patch.
   - `diff.relative` narrows the paths to a subdirectory.
   - `diff.renames` decides whether a rename is one section or two. `-M` pins git's own
     default, so the pairing does not depend on the operator's setting.

   The existing `worktree::git::diff` behind `diff_summary` adopts the same constant, so
   the live path and the recorded one cannot disagree about what a range contains. The
   commits come from the existing `commits` and `parse_log` over `base_sha..head_sha`.
   Anchoring on the two recorded shas instead of branch names is what makes the bundle
   describe this run.
4. **The cap, and how it is applied.** The rules:
   - `PATCH_CAP_BYTES = 512 * 1024` (512 KiB), defined once in `runs/bundle.rs`.
   - The patch is split into per-file sections at lines starting with `diff --git `.
     Inside a patch such a line can only be a header, because every content line starts
     with a space, `+`, `-`, `\` or `@@`.
   - `files` pairs each numstat entry with its patch section by position: one diff
     queue, one argument vector, the same order. If the two counts disagree, `build`
     returns an error. It never returns a mispaired list.
   - Each section gets exactly one `PatchInclusion`, decided in this order:
     1. `Binary` when its numstat row is `-` `-`. git's section for it is a single
        "Binary files … differ" line, which `git apply` rejects, so it is never
        included. The file is still listed.
     2. `TooLarge` when the section does not fit in the remaining budget. This is
        decided as soon as the section outgrows the budget, with no UTF-8 check, because
        its bytes are no longer buffered (below). The next section is still tried.
     3. `NotUtf8` when the section was buffered whole and is not valid UTF-8.
     4. `Included` otherwise. Included sections are appended whole, in git's order.
   - The stored patch is therefore always a concatenation of whole, byte-exact sections
     of text files. It is never cut mid-hunk or mid-character, it holds no binary
     section, and it applies onto `base_sha` as a whole.
   - `patch_bytes` is the size of the whole diff before the cap, every section counted.
     `patch_truncated` is true iff any section is `TooLarge`: it says the cap cut
     something. A `Binary` or `NotUtf8` section would be absent at any cap, so it is
     marked on its file and does not set the flag.
   - git's output is read as a stream. A section that has outgrown the remaining budget
     is counted and no longer buffered, so a vendored 500 MB diff costs the cap plus one
     read buffer in memory, not 500 MB.

**Recording at finish.** `finish_run` gains `capture: &RunCapture`:

- **The write.** In the transaction that already closes the row, it writes `head_sha`
  and inserts the `review_bundles` row with `created_at = ctx.clock.now()`. The insert is
  a `query!` and lands in `.sqlx`. `runner::outcome` stays the only writer of `runs`, and
  it becomes the only writer of `review_bundles` (ADR-0006).
- **A bundle with no `head_sha`.** `capture` never produces one, but 036 builds a
  `RunCapture` from a runner's message. It is dropped with a warning. It is never stored,
  and it never fails the finish.
- **No new change event.** The existing `runs` event already covers the write (D2).
- **Where the capture happens.** `run_task` in `runner/process.rs` calls `capture` after
  `execute` returns and before either `finish_run` call. That covers the `Ok` branch and
  the `Err` (runner-fatal) branch, and every outcome, failed and cancelled included,
  because a failed run's partial work is exactly what a morning review opens to decide
  what to do next.
- **Why the worktree is still there.** The child's process group is dead by then, so
  nothing is still committing. The task is still `running`, and D20's first guard, the
  one with no override, refuses every removal path while it is. That is what "before the
  worktree can be pruned" means here, and the guard is not new.
- **A capture that fails.** For example, the worktree was deleted underneath the run, or
  the bundle could not be built. `capture` has already logged it and returned what it
  could: nothing, or `head_sha` alone. The run's outcome, class and task transition are
  exactly what they would have been. A bundle is a record, not a postcondition. ADR-0033
  point 4's postcondition is task 057's and is connected-only.
- **Other callers.** `scheduler/reconcile.rs` passes `RunCapture::default()`. An
  interrupted row records nothing, and the attempt that resumes it records its own.

**Reading it back.** `runs::get_run` returns
`RunDetail { run, review: RunReview, log_available }`, and it runs no git for any row:

```rust
#[serde(tag = "source", rename_all = "snake_case")]
pub enum RunReview {
    /// Written at this run's finish. `bundle: None` means the recorded shas say the
    /// branch carried nothing: `head_sha` equals `base_sha`.
    Recorded { bundle: Option<StoredBundle> },
    /// Nothing on the row says what the branch carried: the run predates task 033, is
    /// still in flight, was closed by a crash, or its capture failed.
    NotRecorded,
}
```

- **Which variant.** It is decided from the recorded shas and the bundle row together,
  never from a missing bundle row alone. D18 makes NULL "not recorded", never a claim:
  - a `review_bundles` row exists: `Recorded` with that bundle;
  - no row, and `head_sha` and `base_sha` are both set and equal: `Recorded` with
    `bundle: None`, the one case where "no commits on its branch" was measured;
  - anything else, including either sha NULL, or both set and different: `NotRecorded`.
    Both set and different with no row is a bundle that could not be built, or a head
    that is an ancestor of `base_sha` (a branch reset backwards). Nothing on the row
    tells the two apart, and `NotRecorded` is the reading that is never false.
- **What it costs.** One indexed `SELECT` by `run_id` on top of today's row read. It
  needs neither the worktree, the branch nor the clone. A row whose repository has moved
  therefore no longer fails `get_run` as a whole, as it does on `main`, and takes the
  outcome, prompt and transcript down with it.
- **How it is read.** Hand-built SQL decoded through `FromRow` into a private row type,
  per `runs/mod.rs`'s header. `files` and `commits` are `TEXT` parsed with `serde_json`,
  because the workspace `sqlx` has no `json` feature (`tasks/strategy.rs` records the same
  choice). A row whose JSON does not parse is an `internal` error (D8), not an empty list:
  only this module's serde types write it, so a parse failure is a bug. Only the insert in
  `outcome.rs` is a `query!`.
- **`list_runs` and `list_runs_for_task`.** They never join `review_bundles`, so the
  patch never rides a list (D28's reason for a separate table). `RunListEntry` and `Run`
  carry no bundle field.
- **The module header.** `runs/mod.rs`'s header argues that the detail view shows "the
  branch's current state, not a snapshot frozen at that attempt's end". This task
  reverses that decision on purpose, under ADR-0033 point 7, and rewrites the header to
  say so, and to say that the stored JSON uses the wire's camelCase keys. What is left of
  the old behaviour is the overlay's fallback below, and none of it is in this module.
  `get_run_row`'s comment stops citing `get_run`'s git calls, since there are none. The
  function stays, because it still skips the bundle read and the transcript check.

**The interface.** `src/types.ts` mirrors `RunReview`, `StoredBundle`, `BundleFile` and
`PatchInclusion`, and its `Run` gains `headSha` and `baseSha` as `string | null`. Every
`Run` and `RunDetail` fixture in `src/**/*.test.tsx` and in `src/dev/fixtures/` follows
the new shape, or `npm run typecheck` fails. That is at least the `RunOutcomeSection`,
`RunInfoSection`, `RunHistorySection`, `TaskDetailPanel`, `ActiveRunCard`, `RunsView`,
`StorageSection` and `RunDetailOverlay` tests, and 028's typed seed.

`RunDetailOverlay.tsx` keeps ADR-0013's order (outcome, diff, commits, PR, prompt,
transcript) and renders from `review`:

- **Recorded, with a bundle.** Totals, the per-file list, and the commits. Each file the
  patch left out carries a quiet "not in patch" marker that says why: too large, binary,
  or not UTF-8 text. The patch sits in a collapsed `<details>` as plain preformatted
  text.
- **Truncated.** One line saying how many of the files are in the patch and how large
  the whole diff was, pointing at `prUrl` when there is one. ADR-0033 point 7 says "the
  full diff beyond the cap is on the forge, one link away".
- **Pruned** (`patch_pruned_at` set). The file list and commits, plus a line saying the
  patch was pruned and when. Nothing in solo mode writes this state yet (see Out of
  scope), but the reader handles it now so 056 does not have to touch this view.
- **Recorded, empty.** "This run ended with no commits on its branch."
- **Not recorded.** Only for this variant, the overlay calls the existing local wrapper
  `getDiffSummary(taskId)` (`src/lib/commands.ts`, command `get_diff_summary`, which no
  view calls today). It renders that summary's totals, files and commits as the overlay
  does on `main`, under a line saying this is the branch's current state, not what this
  run left. If the call fails, because the branch or the clone is gone, the section is one
  line saying no diff was recorded for this run and the branch cannot be read. The rest
  of the overlay (outcome, prompt, transcript) renders as usual. A failed fallback is
  never an overlay error.

Copy follows ADR-0024's calm register. Syntax highlighting, side-by-side views and per-file
navigation belong to 017.

**The offline cache.** Regenerate `.sqlx/` with D5's recipe, unchanged (D33: tasks before
040 do).

## Out of scope

- **Uploading the bundle, and checking a bundle from a runner nobody trusts.** Both are
  052's (the HTTP adapter) and 056's. The only thing that produces a bundle here is
  in-process code.
- **Writing `patch_pruned_at`.** ADR-0036 point 6 prunes the patch with the transcript on
  the server's retention schedule, and that is 056's. Solo `prune_logs` is unchanged: it
  deletes transcript files, leaves every bundle whole, and a test pins that. A capped
  patch is a row-sized record, and ADR-0022 point 2 keeps records.
- **Branching from `head_sha`, and `runs::latest_successful_head`.** Both are 044's.
- **Run kinds, and copying `base_ref`/`base_sha` onto review and fix rows.** 035 (D29
  points 1 and 4) and 021.
- **Pushing, and the connected-mode postcondition on `head_sha`.** 057.
- **Review actions, the digest, and the morning review screen.** 034 and 017.
- **Capturing at startup reconcile.** A crash-closed row stays `NotRecorded`. Capturing
  there would put git on the board side of the port that 036 draws, for a row whose
  resumed attempt records a bundle anyway.
- **Uncommitted work.** The bundle describes commits. A dirty worktree is what the
  worktree panel's live `dirty` flag is for.
- **Backfilling bundles for old runs.** The branch has moved since those runs ended, so
  any bundle computed now would describe the present and be labelled as the past (D18
  point 2's rule).
- **New commands, and any MCP tool.** There is no MCP run-detail tool today. This task
  adds no command and changes no command's name: the overlay's fallback uses
  `getDiffSummary`, which `commands.ts` already exports and both `generate_handler!`
  lists already register. `commands.ts`, the two lists and `check-command-wiring.sh` do
  not change. D32's appendix already records `get_run` as the board command that carries
  the bundle, and `get_diff_summary` and `get_worktree_status` as local ones; this task
  keeps both classifications true.

## Acceptance criteria

- `src-tauri/migrations/20261001120000_run_head_and_review_bundles.sql` exists and its
  statements are D28 part 6's, column for column. No other migration is added, and
  `.sqlx/` is regenerated and committed.
- `Run` carries `head_sha` and `base_sha` through every reader. Both explicit
  `query_as!` lists name them, and `src/types.ts` mirrors them.
- **Capture, against real repositories in a `TempDir`** (never a mocked git):
  - `a_bundle_records_the_files_commits_and_patch_between_the_fork_point_and_head`: the
    stored `patch` is byte-equal to `git diff` with the pinned argument vector over
    `base_sha...head_sha` in the same repository, and `files` and `commits` equal what the
    live `diff_summary` reports for the same range.
  - `a_patch_over_the_cap_keeps_whole_files_in_order_and_omits_the_rest`: the fixture is
    text files only. The stored patch is at most `PATCH_CAP_BYTES`, every included
    section is whole, and `git apply --check` of it onto `base_sha` succeeds.
  - `a_patch_with_a_binary_file_still_applies_as_a_whole`: the same, with a binary file
    among the text ones.
  - `a_file_larger_than_the_whole_cap_is_omitted_and_later_files_still_fit`.
  - `patch_bytes_counts_the_whole_diff_not_the_stored_part`.
  - `a_file_that_is_not_utf8_is_listed_but_left_out_of_the_patch`: marked `NotUtf8`, and
    `patch_truncated` stays false.
  - `a_binary_file_is_listed_with_no_line_counts`: marked `Binary`, left out of the patch,
    and `patch_truncated` stays false.
  - `the_patch_ignores_the_operators_diff_config`: the test sets `color.ui=always`,
    `diff.external`, `diff.noprefix=true` and `diff.mnemonicPrefix=true` in the
    repository's config. The stored patch contains no `\x1b` byte, its headers read
    `a/` and `b/`, and it is byte-equal to the patch captured without them.
  - `a_submodule_change_is_one_file_and_one_section_under_diff_submodule_diff`: with
    `diff.submodule=diff` set, a submodule bump pairs with exactly one section and
    `build` does not error.
  - `a_branch_with_no_commits_ahead_records_head_but_no_bundle`.
  - `a_bundle_that_cannot_be_built_keeps_head_sha`: `capture` with a `base_sha` that
    names no object in the repository, so `git diff` fails, returns `head_sha` equal to
    the worktree's `HEAD` and `bundle: None`, and `build` on the same input returns the
    error.
  - `the_fork_point_of_a_resumed_worktree_is_where_it_branched_not_where_the_base_is_now`:
    the base branch advances between attempts, and `base_sha` is the original fork point.
  - `a_rename_and_a_path_with_a_space_pair_with_their_own_patch_sections`.
- **End to end through `run_task`**, with `FakeCli` replaying fixture streams and
  `TestClock`, and no `sleep`:
  - `a_run_that_commits_records_its_head_and_a_bundle_at_finish`: `head_sha` equals the
    worktree's `HEAD`, `base_sha` equals the fork point, and `created_at` equals the
    faked clock's now.
  - `a_failed_run_that_committed_still_records_its_bundle`.
  - `each_attempt_records_the_branch_as_that_attempt_left_it`: attempt 1 commits once and
    attempt 2 commits again. `get_run` on attempt 1 still shows one commit after attempt
    2 has finished.
  - `a_run_whose_worktree_cannot_be_read_at_finish_keeps_its_outcome_and_records_nothing`:
    `FakeCli::gates` holds the attempt, the test deletes the worktree directory, then
    `open_gate` lets it finish. The exit class, the task's `run_state` and its column are
    what they would have been with a readable worktree, and `head_sha` is NULL.
- **Reading back.** `get_run` runs no git in any of these:
  - `a_run_stays_reviewable_after_its_worktree_and_branch_are_deleted`: the worktree is
    removed through `cleanup::remove_worktree` with a full `RemovalAuthorization`
    (`uncommitted_changes` and `unpushed_commits` both `confirmed_by_user`, and
    `branch: DeleteEvenIfUnmerged`, since the test repository has no remote and D20's
    unpushed-commits guard would otherwise refuse). The repository's directory is then
    deleted, and `get_run` still returns the identical `Recorded` review. `main`'s
    `get_run` errors at that point, which proves no git ran.
  - `a_run_recorded_before_bundles_reads_as_not_recorded`: a row with `head_sha` NULL
    whose repository directory has been deleted. `get_run` succeeds and returns
    `NotRecorded` with the row's outcome and `log_available` intact.
  - `an_in_flight_run_reads_as_not_recorded`.
  - `a_run_that_ended_with_nothing_to_review_reads_as_recorded_and_empty`: `head_sha`
    equals `base_sha`, and the review is `Recorded` with `bundle: None`.
  - `a_run_whose_fork_point_is_unknown_is_not_reported_as_empty`: a row started with
    `base_sha: None` and finished with a `head_sha` and no bundle reads as `NotRecorded`,
    not as `Recorded` with `bundle: None`.
  - `a_bundle_that_cannot_be_built_keeps_head_sha_and_reads_as_not_recorded`: a row
    finished with `RunCapture { head_sha: Some, bundle: None }` and a `base_sha` that
    differs keeps its `head_sha` and reads as `NotRecorded`.
  - `pruning_transcripts_keeps_every_bundle`.
  - `deleting_a_task_deletes_its_runs_bundles`.
  - `listing_runs_never_reads_review_bundles`: after `DROP TABLE review_bundles` in the
    test pool, `list_runs` and `list_runs_for_task` still succeed. `RunListEntry` and
    `Run` carry no bundle field.
- **Vitest** (`RunDetailOverlay.test.tsx`, mocking `@tauri-apps/api/core` as every other
  test does) renders each state in Scope: recorded; truncated with and without a PR URL;
  pruned; recorded-empty; not recorded with `get_diff_summary` answering, labelled as the
  branch's current state; and not recorded with `get_diff_summary` rejecting, where the
  diff section is one line and the outcome, prompt and transcript still render. The
  patch is collapsed by default. Each file left out of the patch carries the marker for
  its reason. `get_diff_summary` is never invoked for a `recorded` review.
- Every `Run` and `RunDetail` fixture in `src/**/*.test.tsx` and in `src/dev/fixtures/`
  has the new shape, and `npm run typecheck` proves it.
- `runs/mod.rs`'s module header no longer claims the detail view shows the branch's
  current state and says the stored JSON uses the wire's camelCase keys. `runs/bundle.rs`'s
  header states the cap, its reasons, and that the stored JSON's field names are a
  storage format.
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.
- `npm run screenshot` (task 028) captures the run detail overlay. `busy` gains a
  `run-detail` view in `screenshots/views.shot.ts`, opened by clicking a finished run in
  the Runs view, and the `busy` seed's `get_run` row answers with a recorded, truncated
  bundle in place of 028's old-shape `RunDetail`. 028's view table and capture count
  follow (12 per project, 48 per run). The task's report and its commit state that the
  images were inspected.

## Notes

**Read first:**

- **D28** parts 6 and 7, and its D4 amendment: the DDL and the frozen-file rule.
- **D4**: migration file names, and that a file is frozen once it has run.
- **D29** points 4 to 6: which rows record `base_sha`, why `head_sha` is the same for
  every kind, and why 017 reads the newest row's bundle.
- **D31** point 2 (`StartRun`, `FinishRun`) and point 6 ("033 lands before this port … the
  runner computes the bundle … the board never runs git").
- **D32's appendix**: `get_run` is a board Read command that carries the bundle, and
  `get_diff_summary` and `get_worktree_status` are local. This task keeps both true.
- **D18**: NULL is "not recorded".
- **D2**: how a change event crosses into the shell, and why the `runs` event already
  covers this task's write.
- **D20**: the `running` guard this task relies on and does not add.
- **D5 and D33**: regenerate the cache.
- **D8**: no new error code, since a capture failure is logged, not returned.
- **D10**: ids are strings.
- **D34**: axum's default 2 MB body limit on every route but `append_transcript`, which
  "Why 512 KiB" below argues from.

The ADRs are 0033 points 4, 5 and 7, 0036 points 3 and 6, 0013 (the diff-first order), and
0022 part 2.

**Why 512 KiB.**

- **It holds a whole task's diff.** This backlog sizes a task for one agent session at
  about 3–4k lines of diff. At git's typical 60–80 bytes per patch line, that is roughly
  250–300 KB. A patch for a task-sized change therefore fits whole, with about twice that
  as headroom. Past about 7,000 lines, nobody reads a patch line by line over coffee, and
  the forge holds the rest.
- **The transport limit.** D34 leaves axum's default 2 MB body limit on every route
  except `append_transcript`, and that includes 052's `finish_run`. A 512 KiB patch
  roughly doubles in the worst realistic JSON escaping, so the patch alone never forces
  a special limit, which a 1 MiB cap could not promise. The claim stops there. Only the
  patch is capped: `files` costs about 100 bytes a path and `commits` about 250 bytes a
  commit, so a run that regenerates ~10,000 files, or a branch that merged ~4,000
  commits of base history, produces a `FinishRun` over 2 MB. This task leaves the lists
  unbounded because D28's DDL has no place to say a list was cut: `files_changed` is
  exact, so a shortened `files` would be detectable, but nothing counts the commits. 052
  owns the answer (see "After this task").
- **Storage.** Litestream replicates every board write (ADR-0037). Under 021's loop,
  every review and fix row whose branch carries commits stores its own bundle, so a task
  that loops three times stores four patches. The cap bounds a night at runs × 512 KiB.

Lowering the cap later needs no migration. Raising it does not either, but it is bounded
by the body limit.

**Why whole files rather than a byte prefix.** git orders a diff by path, so
`package-lock.json` comes before `src/`. With a prefix cut, one regenerated lockfile would
use up the whole budget and the reviewer would see none of the actual change. Skipping
what does not fit, and still trying what follows, keeps the source files. The per-file
marker then says what is missing, not just that something is. It also keeps the stored
patch applicable, which a test can check with `git apply --check` instead of by eye.

**Known edges, accepted.**

- **A run that rebased its own branch** onto a newer base is still measured from the
  `base_sha` it started on, so its bundle includes the base's intervening commits. That is
  a true statement about what the branch contains relative to where the run began.
  Unattended runs rarely rebase, and nothing here tries to detect it.
- **A base whose name changed between attempts.** `prepare` re-resolves `base_ref` on
  every attempt, and a resumed worktree keeps its branch. If a dependency changed in
  between, so the base flipped from dependency A's branch to `main`, then
  `git merge-base main HEAD` is where A forked from `main`, and the bundle includes A's
  commits. It is the fork point against the base this attempt resolved, which is what the
  row's `base_ref` says, so the row stays self-consistent. 044 replaces the revision a
  worktree branches from, and with it this case.

**Files to start from** (all on `main` @ 728a049):

- `crates/core/src/runner/outcome.rs`: `NewRun`, `start_run`, `finish_run` and
  `fetch_run_row`.
- `crates/core/src/runner/process.rs`: `run_task`'s two `finish_run` calls, and
  `start_run`'s `base_ref`.
- `crates/core/src/worktree/mod.rs`: `Worktree`, `prepare`, `diff_summary` and
  `recorded_base_ref`.
- `crates/core/src/worktree/git.rs`: `diff`, `commits`, `parse_numstat`, `parse_log`,
  `checked`, and `run`, which has no streaming variant yet.
- `crates/core/src/runs/mod.rs`: `RunDetail`, `get_run`, `get_run_row`, `fetch_run`,
  `prune_logs`, and the module header.
- `crates/core/src/tasks/strategy.rs`'s header: the precedent for JSON in a `TEXT` column
  without sqlx's `json` feature.
- `crates/core/src/worktree/cleanup.rs`: `remove_worktree` and `RemovalAuthorization`,
  for the reviewable-after-deletion test.
- `crates/core/src/db/models.rs` (`Run`) and `crates/core/src/tasks/service.rs`
  (`fetch_last_run`).
- `crates/core/src/scheduler/reconcile.rs`: `reconcile_one`'s `finish_run`.
- `crates/core/src/testing/cli.rs`: `FakeCli::commits_on_attempt` makes empty commits.
  Add a verb that writes a file before committing, rather than a second stand-in.
- `crates/core/src/testing/repo.rs` (`TempRepo`).
- `crates/core/tests/{worktree,runner_process,runner_outcome,analytics}.rs`.
  `runner_outcome.rs` and `analytics.rs` call `finish_run` directly and gain
  `&RunCapture::default()`.
- `src/types.ts`, `src/components/runs/RunDetailOverlay.tsx` and its test, every other
  test that builds a `Run` or `RunDetail` (see The interface), and `getDiffSummary` in
  `src/lib/commands.ts`.
- `screenshots/views.shot.ts` and the seed in `src/dev/fixtures/` (task 028).

**Chain.** 028 comes before this task and provides `npm run screenshot` and the fixture
mode at the transport seam, where every command already has a row, `get_run` and
`get_diff_summary` included. This task replaces the `get_run` row's answer with a recorded,
truncated bundle and adds the `run-detail` view.

After this task:

- **034 (review actions, digest)** reads `head_sha` and the bundle through `get_run`, and
  adds any MCP exposure the digest needs.
- **017** renders the newest row's `StoredBundle` (D29 point 5) and owns the real diff
  presentation. It maps `not_recorded` to its Absent line ("no diff was recorded for this
  run") with no special case, because `get_run` carries no live diff for it to
  suppress. The live fallback is `RunDetailOverlay`'s own `getDiffSummary` call, so 017's
  review view gets it only by calling that local command, which it must not.
- **021 and 037** can tighten their wording: a finished row has a bundle only when
  `base_sha..head_sha` is non-empty, not "for every finished row of every kind". A review
  row that 035 gives its implementation row's `base_sha` has one whenever that branch
  carries commits.
- **035** copies `base_ref` and `base_sha` onto review and fix rows.
- **036** moves `capture` to the runner side of the port, and `RunCapture` becomes
  `FinishRun`'s two fields with no reshaping.
- **038** copies both columns and keeps `review_bundles`' foreign key intact through the
  rebuild.
- **044** reads `head_sha` through `latest_successful_head`.
- **052** validates an uploaded bundle against `PATCH_CAP_BYTES`, and decides what
  bounds `files` and `commits`: a larger body limit on `finish_run`, or a cap on each list
  at the runner with a way to say it was cut. This task leaves both lists unbounded (see
  "Why 512 KiB").
- **056** writes `patch_pruned_at`.

**Size.** This is an estimate of 1,500–2,200 lines, about half of it tests, which is
inside one session. If it runs over, cut the overlay's patch `<details>` and the truncation
line to 017 and keep everything else. The DTO, the capture, the storage and every Rust test
are the contract later tasks build on. The patch viewer is only its first reader.
