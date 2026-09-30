---
id: "057"
title: Push postcondition, and run elsewhere
milestone: v0.5
status: ready
depends_on: ["053", "054"]
adrs: ["0033", "0031", "0012"]
size: M
---

# Push postcondition, and run elsewhere

## Goal

Two rules that exist on paper, made true in code, for runners that report to a board on
another machine.

**1. A connected run is `success` only if its branch is on the remote at `head_sha`**
([ADR-0033](../docs/adr/0033-repositories-belong-to-the-team-checkouts-to-the-runner.md)
point 4). After the agent exits and task 033's capture has recorded `head_sha`, a connected
or headless runner checks `origin` with `git ls-remote`. If the task branch is not there at
`head_sha`, the runner pushes it itself:

- a plain push of the task's own `rimaia/…` branch and nothing else;
- never forced, never with a `+` refspec, which ADR-0012 point 3's denied operations already
  forbid the agent;
- never to the default branch, which ADR-0012 point 4 asks of the agent and which the
  runner's push enforces in code by naming only the task branch.

A branch that cannot be pushed turns the outcome into `fatal`/`failed`, with the push error
on the card, before `finish_run` is called. The board never checks this itself (D31 point
6). **Solo is unchanged:** no `ls-remote`, no push, and a repository with no remote still
succeeds.

**2. A pinned task can be moved to another runner, and the runner it left is fenced**
([ADR-0031](../docs/adr/0031-runners-claim-work-with-leases.md) point 4). A person chooses
"run this elsewhere" and names the runner. In one transaction the board moves the pin,
advances the task's lease generation and puts the task back in the queue. The next attempt:

- starts a new agent session on the new runner;
- uses a fresh worktree, created from the task branch's head on `origin` if it was ever
  pushed, and from its base (task 044's `RunContext::base`) otherwise.

The runner it left keeps its worktree, marks it fenced in its own store, never pushes from
it and is not offered the task again while it holds it. Unpairing a runner (task 047)
releases every pin it holds the same way, with no target.

## Why now

053 made leases real across a network, and 054 made a repository something two machines
share through its remote. The next two tasks, 058 (the headless runner) and 059 (desktop
connected mode), are the first to put a second real machine on a board. Both rules have to
hold before that happens, or two things break on the first night:

- **Chains break across machines.** Task 044 branches a dependent from its dependency's
  `runs.head_sha`. If dependency A ran on Alice's laptop and its agent did not push, B's
  runner on Bob's machine fetches and finds no such commit. 044's refusal then stops B,
  which is the right failure but the wrong place for it: the card for A says `in_review`,
  and nobody can review work that exists only on one laptop.
- **A lost laptop strands its tasks.** 053 pins a task to the runner whose lease expired,
  so that the retry resumes the same session in the same worktree. Only a person can undo a
  pin, and no door exists yet. 045 leaves a reassigned, pinned task "claimable by nobody
  until 'run elsewhere' (057) releases the pin". 047's `unpair_runner` explicitly leaves
  releasing pins to this task.

## Scope

### 1. Which runs the postcondition applies to

- **Connected and headless runners only.** `RunnerConfig` gains
  `branch_postcondition: BranchPostcondition`, an enum with two variants:
  - `Recorded` (the `Default`): today's behaviour. `head_sha` is recorded, and nothing is
    checked or pushed.
  - `OnOrigin`: this task's check and push.

  `src-tauri/src/lib.rs` keeps the default in solo. 058 and 059 set `OnOrigin` where they
  build their HTTP board port. It is a runner property, like `program`. It is not derived
  from the port, because the contract suite runs both adapters and the postcondition has to
  be testable against each of them.
- **Every run kind, for every outcome classified `Success`, and nothing else.** A failed,
  cancelled, interrupted or retryable run is not checked and not pushed. A strategy lease
  has no `runs` row and commits nothing, so it is not checked either.
- **The review loop (task 021): every successful phase, not only the last one.** ADR-0033
  point 4 says the postcondition "applies once, to the commit the whole loop ends on". The
  runner cannot know which phase is last: D31 point 6 has it decide before `finish_run`,
  and only `finish_run`'s answer (`Continue` or `Released`) says whether the loop goes on.
  So the runner applies the check at every successful phase. The reason this is also the
  correct reading is 021's exit table. A failed review or fix lands the task in
  `in_review`, *because the implementation had already succeeded*. If the implementation's
  success had not been verified on `origin`, an unpushable branch would reach `in_review`
  through a later phase's failure, and that is the outcome point 4 exists to prevent. The
  cost is small:
  - A review that clears the branch leaves `HEAD` where it was. Its check is one
    `ls-remote` and pushes nothing.
  - A fix pushes what it committed, which the base instructions ask of it anyway.
  - The commit the loop ends on is always checked, because it is the last success.

  An implementation whose branch cannot be pushed is `failed`, and the loop never starts.

### 2. The check and the push

These live in two files:

- **`crates/core/src/worktree/remote.rs`** (new): the git calls. Each is a plain argument
  vector through `worktree::git`'s runner, with no `sh -c`.
- **`crates/core/src/runner/postcondition.rs`** (new): the decision, as a function that
  takes the outcome by `&mut` and rewrites it the way `override_as_fatal` does.

In order, after `worktree::bundle::capture` (033) and before `finish_run`:

1. **The worktree is on the task branch.** `git symbolic-ref -q HEAD` must name
   `refs/heads/<branch>`, where `<branch>` is the task's recorded `tasks.branch`. A
   detached `HEAD`, or an agent that switched to another branch, fails the check. The
   runner does not push a commit the task branch does not point at.
2. **The branch is a task branch.** It must start with `worktree::naming`'s
   `BRANCH_NAMESPACE` and must not equal the repository's default branch. This check is a
   pure function, `push_target(branch, default_branch) -> Result<&str>`. The recorded name
   comes from the board, and ADR-0003 counts the sqlite3 CLI as a writer.
3. **The lease is still current.** The runner calls `run_context` on its lease. A
   `Conflict` means no push: the runner takes D31 point 11's single reaction (owned by
   053) and never reaches step 4. Any other error also means no push, and the outcome
   becomes `failed` with the step-3 sentence below. This check shrinks the window in which
   a fenced runner could push from minutes to one round trip. It cannot close the window
   completely: the agent's own push, while it is still running, is out of Rimaia's hands.
   See Notes.
