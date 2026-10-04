---
id: "017"
title: Morning review flow
milestone: v0.4
status: ready
depends_on: ["033", "034"]
adrs: ["0007", "0008", "0013", "0024", "0033"]
size: M
---

# Morning review flow

## Goal

Turn "open the app and figure out what happened last night" into a single screen and a
short sequence of decisions.

**This task is the interface, and only the interface.** The decisions it offers — approve,
reject, needs changes — are task 034's service functions, reachable from every door. The
diff and commits it shows are task 033's stored review bundle. What is left for 017 is the
part the product is named for: a digest that says what the night did, and a review queue
walked with the keyboard alone.

## Why now

The workflow the product is named for. Once several nights of runs exist, the board alone
under-serves the review.

It is also the first screen that must work when the reviewer is not on the machine that ran
the work. ADR-0033 point 7 says so directly: "task 017's morning review render[s] from this
bundle on every client." Written against the live worktree, as the original version of this
task would have been, it would be rewritten by task 049. Written against the bundle, it
travels to the browser unchanged, and the only desktop-only thing on it is "open worktree".

It lands before the review loop (035, 021, 037) on purpose. More runners produce more PRs,
so review becomes the bottleneck sooner, and 037 adds loop history and findings *to this
view* rather than building a second one.

## Scope

Field names below are the Rust names 033 and 034 give. `src/types.ts` mirrors them in
camelCase, as it does every DTO (`patch_bytes` is `patchBytes` in a component).

The review view has two modes, the **digest** and the **queue**. It opens on the digest.

**Overnight digest**

- Opening the app after a queue has run shows a digest: what completed, failed, was blocked
  or skipped; the night's run time and cost; the tasks needing attention first.
- Failures and blocked chains lead, because they are what changes the day's plan.
- **The digest is task 034's, rendered as it arrives.** One entry per task (D29 point 8),
  in the order `get_review_digest` returns. 017 does not re-sort, re-bucket or choose the
  window the night covers: what counts as "last night" and what order the entries lead in
  are rules, and rules live in `rimaia-core` (ADR-0006) so the MCP tool and this screen
  cannot disagree. If the order is wrong, fix it in 034's service and its test, not in a
  component.
- Each entry shows the task title, its outcome as a dot and a word (ADR-0024 point 3), its
  duration (`run_seconds`) and its cost (`cost_usd`). The eight `DigestOutcome` values have
  one word each: Failed, Blocked, Waiting for retry, Interrupted, Cancelled, Running,
  Completed, Skipped. A Blocked entry names its blocker (`blocking_title`). A Skipped entry
  says why, from its `skip_reason`, using `QUEUE_SKIP_LABELS` in
  `src/components/runs/QueuePlanList.tsx` rather than a second table of the same words. A
  `null` cost or duration is shown as "not recorded" and never as `$0.00` (D18).
- **The totals are 034's `DigestTotals`, rendered as returned.** The view does no arithmetic
  over entries. Totals count runs that ended in the window, and entries include tasks with
  no such run (Blocked, Skipped, Running), so a sum over entries is a different number and
  a second copy of a core rule. The view shows `runs`, `run_seconds` as the total run time,
  `cost_usd` as the total cost, and "N runs had no recorded cost" from `runs_without_cost`
  when it is not zero. If 034 kept `span_seconds` (its Size note lets it be cut), it is
  shown as the night's wall-clock span, labelled as such; if the type has no such field, the
  line is not rendered.