4. **`git ls-remote --heads origin refs/heads/<branch>`.** If it answers `head_sha`, the
   check passes, and nothing is pushed.
5. **Otherwise, push:**
   `git push --porcelain origin refs/heads/<branch>:refs/heads/<branch>`, run in the
   worktree. The rules for this command:
   - No `--force`, no `--force-with-lease`, no `+`, no `--mirror`, no `--all`, no
     `--tags`.
   - No `--no-verify`. A repository's pre-push hook is the repository's rule.
   - A remote branch that is not an ancestor of `head_sha` is rejected by git as a
     non-fast-forward. That rejection is the failure. The runner never reconciles a
     diverged branch.
6. **Verify, do not trust.** `ls-remote` runs again, and it must now answer `head_sha`.

**The remote is `origin`, by name.** 054's checkout mapping verified that `origin`
normalises to the team repository. The branch's upstream configuration is never consulted.

**The environment** is built by one pure function, `push_environment(secret, parent) ->
ChildEnvironment`. It is task 022's `credentials::inject::child_environment` for the
checkout's credential, which 054 re-keyed to the pair (repository, runner), plus:

- `GIT_TERMINAL_PROMPT=0`, so an unattended push fails instead of waiting for a password
  nobody will type;
- `GIT_ASKPASS` and `SSH_ASKPASS` removed.

A checkout with no credential stays ambient, exactly as its agent run was.

**Cancellation.** A Cancel that arrives during steps 4 to 6 kills the git child through the
run's existing cancel token, and the outcome becomes `cancelled`, as it would have been
while the agent was running.

**What reaches the card.** Every git failure detail comes from git's stderr, in this order:

- passed through the run's `Redactor` (`credentials::redact`), so a token in a URL never
  reaches a card or a log;
- whitespace-collapsed;
- truncated to 500 characters, with `…` appended when anything was cut.

The messages, as exact strings (`{short}` is the 12-character abbreviation of a commit):

| Case | `error_message` |
| --- | --- |
| step 1 | `The run finished with its worktree on {where}, not on {branch}. Rimaia pushes only the task's own branch, so nothing was pushed.` where `{where}` is `a detached HEAD` or `branch {name}` |
| step 2 | `Rimaia pushes only a task's own rimaia/ branch, and {branch} is not one, so nothing was pushed.` |
| step 3, not `Conflict` | `The run finished, but Rimaia could not confirm with the board that this runner still holds the task, so nothing was pushed. {detail}` |
| step 4 or 6, `ls-remote` failed | `The run finished, but Rimaia could not read origin to check for {branch}. git said: {detail}` |
| step 5 failed | `The run finished, but {branch} could not be pushed to origin, so nobody else can reach its work. git said: {detail}` |
| step 6 disagrees | `The run finished, but after the push origin has {branch} at {remote_short}, not at {head_short}.` |

`exit_class` becomes `Fatal` and `status` becomes `Failed`. `head_sha`, the bundle, the
usage figures, `pr_url` and `spawned_as` are kept, as `override_as_fatal` keeps them. The
bundle still uploads: 056 reports it with `finish_run`, and a reviewer can see what could
not be pushed.

### 3. A fresh worktree from the remote, for a runner that has none

When `branch_postcondition` is `OnOrigin`, `worktree::prepare` changes in one case only: no
worktree for the task exists on this runner, and the task has a recorded, non-blank
`tasks.branch`. It still takes its flag from its caller. It never reads `RunnerConfig`
itself. Then:

1. **The fetch is required.** `git fetch --prune origin` must succeed. If it fails, the
   start is refused with `Could not fetch origin to find {branch}, so this task cannot
   start from where its last run left it. git said: {detail}`. Starting from the base
   instead would build on a branch that may be behind the one on `origin`, and the run
   would then fail its own postcondition at the end, after spending the night.
2. **The branch name is the recorded one.** `resolve_branch`'s collision suffixes apply
   only to a task with no recorded branch. Two runners must push the same ref, so a second
   runner never derives a different name.
3. **Where it starts:**

   | Local branch | `refs/remotes/origin/<branch>` | Worktree starts at |
   | --- | --- | --- |
   | absent | present | the remote head, creating the local branch there |
   | absent | absent | the base, as today (044's `RunContext::base`) |
   | an ancestor of, or equal to, the remote head | present | the remote head, moving the local branch with `git update-ref refs/heads/<branch> <remote> <old>`, a compare-and-swap that cannot rewrite history |
   | ahead of the remote head (the remote is its ancestor) | present | the local branch, as is |
   | diverged from the remote head | present | refused: `{branch} here ({local_short}) and on origin ({remote_short}) have diverged. Rimaia never rewrites a branch. Reconcile it by hand, then run the task again.` |
   | present | absent | the local branch, as is |

`base_sha` is still 033's `git merge-base <base_ref> HEAD`, so a run that started from a
pushed head still records the fork point of the whole branch. An existing worktree is
reused exactly as today. Section 5 keeps a stale one from ever being handed a claim. With
`Recorded`, `prepare` is byte for byte what it was.

### 4. Run elsewhere: the board side

**One function releases a pin, and both doors go through it.**
`release_pin(tx, clock, task_id, target: Option<RunnerId>) -> ReleasedPin` lives in the
module where 043 writes `tasks.pinned_runner_id`, and runs inside its caller's transaction.
It does these things, and nothing else:

- sets `pinned_runner_id` to `target`, which is `NULL` when `target` is `None`;
- increments `tasks.lease_generation`, so every `LeaseRef` the old holder still has is
  refused as `Conflict` by every lease method (D31 point 3). That includes a lease whose
  expiry the board has already processed;
- moves `run_state` so that the next claim starts a new session:
  - `waiting_retry` goes to `failed`, then `queued`: the pinned attempt is abandoned, and
    the task is queued again. Both edges exist in `is_legal_run_state_transition`, and both
    are written through the in-transaction form of `set_run_state` that 043 added for the
    one-transaction claim. The run-state table does not change.
  - every other state is left alone.

  A queued task is claimed as a new attempt with no `resume` (D23 point 7's "a task the
  user re-queued … starts a new session").

**`run_task_elsewhere(ctx, task_id, runner_id)`**, the person's door, in one transaction:

- **Refuses** each of the following as `Invalid`, with these exact sentences:
  - the task is not pinned: `"{title}" is not waiting for a particular runner, so there is
    nothing to move.`
  - a `runner_leases` row exists for the task, of any purpose: `"{title}" is running on
    {label}. Cancel the run first. A runner that has gone quiet loses the task on its own
    once its lease runs out.` A live lease means a process that may still be committing
    and pushing, and moving the pin under it would race it.
  - `runner_id` is the runner it is pinned to: `"{title}" is already waiting for {label}.
    Choose another runner.`
  - the target is unpaired, does not map the task's repository (054's
    `runner_repositories`), or is not eligible for the task under 045's
    `eligibility::decide`: `{label} cannot take "{title}": {reason}.` with 045's reason
    sentence, or `it has been unpaired`, or `it has no checkout of {repository}`.
  - the task is not in `ready`.
- **Refuses a task in another team, and a target runner in another team, as `NotFound`**,
  worded as for one that does not exist (039's rule, ADR-0029 point 5).
- **Otherwise** calls `release_pin` with `Some(runner_id)`, then publishes
  `ChangeEvent::tasks` for the task's team after the commit.
- **Consent is not checked here.** The claim checks it (045), and the card shows what is
  missing. Moving a task to a runner whose owner has not yet consented is a legitimate step
  towards running it there.

**Why a target is required.** ADR-0031 says "run this elsewhere … on another runner". With a
pin cleared to `NULL`, the runner it left would be as eligible as any other, and would
usually be first to claim it, because it is awake and long-polling. D28 has no column that
could say "anyone but that one". Its D4 amendment forbids adding one without an amendment.
Naming the target uses the one column that exists. It also gives the owner of the runner the
task left a definite answer to "where did it go".

**`list_run_elsewhere_targets(ctx, task_id) -> Vec<RunElsewhereTarget>`** (Read) returns
the runners the command would accept, plus the pinned runner itself, each with `runnerId`,
`label`, the owner's `login`, `lastSeenAt`, and `refusal: Option<String>` (the sentence
above, or `None`). In a personal team served in process there is one runner, the pinned
one, so the list offers nowhere to go. That is how solo stays unchanged without a special
case. Both functions are `pub` in `rimaia-core`, with no path in any DTO (ADR-0028 point 2's
test covers them).

**The doors.**

- **Registry.** Two rows in D32's registry (`crates/core/src/api/registry.rs`):
  `run_task_elsewhere` (board, `Write`) and `list_run_elsewhere_targets` (board, `Read`),
  with handlers in `crates/core/src/api/board/`. From 046 on, the registry *is* the list,
  so these two rows are the whole wiring for both `invoke` and `/api/v1`.
- **MCP.** Two MCP tools of the same names in `mcp/server.rs`, following ADR-0021's parity
  rule: neither spawns a process, so D23's exception does not apply. Both are
  `RunAccess::Refused` in `Tool::run_access`, because a run has no business moving its own
  task between machines. `every_registered_tool_has_a_run_scope_decision` then covers
  them.
- **Frontend.** `src/lib/commands.ts` and `src/types.ts` gain typed wrappers and the DTO.
  No component changes. 061 draws the card.

### 5. Run elsewhere: the runner side

- **Fencing is a runner fact.** `worktrees.fenced_at` (D28's runner set, written first
  here) is set, never cleared, and means:
  - the worktree is kept;
  - it is never pushed from, not by the postcondition and not by anything else Rimaia
    runs;
  - it is never reused for a claim;
  - its task is not claimed by this runner while the row exists.

  The row goes away only when its owner removes the worktree through the existing removal
  path, with D20's guards unchanged (an unpushed commit still needs its confirmation).
  After that, the runner may take the task again, starting fresh as in section 3.
- **When a runner decides a worktree is superseded.** One pure function,
  `superseded(me, task: &TaskDetail) -> bool`, returns true when either of these holds:
  - the task is pinned to another runner;
  - the task's newest run row (D29 point 4's "newest", every kind) was recorded by another
    runner, or by none (`runs.runner_id IS NULL`: a deleted runner counts as another).

  In solo every row names the one runner, so this is never true there.