- **The digest is live.** It re-reads `get_review_digest` on `runs:changed` (a run ended),
  `tasks:changed` (a Blocked or Skipped entry depends on columns) and `settings:changed`
  (034's marker write publishes `ChangeEvent::Settings`), all through `src/lib/events.ts`
  (D7). An empty digest says in one line that nothing has ended since the last finished
  review.
- **Where the app opens.** A pure function, `openingView`, decides from `get_app_info`'s
  `onboardingDismissed` and the digest:
  1. `welcome` while onboarding is not dismissed (today's rule; it takes precedence).
  2. `review` when the digest's `totals.runs > 0`: at least one run ended since the last
     finished review.
  3. `board` otherwise, **including when `get_review_digest` rejects**, for the reason
     `App.tsx` already falls back to the board when `get_app_info` fails: a failed read is
     not a reason to withhold the app.

  The rule is keyed on `totals.runs` and not on "the digest has entries" on purpose.
  Blocked and Skipped entries come from `ready` tasks whatever the window (034: "a ready
  task with no row in the window can still appear"). A board with one blocked chain, or one
  ready task in a repository without unattended opt-in, would otherwise open on the review
  view at every launch, and the marker could never clear it, because the marker moves only
  when a review empties `in_review`. `totals.runs` is exactly what the marker empties. A
  night that produced only failures still opens on the review view until the next finished
  review. 034's "What the next tasks expect" accepts that, and names the fix (a key calling
  `mark_review_digest_seen`) if real mornings show otherwise.

  No "seen" marker is stored by this view. The digest is one keypress away from the queue
  and one click away from the board, which is cheaper than a column that records whether it
  was looked at.
- A **copyable summary** of the digest as plain text, built by a pure function from the
  same data the screen renders. It replaces the original task's optional item and costs
  one function and one test.

**Review queue**

- Walk `in_review` tasks one at a time, in **board order**: the queue is
  `groupIntoColumns(tasks).in_review` from `src/lib/board.ts`, over the board's own read
  (`useTasks`, `list_tasks`), with archived tasks excluded. That is `compareBoardOrder`'s
  repository, then `position`, then `created_at`, then `id`. It is not "ascending
  `position`": a position is only comparable within one (repository, column) (ADR-0007), so
  with two repositories a global sort by position interleaves them, and the queue would
  disagree with the board it sits beside.
- **What is read for the task on screen, and how.** `TaskSummary` carries no run id
  (`LastRunSummary` is four fields of a `Run`, not the row). The read path is:
  1. `get_task(id)`. Its `lastRun` is the newest row (`fetch_last_run`, a D29 point 4
     reader), and gives the run id, its `status`, `exit_class` and `pr_url`. Its `dependsOn`
     lists the task's own dependencies, whose titles and columns come from the board read.
  2. `get_run(lastRun.id)`, whose `review` is 033's `RunReview`.
  3. `get_task_dependents(id)`, 034's `dependents_of` with `built_on`.

  All three are re-read when a `tasks:changed` or `runs:changed` payload names the task, or
  carries no ids (D7: an empty payload means "every id changed").
- Per-task actions:
  - **Approve** → 034's `approve_task`. The task moves to `done` and the queue advances.
  - **Reject** → 034's `reject_task`, with the note typed here. The task goes back to
    `ready` and the note is appended to its extra instructions. **Its worktree is removed
    and its work is kept on a set-aside branch**; the next run starts on a fresh branch from
    the base. The note step says this in one line before submission, because it is the whole
    difference between `r` and `c`. After a successful reject the view advances and shows
    one line naming `set_aside_branch` exactly (for example "Rejected *Add login*. Its work
    is on `rimaia/x-2`, and a pull request opened from it stays open."). When
    `set_aside_branch` is `None`, the line says the task had no branch to set aside.
  - **Needs changes** → 034's `request_task_changes`, with the note. The task goes to
    `ready`, keeping its worktree and branch so the next run continues on the reviewed
    commits rather than restarting. The note step says that too.
  - **Open PR** in the browser (below).
  - **Open worktree** in the tool the user works in, through task 026's Open in… menu.
- **Keyboard-driven, and keyboard-complete.** Every action above, plus moving between the
  digest and the queue and between tasks, has a key. A review from the opened digest to the
  empty queue needs no pointer. The keys:

  | Key | Where | Action |
  | --- | --- | --- |
  | `Enter` | digest | Start the review: switch to the queue, on its first task |
  | `d` | queue | Back to the digest |
  | `j` or `→` | queue | Next task |
  | `k` or `←` | queue | Previous task |
  | `a` | queue | Approve |
  | `r` | queue | Reject — opens the note field |
  | `c` | queue | Needs changes — opens the note field |
  | `o` | queue | Open the PR |
  | `w` | queue | Open the worktree — opens the Open in… menu |
  | `Mod+Enter` | note field | Submit the note (Enter is a newline in it) |
  | `Escape` | note field, menu | Cancel the note, or close the menu, without acting |

  The legend is always visible and small, not behind a help key, and shows the keys for the
  current mode. Bare-letter keys, and `Enter` on the digest, are inert while focus is in an
  editable element, the rule `Board.tsx` applies with `isEditableTarget`. `Enter` on the
  digest is also left alone while focus is on a button or link, so the copy button still
  activates natively. Move `isEditableTarget` to `src/lib/keyboard.ts` so both views share
  one definition rather than two that drift. `Board.tsx` imports it from there, and its
  `describe("isEditableTarget")` block moves from `Board.test.tsx` to
  `src/lib/keyboard.test.ts` with the import updated, rather than a re-export kept for the
  test's sake. After every action and every navigation, focus stays on the review surface,
  so the next key works without a click. Next and previous stop at the ends; they do not
  wrap, because wrapping hides that the queue is finished.
- **The note is 034's rule, not this screen's.** 034 refuses a blank note, and the refusal
  is rendered as its message (D8). The UI adds no validation the service does not have.
- **Each task shows the ADR-0013 review order without navigation:** outcome, the diff
  summary (files changed, insertions, deletions, and the per-file list), the commits, the
  PR link, then the patch as plain monospace text, expanded. The outcome is read off the
  newest run: `status`, with "interrupted" taken from its `exit_class` and never from the
  task's `run_state` (D9). The rest comes from that run's `get_run` (D29 point 5: the
  reviewer sees the branch as it is, not the last commit a dependent built on). Each
  `RunReview` variant renders one way:

  | `get_run` returns | The review view renders |
  | --- | --- |
  | `recorded`, bundle with `patch_truncated: false` and no `patch_pruned_at` | Totals, per-file list, commits, PR link, the whole patch |
  | `recorded`, `patch_truncated: true` | As above, but the patch holds only the file sections that fit, each whole. Each file left out carries 033's quiet "not in patch" marker. After the patch, one line: how many of the files are in it, how large the whole diff was (`patch_bytes`), and that the rest is on the PR, with the link. With no PR, the line says the rest is on the branch |
  | `recorded`, `patch_pruned_at` set | Totals, per-file list, commits and PR link as usual, and in place of the patch one line saying it was pruned and when |
  | `recorded`, `bundle: null` | "This run ended with no commits on its branch." No totals, no file list |
  | `not_recorded` | "No diff was recorded for this run." No totals, no file list |
  | no run (`lastRun` is `null`, a card dragged into `in_review` by hand) | "This task has no run to review." |

  Nothing but a recorded bundle ever renders a file count. "0 files changed" would be a
  claim about the branch that nobody made. `not_recorded` carries nothing: 033's `get_run`
  runs no git for any row, so there is no diff on it to show or to hide. The only way to a
  diff for such a row is the local command `get_diff_summary`, which reads the branch as it
  is now, on the machine holding the worktree. Calling it here would make the review view
  desktop-only for exactly those rows, which is what ADR-0033 point 7 set out to avoid, and
  would show the branch's present state as if it were what the run left. That fallback
  stays in `RunDetailOverlay`, labelled as the branch's current state, as 033 left it.
- **One renderer for the bundle, two presentations.** `RunDetailOverlay.tsx` renders the
  same `RunReview` after 033, with the patch in a collapsed `<details>` and, for
  `not_recorded` only, its own `getDiffSummary(taskId)` call. Extract its diff, commits and
  PR sections, that fallback call included, into one component,
  `src/components/runs/RunReviewSections.tsx`, taking two props:
  - `liveDiff: "fallback" | "none"` decides whether a `not_recorded` review calls
    `getDiffSummary`. With `fallback` the component does exactly what 033's overlay does:
    calls it, renders the answer under the "branch's current state" line, and renders the
    one-line failure when it rejects. With `none` it calls nothing and renders "No diff was
    recorded for this run." The prop changes nothing for a `recorded` review, which never
    calls `getDiffSummary` under either value (033's criterion).
  - `patch: "collapsed" | "expanded"`.

  The overlay passes `fallback` and `collapsed`, and its test from 033 passes unedited,
  because it mocks `invoke` and the call is the same call from a different component. The
  review view passes `none` and `expanded`. The two views then cannot show the same bundle
  two ways, and neither contradicts the other's acceptance criteria.
- **No live git from this view.** It never calls `get_diff_summary` or
  `get_worktree_status`. Those are local commands (D32's appendix) and the browser will not
  have them.
- **Open PR.** `o` opens the newest run's `pr_url` through one wrapper,
  `openExternalUrl(url)` in a new `src/lib/open.ts`, which calls
  `@tauri-apps/plugin-opener`'s `openUrl`. No component imports the plugin. The wrapper
  exists because nothing on `main` opens a URL programmatically (`RunDetailOverlay` uses
  `<a target="_blank">`), and because `openUrl` calls `invoke("plugin:opener|open_url", …)`
  directly, outside 028's `CommandTransport`. That makes it a third client-side capability
  beside the two Open in… commands, and `src/lib/open.ts` is the one place 049 changes to
  `window.open` in the browser. In 028's fixture mode `o` therefore fails, and the view
  shows the error like any other; the screenshot script never presses it.

  `o` deliberately does **not** fall back to an older run's `pr_url`. Runs carry no branch
  column, and after a reject an older run's PR belongs to the set-aside branch: falling back
  would put the reviewer on the work they rejected. With no `pr_url` on the newest run, `o`
  opens nothing and the view says no pull request was recorded for the latest run. A
  needs-changes run that pushed to an existing PR without printing its URL shows that line
  too. That is a gap in what the runner records, and the fix belongs there, not in a UI
  guess.
- **The queue is live.** It re-reads on `tasks:changed` and `runs:changed` through
  `src/lib/events.ts` (D7). A task that leaves `in_review` is dropped from the queue, and a
  task that arrives joins in board order. When the task on screen leaves **through another
  door** (an approve over MCP, a drag on the board), the view moves to the next and says in
  one line what happened to it. The view's own actions do not count as another door: a task
  whose action this view has pending, or has just completed, leaves silently (reject shows
  its set-aside line instead), even though the backend's `tasks:changed` for it arrives
  like any other.

**Chain awareness**

- **Dependents.** When the task on screen has dependents (ADR-0008), the view names each
  one with its column, from `get_task_dependents`, in the order returned. A dependent whose
  `built_on` is true is marked **"Already ran on this branch"**. Archived dependents
  (034's `dependents_of` includes them, with `archived_at`) are listed with the word
  "Archived".
- **Rejecting it warns which downstream tasks are affected, and so does needs changes.**
  Both move the task out of `in_review`, and ADR-0008's amendment makes a dependency
  satisfied only in `in_review` or `done`, so every dependent becomes blocked the moment
  either lands. The warning appears in the note step, before submission, and names every
  **unarchived** dependent. Archived ones are left out of the warning, because an archived
  task is never picked up and so is not blocked by anything. It is not a second
  confirmation dialog: the review stays keyboard-complete, and `Mod+Enter` after reading
  the warning is the confirmation. Approve does not warn.
- **Builds on.** The task's own dependencies are listed under "Builds on", because
  ADR-0008's Consequences promise that the review view shows the chain and that stacked PRs
  are reviewed in order. The two lists use distinct words on purpose: "Builds on" is what
  this task depends on; "Already ran on this branch" is a dependent's `built_on`.

**Fixture data for the screenshot script**

Task 028's matrix is scenario × view, and a view is reached by clicking the sidebar. Two
additions make this view's states reachable, and both belong to 028's spec table rather
than a second mechanism:

- **One scenario per state**, with the task that shows the state first in board order, so
  the queue opens on it.
- **An optional key sequence per row**, pressed after the sidebar click and before the
  capture. Only non-mutating keys appear in it, because 028's rule is that nothing the
  script does triggers a write. `Enter`, `j`, `r` and `c` qualify; `a`, `o`, `w` and
  `Mod+Enter` do not.

| Scenario | Keys | State captured |
| --- | --- | --- |
| `review-digest` | — | A digest led by a failure and a blocked chain, with a skipped entry and one run whose cost was not recorded |
| `review-truncated` | `Enter` | A queue task whose patch was truncated, with a PR |
| `review-pruned` | `Enter` | A queue task whose patch was pruned |
| `review-no-commits` | `Enter` | A queue task whose newest run is `recorded` with `bundle: null` |
| `review-not-recorded` | `Enter` | A queue task whose newest run is `not_recorded` (recorded before 033), while the fixture's `get_diff_summary` row still answers with files |
| `review-chain` | `Enter r` | A task with three dependents (one `built_on`, one archived) and one dependency, with the reject note step open: its set-aside explanation and its warning |
| `review-empty` | `Enter` | The empty queue |

In both colour schemes and at both widths, as 028's projects already do.

## Out of scope

- **Every backend change.** The review actions, `dependents_of`, the digest service and
  their MCP tools are task 034's. The bundle, `runs.head_sha` and the bundle on `get_run`
  are task 033's. This task adds no migration, no Tauri command, no MCP tool and no
  dependency. If a wrapper 034 was meant to add to `src/lib/commands.ts` is missing, add
  only the wrapper. A second path to the same backend function is a bug (ADR-0006).
- **Review findings, loop history, and the "unreviewed" flag.** Task 035 adds the
  findings store and the loop summary on a digest entry; task 021 produces them; task 037
  renders them in this view. Leave a place after the PR link where a findings section
  slots in, and build nothing in it.
- **A live diff fallback** for `not_recorded` runs, such as those recorded before 033.
  They show "No diff was recorded for this run.", the run overlay keeps its
  `getDiffSummary` fallback for them, and Open worktree is one key away on the desktop.
- **Syntax highlighting, side-by-side diffs, per-file folding and inline comments.** The
  full diff is on the forge, one link away (ADR-0033 point 7).
- **Forge actions:** merging, closing or commenting on a PR. A rejected task's old PR stays
  open, and the set-aside line says so.
- **A key for `mark_review_digest_seen`.** 034 adds the wrapper; this view does not call
  it (see "Where the app opens").
- **Undo.** An approve is reversible by moving the card, and 034's actions are ordinary
  board writes.
- **Bulk actions.** Approve-all is the opposite of a review.
- **Team mode.** Who may approve (roles, 051), how the browser hides Open worktree
  (049, 050), and whose runner holds the worktree (054) are later tasks. Nothing here
  should make them harder. The only client-side capabilities this view uses beyond board
  commands are the two Open in… commands, which stay inside `OpenInMenu.tsx`, and
  `openExternalUrl`, which stays inside `src/lib/open.ts`.
- **A configurable editor command per repository**, the original optional item. Task 026
  landed the Open in… menu, which answers it.

## Acceptance criteria

Frontend tests are vitest. They mock `@tauri-apps/api/core` and `@tauri-apps/api/event`,
never the wrappers (the pattern `StorageSection.test.tsx` explains), and assert the exact
command name and arguments each keypress sends. `openExternalUrl` is covered by the same
mock, because `openUrl` sends `plugin:opener|open_url` through `invoke`. Time comes from an
injected `now` or from vitest's fake timers. No test waits on real time.

- **After a queue of six tasks, the digest reports each outcome.** Given 034's six-task
  digest (a succeeded task now `in_review`, a failed one, a blocked dependent of the failed
  one, one skipped because its repository does not allow unattended runs, a cancelled one,
  and a succeeded one whose cost was not recorded), the digest renders one entry per task
  with the right word, in the order received. The Blocked entry names its blocker, and the
  Skipped entry shows `QUEUE_SKIP_LABELS.unattended_runs_not_allowed`. No entry shows
  `$0.00` for a `null` cost.
- **The totals are the service's, not a sum.** In a fixture whose `DigestTotals` differ
  from any sum over its entries (for example `cost_usd` 4.20 while the entries' costs add
  to 3.10, and `run_seconds` likewise), the rendered run count, run time and cost equal
  `totals.runs`, `totals.run_seconds` and `totals.cost_usd`, and the "N runs had no
  recorded cost" line reads its N from `totals.runs_without_cost`. With
  `runs_without_cost: 0` that line is absent.
- **The digest follows the backend.** A `settings:changed` event re-invokes
  `get_review_digest`, and so do `runs:changed` and `tasks:changed`.
- **The copyable summary is exact.** `digestAsText(digest, now)` for the six-task digest
  equals a literal expected string, asserted in full rather than by substring.
- **The app opens where it should.** `openingView` returns `welcome` when onboarding is not
  dismissed, whatever the digest; `review` when `totals.runs > 0`; `board` when the digest
  holds only Blocked and Skipped entries and `totals.runs` is 0; `board` for an empty
  digest; and `board` when the digest read failed. One test per case.
- **The queue is `in_review` in board order.** Given `in_review` tasks in two repositories
  whose positions interleave (repository A at 1 and 3, repository B at 2), tasks whose
  `position` order differs from their creation order, an archived `in_review` task, and
  tasks in other columns, the queue equals `groupIntoColumns(tasks).in_review` minus the
  archived one: A's two tasks, then B's, not A1, B2, A3.
- **The whole queue is reviewable with the keyboard only.** One test renders the review
  view **opened on the digest** over three `in_review` tasks and uses nothing but
  `userEvent.keyboard`. `Enter` reaches the first task; `d` returns to the digest and
  `Enter` comes back. It moves with `j` and `k` (and the arrows), stops at both ends,
  approves one task, sends another back with needs changes and a note, rejects the third
  with a note, and reaches the empty-queue state. The mocked `invoke` receives exactly
  `approve_task`, `request_task_changes` and `reject_task`, with the expected task ids and
  note texts, in that order, and no other command that writes. A `tasks:changed` event is
  delivered after each action, and no "left the queue" line appears for any of the three.
- **Approve moves the task to `done` and advances to the next.** After `a`, `approve_task`
  is invoked for the task on screen, and the next task in board order is shown. After the
  last one, the empty state is shown.
- **Reject and needs changes are two different commands, and say so.** `r` then
  `Mod+Enter` invokes `reject_task`; `c` then `Mod+Enter` invokes `request_task_changes`.
  The note step for `r` says the worktree will be removed and the work kept on a set-aside
  branch; the note step for `c` says the worktree and branch are kept. The note is passed
  verbatim, including newlines. `Escape` in the note field closes it and invokes nothing.
  Typing `a`, `r`, `j` or `o` into the note field inserts the letters and triggers no
  action.
- **A reject names where the work went.** With the mocked `reject_task` resolving to a
  `ReviewOutcome` whose `set_aside_branch` is `rimaia/x-2`, the view renders that exact
  branch name after `r` and `Mod+Enter`, with the line saying an open pull request stays
  open.
- **A service refusal is shown, not swallowed.** When the mocked `invoke` rejects an
  action with a `RimaiaError`, its message is rendered, the task stays on screen, and the
  note text is kept.
- **Rejecting a task with dependents warns and names them.** For a task whose
  `get_task_dependents` returns three dependents, one of them archived and one with
  `built_on: true`, the chain section names all three, marks the archived one "Archived"
  and the built-on one "Already ran on this branch". Pressing `r` shows the warning naming
  the two unarchived dependents, and not the archived one, before anything is invoked. The
  same holds for `c`. For a task with no dependents, neither shows a warning. Approve never
  shows one. The task's own dependencies render under "Builds on".
- **The review order holds without navigation.** For the task on screen, `get_run` is
  invoked with the id from `get_task`'s `lastRun`, and the outcome, diff summary, commits,
  PR link and patch are rendered in that DOM order (asserted with
  `compareDocumentPosition`), with no click and the patch not inside a closed `<details>`.
  An `interrupted` run shows "Interrupted" while the task's `run_state` is `failed` (D9).
- **Every `RunReview` variant renders its own line.** Truncated, pruned, `recorded` with
  `bundle: null`, `not_recorded`, and no run at all each render the line in Scope's table.
  Only the recorded-with-bundle cases render a file count. For `not_recorded`, the test's
  mocked `invoke` would answer `get_diff_summary` with a summary listing files, and the
  view still renders "No diff was recorded for this run." with no file count and none of
  those file names, because it never invokes `get_diff_summary`. Across the whole keyboard
  test, `get_diff_summary` and `get_worktree_status` are never invoked.
- **`liveDiff` decides the fallback, and nothing else.** `RunReviewSections` given a
  `not_recorded` review with `liveDiff: "fallback"` invokes `get_diff_summary` once with
  the task id; with `liveDiff: "none"` it invokes nothing and renders "No diff was
  recorded for this run."
  Given a `recorded` review, it invokes `get_diff_summary` under neither value.
- **The run overlay is unchanged.** `RunDetailOverlay.test.tsx`, as 033 left it, passes
  unedited against the extracted `RunReviewSections`, its two not-recorded cases
  (`get_diff_summary` answering, and rejecting) included.
- **Open PR and Open worktree.** `o` invokes `plugin:opener|open_url` with the newest
  run's `pr_url`, exactly. No file outside `src/lib/open.ts` imports
  `@tauri-apps/plugin-opener`. With no `pr_url` on the newest run, `o` invokes nothing, even
  when an older run of the same task has one, and the view says no pull request was
  recorded. `w` opens the Open in… menu for a task with a worktree and does nothing for a
  task without one.
- **The queue follows other doors.** When a `tasks:changed` event arrives that this view
  did not cause, and the re-read no longer has the on-screen task in `in_review`, the view
  shows the next task and one line naming what happened to the one that left. A task that
  newly appears in `in_review` is inserted in board order.
- **`isEditableTarget` has one home.** It is exported from `src/lib/keyboard.ts`, imported
  by both `Board.tsx` and the review view, and its tests live in
  `src/lib/keyboard.test.ts`. `Board.tsx` no longer exports it.
- **Screenshots.** `npm run screenshot` produces the review view in every state in Scope's
  fixture table, reached by the row's scenario and key sequence, in both colour schemes and
  at both widths, from 028's fixture mode with no Rust built. The PR says which images were
  looked at, and what was changed because of them.
- **Nothing outside the frontend changed.** No file under `crates/`, `src-tauri/` or
  `.sqlx/` is in the diff, no dependency is added, and `./scripts/check-command-wiring.sh`
  passes.
- `npm run typecheck`, `npm run test` and `npm run build` pass, as does the rest of
  CLAUDE.md's command list, unchanged.

**Needs a person.** Whether this *feels* like a morning review rather than a form is not
something a test can establish. The end-of-M1 smoke run in the plan is where it is judged:
`RIMAIA_DATA_DIR=/tmp/rimaia-m1 npm run tauri dev`, a real queue overnight or its
equivalent, and the review done with the keyboard only. The PR carries that as a
checklist item.

## Notes

This is the task where the product either feels good or feels like a database with a
board. Time spent here is well spent — but only after there are real nights of results to
design against.

**What changed, 2026-09-30.** The original task owned both halves: the actions (move to
`done`, append a note, keep the worktree) and the screen. Team mode split them. The actions
became task 034's, so the MCP server and the future web client get the same rules, and the
diff became task 033's stored bundle, because in team mode the reviewer usually does not
hold the worktree (ADR-0033 point 7, ADR-0036). `depends_on` moved from `015` to `033` and
`034` accordingly, and the milestone from v0.3 to v0.4, the review milestone both of them
are in. The original acceptance criterion "'Needs changes' preserves the worktree and
branch, and the next run resumes there with the note included in its prompt" is now 034's,
proven there against a real repository in a `TempDir` with the exact composed prompt. Here
it is the narrower claim that `c` sends 034's needs-changes command and not reject.

**The risk the plan names.** Task 021 asked to be revisited "after 017 has been used for
real mornings", and an unattended workflow has no mornings in between. The workflow's
`stopAfter: "017"` exists for that. If this task lands and nobody has reviewed a real night
with it, say so in the PR rather than implying it was tried.

**Seam entries to read:** D7 (events only through `src/lib/events.ts`; an empty payload
means every id), D8 (render a refusal's message, add no error code), D9 (`interrupted` is
read off the run's exit class, never the task's state), D12 and its amendments
(`TaskSummary` and `LastRunSummary`, which the queue reads for order and state), D18
(`null` is "not recorded"), D29 points 4, 5 and 8 (the newest row's bundle; one digest
entry per task), D32's appendix (`get_run` is a board command carrying the bundle;
`get_diff_summary`, `get_worktree_status`, `list_open_in_targets` and
`open_task_worktree_in` are local). D4 and D6 apply as prohibitions: no migration, no new
dependency. Also read ADR-0024, since this is a new screen and ADR-0024 is the design brief
(sentence case, a dot and a word, actions reachable by keyboard), and ADR-0033 point 7.

**Files to start from.**

- `src/App.tsx`: the route state and the opening-view decision, which moves into
  `openingView`. Its fallback to the board when `get_app_info` fails is the pattern for a
  failed digest read.
- `src/types.ts`: add `"review"` to `View`. 033's bundle types and 034's digest and
  dependents types are mirrored here by those tasks.
- `src/components/Sidebar.tsx`: the entry for the new view, with the queue's count.
- `src/lib/board.ts`: `groupIntoColumns` and `compareBoardOrder`. Reuse them rather than
  sorting a second way. `relativeTime(timestamp, now)` takes its clock as an argument,
  which is the pattern to follow.
- `src/hooks/useTasks.ts`: the board's read and its change-event subscription.
- `src/components/board/Board.tsx` and `Board.test.tsx`: `isEditableTarget`, its tests, and
  the window-level `keydown` pattern, including why shortcuts must not fire while typing.
- `src/components/board/OpenInMenu.tsx`: task 026's menu, already keyboard-navigable.
  Open it from `w`; do not build a second one.
- `src/components/runs/RunDetailOverlay.tsx` and its test: the ADR-0013 sections to
  extract into `RunReviewSections.tsx`.
- `src/components/runs/QueuePlanList.tsx`: `QUEUE_SKIP_LABELS`.
- `src/views/settings/StorageSection.test.tsx`: why tests mock `invoke` and not the
  wrappers.

New, suggested: `src/views/ReviewView.tsx` (digest and queue), `src/lib/review.ts` (the
pure parts: the index after a removal, the key map, `openingView`, `digestAsText`) with
`src/lib/review.test.ts`, `src/lib/keyboard.ts` and `src/lib/open.ts` with their tests, and
component tests beside each component. Keep the logic in `src/lib/review.ts`. jsdom has no
layout engine, and pure functions are where exact assertions are cheap.

**Migration: none.**

**What the chain provides.** Task 028 provides the fixture mode and `npm run screenshot`.
It is ordered first and is not in `depends_on`, because it is not a data dependency, but
the screenshot criterion assumes it, and this task adds the per-row key sequence to its
spec table. Task 033 provides `head_sha`, the `review_bundles` row written at `finish_run`,
the `RunReview` on `get_run` and its `RunDetail` mirror in `src/types.ts`. Task 034 provides
`approve_task`, `reject_task`, `request_task_changes`, `get_task_dependents`,
`get_review_digest` and `mark_review_digest_seen` as core services with Tauri commands,
`commands.ts` wrappers and MCP tools. Use those names; if 034's wrappers differ, theirs win,
because this file does not choose them.

**What the next tasks expect.** Task 035 gives `LastRunSummary` a `kind` and adds a loop
summary to each digest entry. The digest entry component should render fields it does not
know about yet by being given them, not by being rewritten. Task 037 adds loop history and
findings to the task on screen, after the PR link. Task 049 routes commands by kind and
task 050 decides what the browser shows for local-only actions. Both rely on this view
using exactly three client-side capabilities beyond board commands: the two Open in…
commands inside `OpenInMenu.tsx`, and `openExternalUrl` inside `src/lib/open.ts`, which 049
switches to `window.open` in the browser.

**Size.** Frontend only: roughly 850 lines of components and CSS, 300 of pure logic,
1,000 of tests and 250 of fixture seed, well inside one session. If it runs over, cut the
copyable summary and the "Builds on" list of the task's own dependencies, in that order.
The dependents warning, the keyboard-complete queue and the bundle states are the task.