- **Where it is applied.** In three places, each reading the task through the port's
  `preview`:
  - **053's `Conflict` reaction** (D31 point 11). After it stops the process and drops the
    lease, it fences the worktree if it is superseded, instead of claiming again through a
    pin that has moved.
  - **043's per-runner startup reconciliation.** It fences every superseded worktree the
    runner holds, so a laptop that was off while its task moved learns of it at launch.
  - **The starter preflight** that D31 point 5 made one `rimaia-core` function, for Run
    now, Retry now and Plan now. A fenced or superseded worktree is refused with `This
    machine's worktree for "{title}" was left behind when the task moved to {label}. It is
    kept for you to look through. Remove it to run the task here.` A superseded one is
    fenced first.

  Each fencing writes one `tracing::warn!` naming the task and the runner it moved to. The
  local worktree inventory entry gains `fencedAt`, so 061 can tell the owner.
- **The board never hands a runner a task it holds a stale worktree for.**
  `ClaimTarget::Next` gains `#[serde(default)] worktrees: Vec<HeldWorktree { task_id,
  fenced: bool }>`, built from `runner.db`'s `worktrees` table on every request. The board's
  selection skips a task for this runner when it is listed and either `fenced` is true or
  the `superseded` rule holds, which it evaluates in SQL. This closes the one gap the three
  places above leave: a runner that stays awake and idle through a move, and is then offered
  the same task after the other runner has finished with it. The skip is per runner, so it
  is not a `SkipReason` on the card. The field is `serde(default)` because it is added after
  052 (D31 point 6), and an older runner that omits it is only as protected as the three
  places above make it.
- **`resume` goes only to the runner that recorded the session.** The board sets
  `Claim::resume` only when the claiming runner is the one on the newest row. If 043
  already enforces this, this task adds the contract case and nothing else. Resuming a
  session on a machine that does not have it is the seam bug ADR-0026 point 6 warns about.

### 6. Unpairing releases pins

`identity::unpair_runner` (047) gains, in its existing transaction:

- **Every live `runner_leases` row of that runner** is ended the way 053's expiry ends one:
  the run is closed `interrupted`, and the `run_state` edge is what 053's expiry writes.
  The difference is that no pin is written. The runner's token is being deleted in the
  same transaction, so its heartbeat could never renew the lease anyway, and waiting for
  expiry would only pin the task to a runner that cannot come back.
- **`release_pin(tx, clock, task_id, None)`** for every task pinned to it, including any
  pinned by the step above.

It returns the released task ids with their teams, and its caller publishes one
`ChangeEvent::tasks` per affected team after the commit. 047's function takes a connection,
not a context, and stays that way. 053's expiry sweep gains one guard: it never pins a task
to a runner with `unpaired_at` set.

**Every path that unpairs goes through `unpair_runner`.** That is 047's `revoke_api_token`
for a runner token, and 051's account deletion, which removes the account's runners. If 051
deletes `runners` rows directly, this task routes it through `unpair_runner` first.
`ON DELETE SET NULL` would clear a pin without advancing the generation or requeueing the
task.

### 7. Contract, CLAUDE.md, D31 and the caches

- **The board contract suite** (`crates/core/src/testing/board_contract.rs`) gains this
  task's cases, listed under Acceptance criteria. They run through both adapters (036's
  in-process one, 052's HTTP one).
- **D31 is amended in the same PR** with one dated paragraph:
  - `ClaimTarget::Next`'s `worktrees` field and the skip rule;
  - the `resume` rule;
  - its point 6 bullet for 057, rewritten to say "every successful phase", with section 1's
    reason.
- **CLAUDE.md:**
  - "the push postcondition and pin release" joins the modules that must have tests;
  - the Gotchas list gains: "A connected run is not `success` until its branch is on
    `origin` at `head_sha`. The runner pushes only the task branch, never forced, and never
    from a fenced worktree."
  - CI changes nothing, and CLAUDE.md's command list stays identical to it.
- **The offline caches.** Regenerate both with D33's recipe. The skip rule, `release_pin`
  and the targets read add queries on the board side. Setting `fenced_at` and listing held
  worktrees add queries on the runner side. There is no migration.

## Out of scope

- **Opening a pull request.** The base instructions still ask the agent to. The runner
  pushes the branch and does nothing on the forge.
- **Pushing from a solo runner**, including a solo repository that has a remote. ADR-0033
  point 4 keeps solo's push a request, not a condition.
- **Any interface.** The pinned card, the target picker, the "the agent loses its
  conversation" copy, the fenced-worktree notice and the push error's presentation are all
  061's. This task gives 061 every read, command and sentence it needs.
- **Moving a task that is not pinned to a chosen runner.** That would be assignment by
  machine, which ADR-0032 rejects. `run_task_elsewhere` refuses it.
- **"Start over on the same machine."** Refused as "already waiting for {label}". The owner
  can remove the worktree and requeue.
- **A board-side push check, or the server running git.** Both are refused by D31 point 6
  and by ADR-0033's alternatives.
- **The doctor's push check for a mapping.** That is 054's. This task reports the push that
  actually failed, at the end of a run.
- **Reconciling a diverged branch, rebasing onto the remote, or fetching before reusing an
  existing worktree.** The postcondition's rejection is the signal, and a person decides.
- **Re-running only the review phase on the new runner.** A released task starts again
  from implementation, on top of the pushed branch, exactly as Run now does (021's Out of
  scope).
- **A new column or migration.** If the implementation finds it needs one (for example, a
  board-side record of the runner a pin left), stop and ask. D28's D4 amendment requires
  that.

## Acceptance criteria

Every git test runs real `git` against a `TempRepo` with a bare `origin` (`with_remote`) in
a `TempDir`. The bare remote is created with `core.logAllRefUpdates=true`, so a test can
count the pushes from its reflog rather than infer them. The agent is `FakeCli` replaying
recorded fixture streams. `FakeCli` gains one mode, `commits_and_pushes_on_attempt`, which
runs `git commit --allow-empty` and then `git push origin HEAD` in the worktree. Clocks are
`TestClock`. Nothing sleeps: a test that has to act between the agent's exit and the push
holds the stream open with `FakeCli::gates`.

**The postcondition, in `crates/core/tests/runner_push.rs` (new):**

- `a_connected_run_whose_agent_pushed_succeeds_without_a_second_push`: the remote reflog for
  the branch has exactly one entry, and the task lands in `in_review`.
- `a_connected_run_that_did_not_push_is_pushed_by_the_runner_and_succeeds`: `origin` has the
  branch at `head_sha`, and the run is `succeeded`.
- `the_runner_push_names_only_the_task_branch`: a new local commit on the default branch, a
  second local branch and a local tag all stay off `origin`.
- `a_diverged_remote_branch_is_never_force_pushed`: `origin`'s branch holds a commit that is
  not in the run's history. The run is `failed` with the step-5 sentence, and `origin`'s
  branch is unchanged.
- `a_push_that_fails_turns_success_into_failed_with_the_error_on_the_card`: `origin` points
  at a path that does not exist. The card's error is the exact step-4 sentence, `exit_class`
  is `fatal`, and the task is `failed` in `ready` with no `resume_after`.
- `a_run_left_off_its_branch_is_not_pushed`: the fake agent checks out a new branch before
  it exits. The step-1 sentence is on the card, and `origin` is unchanged.
- `push_target_refuses_the_default_branch_and_anything_outside_the_namespace`: pure, with
  `main`, a recorded branch equal to the default, `feature/x` and `rimaia/`-prefixed names.
- `a_failed_run_is_not_pushed` and `a_cancelled_run_is_not_pushed`: `origin` is
  unchanged, and the card carries the run's own error, not a postcondition sentence. A
  variant with `origin` pointing at a missing path proves no `ls-remote` ran: the error is
  still the run's own.
- `a_fenced_lease_is_never_pushed_from`: while the fake agent is held at a gate, the test
  advances the task's lease generation through the board. The runner does not push, takes
  the `Conflict` reaction, and `origin` is unchanged.
- `push_environment_never_prompts_and_carries_the_checkout_credential`: pure. It asserts
  the exact removals and additions, both with and without a credential.
- `a_push_error_is_redacted_before_it_reaches_the_card`: pure, over the detail formatter,
  with the token in a URL and in its base64 header form. The 500-character truncation is
  tested at exactly 500 and at 501.
- **Solo is unchanged:**
  - `a_solo_run_in_a_repository_with_no_remote_succeeds`;
  - `a_solo_run_never_pushes`: with a remote, the unpushed branch stays unpushed. With
    `origin` pointing at a missing path the run still succeeds, which proves nothing ran
    `ls-remote`.
- **Review loop**, through 021's machinery:
  - `an_implementation_that_cannot_be_pushed_never_starts_a_review`: loop on, `origin`
    broken. The implementation is `failed`, and no review row exists.
  - `every_successful_phase_leaves_the_branch_on_origin`: implementation, review with
    findings, fix, clean review. After each succeeded row, `origin` is at that row's
    `head_sha`. The review rows add no reflog entries.
  - `a_fix_that_cannot_be_pushed_lands_in_review_with_the_last_pushed_head`: the fix row is
    `failed` with the step-5 sentence, and 044's `latest_successful_head` equals `origin`'s
    branch.

**The fresh worktree, in `crates/core/tests/worktree.rs`:**

- `a_connected_worktree_for_a_task_pushed_elsewhere_starts_from_the_remote_head`: a second
  clone of the same `origin` plays the new runner.
- `a_connected_worktree_for_a_task_never_pushed_starts_from_its_base`.
- `a_connected_worktree_uses_the_recorded_branch_name_even_when_it_would_collide`.
- `a_local_branch_behind_the_remote_is_moved_forward_not_rewritten`.
- `a_diverged_local_branch_is_refused_and_left_alone`: exact sentence. Both refs are
  unchanged afterwards.
- `a_connected_prepare_that_cannot_fetch_is_refused`: exact sentence. No worktree is
  created.
- Every existing `worktree.rs` test passes unchanged, and each runs with `Recorded`.

**Run elsewhere, as board service tests and contract suite cases:**

- `run_elsewhere_moves_the_pin_advances_the_generation_and_queues_the_task`: from
  `waiting_retry`. Afterwards `pinned_runner_id` is the target, `lease_generation` has gone
  up by one, and `run_state` is `queued`.
- `the_old_holder_is_fenced_on_every_lease_method_after_run_elsewhere`: iterates
  `BoardMethod::ALL`, the way `every_lease_method_refuses_a_stale_generation` does.
- `the_next_attempt_after_run_elsewhere_is_a_new_session_on_the_new_runner`: the claim by
  the target has no `resume`. In the runner test, `FakeCli`'s argv for that attempt carries
  `--session-id` with a new id and no `--resume`, and its stdin is the freshly composed
  prompt, asserted as an exact string.
- `the_runner_a_task_left_is_never_offered_it_again`: runner A lists the task in
  `worktrees` as fenced, and `claim(Next)` gives A nothing, even after B's run succeeds and
  a person requeues the task.
- `a_runner_holding_a_superseded_worktree_is_not_offered_that_task`: A never learned of the
  move (no heartbeat, no restart), and lists the worktree as unfenced.
- `a_claim_never_offers_resume_to_a_runner_that_did_not_record_the_session`.
- Refusals, each asserting its exact sentence and changing nothing:
  - `run_elsewhere_refuses_a_task_that_is_not_pinned`;
  - `run_elsewhere_refuses_while_a_lease_is_live`;
  - `run_elsewhere_refuses_the_runner_it_is_pinned_to`;
  - `run_elsewhere_refuses_an_unpaired_target`;
  - `run_elsewhere_refuses_a_target_without_a_checkout`;
  - `run_elsewhere_refuses_a_target_the_task_is_not_eligible_for`;
  - `run_elsewhere_refuses_another_teams_task_and_runner_as_not_found`.
- `a_reassigned_pinned_task_moves_to_the_new_assignees_runner`: this is 045's stuck case,
  unstuck.
- `in_solo_the_only_target_is_the_pinned_runner_itself`: the targets read returns one
  entry, with a refusal.
- `run_elsewhere_through_mcp_and_through_the_command_are_the_same_act`, in
  `tests/mcp_tools.rs`. `tests/mcp_scope.rs` refuses both tools on the run-scoped route for
  every grant.

**The runner side:**

- `a_conflict_after_a_move_fences_the_worktree_and_keeps_it`: the worktree directory, the
  local branch and its commits all remain. `fenced_at` is set, and `origin` is unchanged.
- `startup_fences_worktrees_whose_tasks_moved_while_the_runner_was_off`.
- `run_now_on_a_left_behind_worktree_is_refused_with_a_sentence`: exact sentence, and no
  claim is made.
- `removing_a_fenced_worktree_lets_the_runner_take_the_task_again_from_origin`.
- `solo_never_fences_anything`: a solo runner through a full cycle (fail, retry, succeed,
  requeue, run again) never sets `fenced_at` and never skips a task.

**Unpairing:**

- `unpairing_a_runner_releases_its_pins_and_queues_its_waiting_tasks`: generation advanced,
  pin `NULL`, one change event per team.
- `unpairing_a_runner_closes_its_live_leases_as_interrupted_without_pinning`.
- `expiry_never_pins_to_an_unpaired_runner`, using a faked clock advanced past the lease's
  lifetime.
- `deleting_an_account_releases_its_runners_pins_like_unpairing`.

**Everything else:**

- `./scripts/check-command-wiring.sh` passes with the two new registry rows. The 31 frontend
  test files that mock `@tauri-apps/api/core`, and 049's HTTP mock, pass unchanged.
- The D31 amendment and the CLAUDE.md lines from Scope 7 exist.
- Both `.sqlx` caches are regenerated with D33's recipe, and no migration file is added or
  edited.
- Every CI check passes, exactly as CLAUDE.md lists them, on all three operating systems.
- **Needs a person; the PR body carries it as a checklist.** With 058's headless runner not
  yet built, use two solo-built binaries pointed at a local `rimaia-server` (052) against a
  scratch GitHub repository:
  - run a task whose plan tells the agent not to push, and see the branch appear on GitHub
    at the recorded `head_sha`;
  - revoke push access, run again, and read the push error on the card;
  - put a laptop runner to sleep mid-run, wait past its lease, move the task to the other
    runner, wake the laptop, and confirm that its worktree is kept and fenced and that
    nothing new reached GitHub from it.

## Notes

**Read first.**

- ADR-0033 point 4 (the postcondition), point 5 (why `head_sha` has to be on the remote)
  and point 6 (credentials on the runner).
- ADR-0031 point 4 (pins, "run this elsewhere", unpairing) and point 5 (per-runner
  recovery).
- ADR-0012 points 3 and 4 (never forced, never the default branch). The runner's push is
  held to the rules the agent is held to.

Then these seam entries:

- **D31**: points 3 (generation on every method), 4 (`claim`, `preview`, `run_context`), 5
  (the starter preflight), 6 (057's bullet, which this task amends), 11 (the one `Conflict`
  reaction, owned by 053) and 13 (the contract suite).
- **D28**: `runner_leases`, `tasks.lease_generation`, `tasks.pinned_runner_id`,
  `runs.runner_id`, `runners.unpaired_at`, `runner_repositories`, and the runner set's
  `worktrees.fenced_at`. Its D4 amendment is a prohibition here: there is no migration for
  this task.
- **D29**: points 4 and 5 ("newest" versus "latest successful").
- **D9**, with its 2026-09-03 amendment: where an interrupted task lands.
- **D23** point 7: a requeued task starts a new session.
- **D32**: the registry, and point 9's parity note.
- **D30**: the run surface the new tools are refused on.
- **D33**: both caches.
- **D8**: no new error code. `Invalid`, `NotFound` and 043's `Conflict` cover everything.
- **D20**: the removal guards a fenced worktree still passes through.

**Files to start from.** On `main` today:

- `crates/core/src/runner/process.rs`: `run_task`, `override_as_fatal`, `RunnerConfig`.
- `crates/core/src/worktree/mod.rs`: `prepare`, `resolve_branch`, `existing_worktree`.
- `crates/core/src/worktree/git.rs`: `run`, `checked`, `fetch_prune`, `branch_exists`,
  `commit_exists`.
- `crates/core/src/worktree/naming.rs`: `BRANCH_NAMESPACE`.
- `crates/core/src/credentials/inject.rs` and `crates/core/src/credentials/redact.rs`.
- `crates/core/src/tasks/run_state.rs`: the edges this task uses, and does not add to.
- `crates/core/src/mcp/scope.rs` and `crates/core/src/mcp/server.rs`: `Tool`,
  `Tool::run_access`, and `give_up_on_task` as the pattern.
- `crates/core/src/testing/repo.rs` (`TempRepo::with_remote`) and
  `crates/core/src/testing/cli.rs` (`FakeCli::commits_on_attempt`, `gates`).
- `crates/core/tests/runner_process.rs`, `crates/core/tests/worktree.rs`,
  `crates/core/tests/mcp_tools.rs` and `crates/core/tests/mcp_scope.rs`.
- `src-tauri/src/lib.rs`, `src/lib/commands.ts` and `src/types.ts`.

Created by earlier tasks in this chain:

- `crates/core/src/board/` (036, 043, 044): the port, `board::service`, and the claim
  transaction and selection.
- `crates/core/src/testing/board_contract.rs` (036) and
  `crates/runner/tests/board_port_http.rs` (052).
- `crates/core/src/api/registry.rs` and `crates/core/src/api/board/` (046).
- `crates/core/src/identity/` (047): `unpair_runner`.
- `crates/runner/`: `runner.db`'s `worktrees` (041), the runner loop (042), the per-runner
  reconcile (043), the `Conflict` reaction (053), and the checkout mapping and credentials
  (054).
- `crates/core/src/worktree/bundle.rs` (033): `capture`, which runs first.

**Migration.** None. D28 already carries every column this task writes:
`tasks.pinned_runner_id` and `tasks.lease_generation` (043's file), and
`worktrees.fenced_at` (041's file).

**What the chain provides.**

- 033: `head_sha` recorded at every finish, and `capture` before `finish_run`.
- 021: the loop, `NextStep::Continue`, and its exit table.
- 036 and 052: the port, both adapters, and the contract suite.
- 043: leases, generations, pins, the in-transaction `set_run_state`, and `Conflict`.
- 044: `RunContext::base` and `latest_successful_head`.
- 045: `eligibility::decide` and its reason sentences.
- 047: `unpair_runner`.
- 053: expiry and the `Conflict` reaction.
- 054: `origin` verified for every mapping, and credentials keyed by (repository, runner).

If any of these is not where this file says, or 043 writes pins in more than one place,
stop and ask rather than build a second copy.

**What the next tasks expect.**

- **058 and 059** set `branch_postcondition: OnOrigin` where they build `HttpBoard`, and
  send `worktrees` on every `claim(Next)`.
- **059**'s connected desktop runs `prepare` with `OnOrigin`. Its required fetch is what
  brings a teammate's commits into the local clone.
- **061** draws the pinned card, the target picker from `list_run_elsewhere_targets`, the
  fenced-worktree notice from the inventory's `fencedAt`, and the push error.
- **064** checks the CLAUDE.md lines against CI.

**The known edges, accepted.**

- **The agent can push before its fence.** A laptop that wakes with its agent still running
  can push before its first heartbeat is refused. The push is never forced (ADR-0012 point
  3), so at worst it fast-forwards `origin`. If the new runner has already started from the
  older head, its own postcondition push is then rejected as a non-fast-forward. It fails
  with the step-5 sentence, and a person decides. Nothing is overwritten.
- **Unpushed commits stay behind.** A pinned run's commits that were never pushed stay in
  the fenced worktree. The new runner starts from `origin`, which is why the worktree is
  kept and why 061's copy has to say so.
- **A released `waiting_retry` task's last row still has a `resume_after`.** Its `run_state`
  is `queued`, and every reader that acts on `resume_after` gates on `waiting_retry` (D12's
  amendment, `reconcile::has_scheduled_resume`). Check this while implementing, and add a
  test if any reader does not.

**Size.** About 3,000 to 3,500 lines of diff, most of them git-backed tests. The production
code is two small modules (`remote.rs`, `postcondition.rs`), one `prepare` branch, one
board function with two doors, a claim filter, and three call sites on the runner side. If
it runs past 4,000 lines, stop after sections 1–4 and 6 and ask for section 5's
`ClaimTarget::Next` filter and the runner-side fencing to become their own task (066). The
known edge that section 5 then leaves open is the one its filter bullet names. It fails at
the postcondition, loudly, and never silently.
