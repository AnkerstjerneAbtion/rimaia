---
id: "057"
title: Push postcondition, and run elsewhere
milestone: v0.5
status: ready
depends_on: ["053", "054", "056"]
adrs: ["0033", "0031", "0012"]
size: L
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
it and is not offered the task again while it holds it. Unpairing a runner (task 047), and
removing a member from a team (task 051), release the pins they strand the same way, with no
target.

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
- **The review loop (task 021): every successful phase.** This is ADR-0033's 2026-10-04
  amendment to point 4, which carries the reasoning: the runner decides before `finish_run`,
  and only `finish_run`'s answer says whether a phase was the last. An implementation whose
  branch cannot be pushed is `failed`, and the loop never starts.

### 2. The check and the push

These live in two files:

- **`crates/core/src/worktree/remote.rs`** (new): the git calls, as argument vectors, with
  no `sh -c`. `worktree::git::run` and `checked` take neither an environment nor a cancel
  token, so `git.rs` gains one variant that takes a `ChildEnvironment`, the run's
  `CancelSignal` and a timeout, and stops the process group through
  `runner::process::signal_group` on either. Only `remote.rs` calls it. If 054's push check
  already added such a runner, this task reuses it.
- **`crates/core/src/runner/postcondition.rs`** (new): the decision, as a function that
  takes the outcome by `&mut` and rewrites it the way `override_as_fatal` does.

It runs after `worktree::bundle::capture` (033) and before `finish_run`. In a connected
runner that is before `OutboxBoard::finish_run` (056), so the outcome the outbox queues is
already the rewritten one. In order:

1. **The worktree is on the task branch.** `git symbolic-ref -q HEAD` must name
   `refs/heads/<branch>`, where `<branch>` is the task's recorded `tasks.branch`. A
   detached `HEAD`, or an agent that switched to another branch, fails the check. The
   runner does not push a commit the task branch does not point at.
2. **The branch is a task branch.** It must start with `worktree::naming`'s
   `BRANCH_NAMESPACE` and must not equal the repository's default branch. This check is a
   pure function, `push_target(branch, default_branch) -> Result<&str>`. The recorded name
   comes from the board, and ADR-0003 counts the sqlite3 CLI as a writer.
3. **`origin` is still the team's repository.** 054 verified the clone's `origin` before the
   spawn, but the agent ran with `bypassPermissions` in a worktree that shares the clone's
   config, and could have re-pointed it. The runner reads `git remote get-url --all origin`
   and `git remote get-url --push --all origin`. Git expands `pushurl`, `insteadOf` and
   `pushInsteadOf` in these answers, so they are the URLs the next commands will really
   use. Every one must normalise, through 054's `NormalizedRemote::parse`, to
   `RunContext::repository.remote`. Otherwise nothing is pushed.
4. **The lease is still current.** The runner calls `heartbeat(&[lease])`. A lease listed in
   `fenced` means no push: the runner takes D31 point 11's single reaction (053's
   `on_fenced`) and never reaches step 5. The probe is the heartbeat and not `run_context`,
   because 045 makes `run_context` answer `Conflict`, and end the lease, whenever
   consent-gated content changed. A plan a teammate edited mid-run would then throw away a
   finished, successful run. The heartbeat's `fenced` list answers on the generation only,
   and renewing the lease is harmless.
   - **A transport error is not an answer.** The probe retries with the backoff 056's
     sender uses, holding the slot and the worktree, exactly as `OutboxBoard::finish_run`
     would wait to deliver. If the lease expires meanwhile, the board's eventual answer is
     `fenced`, and the run ends as any run that finished during an outage ends (053).
   - **Any other error** also means no push, and the outcome becomes `failed` with the
     step-4 sentence below.

   This shrinks the window in which a fenced runner could push from minutes to one round
   trip. It cannot close it: the agent's own push, while it is still running, is out of
   Rimaia's hands. See Notes.
5. **`git ls-remote --heads origin refs/heads/<branch>`.** If it answers `head_sha`, the
   check passes, and nothing is pushed.
6. **Otherwise, push:**
   `git push --porcelain origin refs/heads/<branch>:refs/heads/<branch>`, run in the
   worktree. The rules for this command:
   - No `--force`, no `--force-with-lease`, no `+`, no `--mirror`, no `--all`, no
     `--tags`.
   - No `--no-verify`. A repository's pre-push hook is the repository's rule.
   - A remote branch that is not an ancestor of `head_sha` is rejected by git as a
     non-fast-forward. That rejection is the failure. The runner never reconciles a
     diverged branch.
   - It is bounded by an injected `PUSH_TIMEOUT` (10 minutes in production), scoped like
     054's `PUSH_CHECK_TIMEOUT`: injected, never read at the call site. A real push carries
     the branch's objects, so it gets longer than the dry run's 30 seconds. On expiry the
     process group is stopped and the step fails with the detail `git did not finish within
     10 minutes`.
7. **Verify, do not trust.** `ls-remote` runs again, and it must now answer `head_sha`.

The branch's upstream configuration is never consulted.

**No push preflight per run.** 054's handoff names `RemotePush` as this postcondition's
preflight, and it is one at queue start only. On a connected runner a failed push check is
a doctor **fail**, which blocks `QueueHandle::start` (D22 points 1 and 3), so a queue does
not start against a repository it cannot push to. It does not keep a task away from that
runner. 054 has no `serving` flag: a push error is a per-runner state that counts only when
`connected` is true, and the runner stays in `list_repository_runners` and in
`ClaimTarget::Next.repositories`. So a run can still reach its end with a push that will
fail: Run now and Retry now do not run the doctor, a run the board relays through 052's
`start_task_run` arrives without one, and a queue that started cleanly keeps claiming after
a credential expires or a forge revokes access in the night. The postcondition is the
end-of-run check for all of them, and reports the push that actually failed. A dry run
before every spawn would add a round trip to `origin` per run and still could not close the
gap, because the credential can change while the agent works.

**The environment and the redaction are 054's, not a second copy.** 054's push check builds
its environment from `credentials::inject::child_environment` for the (repository, runner)
credential, plus `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=Never`, and
`GIT_SSH_COMMAND=ssh -o BatchMode=yes` when the parent sets none. It redacts git's stderr with the credential's
`Redactor` (D25 point 5), a `scheme://userinfo@` → `***` rule, the checkout path replaced
with `<clone>`, and a 1,024-byte cap. Both become shared functions in `worktree/remote.rs`
if 054 left them inside the doctor check, and the postcondition calls them for every git
command in steps 3 and 5 to 7, passing the worktree path as the path to replace. Removing
`GIT_ASKPASS` and `SSH_ASKPASS` goes into the shared builder, so the doctor's check gains it
too and 054's exact-environment test is updated with it. Holding one rule in two places is
the defect ADR-0006 names.

**Cancellation.** A Cancel that arrives during steps 3 to 7 kills the git child through the
run's cancel token, and the outcome becomes `cancelled`, as it would have been while the
agent was running.

**What reaches the card.** The exact strings, where `{detail}` is git's stderr after 054's
redaction and `{short}` is the 12-character abbreviation of a commit:

| Case | `error_message` |
| --- | --- |
| step 1 | `The run finished with its worktree on {where}, not on {branch}. Rimaia pushes only the task's own branch, so nothing was pushed.` where `{where}` is `a detached HEAD` or `branch {name}` |
| step 2 | `Rimaia pushes only a task's own rimaia/ branch, and {branch} is not one, so nothing was pushed.` |
| step 3 | `The run finished with origin pointing at {found}, not at {expected}, so nothing was pushed. Point origin back at {expected}, then run the task again.` where `{found}` is the first non-matching URL's normalised form, or `a URL Rimaia does not recognise` |
| step 4, not fenced | `The run finished, but Rimaia could not confirm with the board that this runner still holds the task, so nothing was pushed. {detail}` |
| step 5 or 7, `ls-remote` failed | `The run finished, but Rimaia could not read origin to check for {branch}. git said: {detail}` |
| step 6 failed | `The run finished, but {branch} could not be pushed to origin, so nobody else can reach its work. git said: {detail}` |
| step 7 disagrees | `The run finished, but after the push origin has {branch} at {remote_short}, not at {head_short}.` |

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

**One function releases a pin, and every path that releases one goes through it.**
`release_pin(tx, clock, task_id, target: Option<RunnerId>) -> ReleasedPin` lives in the
module where 043 writes `tasks.pinned_runner_id`, and runs inside its caller's transaction.
It does these things, and nothing else:

- sets `pinned_runner_id` to `target`, which is `NULL` when `target` is `None`;
- increments `tasks.lease_generation`, so every `LeaseRef` the old holder still has is
  refused as `Conflict` by every lease method (D31 point 3). That includes a lease whose
  expiry the board has already processed;
- takes `waiting_retry` to `failed`, then `queued`: the pinned attempt was going to run
  again anyway, and now runs as a new one. Every other state is left alone.

`run_task_elsewhere` additionally takes `failed` and `cancelled` to `queued`, because a
person asked for the task to run. Unpairing and member removal do not: a task that had
stopped is not restarted by a machine leaving. All three edges exist in
`is_legal_run_state_transition`, and are written through the in-transaction form of
`set_run_state` that 043 added for the one-transaction claim. The run-state table does not
change. A queued task is claimed as a new attempt with no `resume` (D23 point 7's "a task
the user re-queued … starts a new session").

**`run_task_elsewhere(ctx, task_id, runner_id)`**, the person's door, in one transaction:

- **Who may call it:** any member of the task's team, owner or member. Moving a task is an
  edit of the task, which ADR-0029 point 3 gives every member, and the target must pass the
  eligibility a claim would apply (045), so the command cannot send work to a runner that
  could not otherwise take it. A caller outside the team gets `NotFound`, below.
- **Refuses** each of the following as `Invalid`, with these exact sentences:
  - the task is not in `ready`: `"{title}" is in {column}. Only a task in Ready can be
    moved to another runner.`
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
- **Refuses a task in another team, and a target runner in another team, as `NotFound`**,
  worded as for one that does not exist (039's rule, ADR-0029 point 5).
- **Otherwise** calls `release_pin` with `Some(runner_id)`, requeues a `failed` or
  `cancelled` task, then publishes `ChangeEvent::tasks` for the task's team after the
  commit.
- **Consent is not checked here.** The claim checks it (045), and the card shows what is
  missing. Moving a task to a runner whose owner has not yet consented is a legitimate step
  towards running it there.

**Why a target is required.** With a pin cleared to `NULL`, the runner it left would be as
eligible as any other, and would usually claim first, because it is awake and long-polling.
D28 has no column that could say "anyone but that one", and its D4 amendment forbids adding
one without an amendment. Naming the target uses the one column that exists, and tells the
owner of the runner the task left where it went.

**`list_run_elsewhere_targets(ctx, task_id) -> Vec<RunElsewhereTarget>`** (Read). The
candidates are exactly the runners with `unpaired_at IS NULL` whose owner
(`runners.user_id`) has a `team_memberships` row for the task's team, the pinned runner
included, whatever the runner's eligibility policy and whether or not it has a
`runner_pool_teams` row. Eligibility is a refusal on an entry, never a filter on the list.
The runner a pinned task most often has to go to is the new assignee's, and a runner on
045's default `assigned` policy has no pool row at all. A list built from pool rows would
never offer it, and 045's reassigned, pinned task, "claimable by nobody until run
elsewhere", could not be unstuck from 069's picker. Each entry carries `runnerId`, `label`,
the owner's `login`, `lastSeenAt`, and `refusal: Option<String>`: the sentence
`run_task_elsewhere` would answer for that target, or `None` where it would succeed. The
list is ordered by `label`, then `runnerId`, so a picker and a test read it the same way
twice. In a personal team served in process there is one member with one runner, the
pinned one, so the list holds that entry with its `already waiting for` refusal and offers
nowhere to go. That is how solo stays unchanged without a special case. Both functions are
`pub` in `rimaia-core`, with no path in any DTO (ADR-0028 point 2's test covers them).

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
- **Frontend.** `src/lib/commands.ts` gains two `board<T>` wrappers, and `src/types.ts`
  the `RunElsewhereTarget` DTO. No component changes. 061 draws the card and the target
  picker.
- **Fixtures.** 028's rule is that every command `commands.ts` sends has a row in the
  fixture table in `src/dev/fixtures/`, so this task adds both. `list_run_elsewhere_targets`
  answers the solo shape: one entry, the pinned runner, with its exact `already waiting
  for` refusal. `run_task_elsewhere` answers success without changing the seed, as every
  fixture write does. The team scenario's answers, with targets to move to, are 069's,
  which extends these rows rather than adding them.

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
  - **053's `on_fenced`** (D31 point 11). After it stops the process and drops the lease,
    it fences the worktree if it is superseded, instead of claiming again through a pin
    that has moved.
  - **043's per-runner startup reconciliation.** It fences every superseded worktree the
    runner holds, so a laptop that was off while its task moved learns of it at launch.
  - **The starter preflight** that D31 point 5 made one `rimaia-core` function, for Run
    now, Retry now and Plan now. A fenced or superseded worktree is refused with `This
    machine's worktree for "{title}" was left behind when the task moved to {label}. It is
    kept for you to look through. Remove it to run the task here.` A superseded one is
    fenced first.

  Each fencing writes one `tracing::warn!` naming the task and the runner it moved to.
  066's `list_local_worktrees` entry gains `fencedAt`, so 061 can tell the owner.
- **The board never hands a runner a task it holds a stale worktree for.**
  `ClaimTarget::Next` gains `#[serde(default)] worktrees: Vec<HeldWorktree { task_id,
  fenced: bool }>`, built from `runner.db`'s `worktrees` table on every request. The board's
  selection skips a task for this runner when it is listed and either `fenced` is true or
  the `superseded` rule holds, which it evaluates in SQL. This closes the one gap the three
  places above leave: a runner that stays awake and idle through a move, and is then offered
  the same task after the other runner has finished with it. Notes says why the skip is not
  a `SkipReason`. The field is `serde(default)` because it is added after 052 (D31 point 6),
  and an older runner that omits it is only as protected as the three places above make it.
- **`resume` goes only to the runner that recorded the session.** The board sets
  `Claim::resume` only when the claiming runner is the one on the newest row. If 043
  already enforces this, this task adds the contract case and nothing else. Resuming a
  session on a machine that does not have it is the seam bug ADR-0026 point 6 warns about.

### 6. Unpairing and member removal release pins

`identity::unpair_runner` (047) gains, in its existing transaction:

- **Every live `runner_leases` row of that runner** is ended through 053's
  `expire_in(conn, clock, task_id, generation, pin: false)`, which takes the transaction's
  connection, never commits and never publishes. A lease with a run closes it `interrupted`
  and lands the task as expiry does, without the pin. A lease with no run, a strategy lease
  included, goes where `release` sends it. The runner's token is being deleted in the same
  transaction, so its heartbeat could never renew the lease, and waiting for expiry would
  only pin the task to a runner that cannot come back.
- **`release_pin(tx, clock, task_id, None)`** for every task pinned to it.

It returns the released task ids with their teams, and its caller publishes one
`ChangeEvent::tasks` per affected team after the commit. 047's function takes a connection,
not a context, and stays that way.

**051's member removal** (`remove_member` and `leave_team`, its Scope 4) clears the pins
on that team's tasks that name the member's runners with a direct `pinned_runner_id =
NULL`. This task replaces that with `release_pin(.., None)` for each, in 051's transaction.
The direct write advances no generation and leaves `waiting_retry` alone, so the
ex-member's runner keeps a usable lease ref, and a waiting task is left for any runner to
resume: the cross-machine resume ADR-0026 point 6 warns about. 051's lease expiry stays as
it is, and 053's membership guard keeps the sweep from pinning the task back.

**Every path that unpairs goes through `unpair_runner`.** That is 047's `revoke_api_token`
for a runner token, and 051's account deletion, which removes the account's runners. If 051
deletes `runners` rows directly, this task routes it through `unpair_runner` first.
`ON DELETE SET NULL` would clear a pin without advancing the generation or requeueing the
task. 053's pin decision gains the guard it names as this task's, beside its membership
check: it never pins a task to a runner with `unpaired_at` set.

### 7. Contract, CLAUDE.md, D31 and the caches

- **The board contract suite** (`crates/core/src/testing/board_contract.rs`) gains this
  task's cases, listed under Acceptance criteria. They run through both adapters (036's
  in-process one, 052's HTTP one).
- **D31 is amended in the same PR** with one dated paragraph: `ClaimTarget::Next`'s
  `worktrees` field and the skip rule, and the `resume` rule. The every-phase rule is
  already ADR-0033's amendment, and needs no seam entry of its own.
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
- **The doctor's push check for a mapping.** That is 054's.
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
recorded fixture streams. `FakeCli` gains one mode, `runs_git_on_attempt(attempt,
commands)`, which runs each git argument vector in the worktree after the stream's last
event; "commits and pushes" is `commit --allow-empty` then `push origin HEAD`. Clocks are
`TestClock`. Nothing sleeps: a test that has to act between the agent's exit and the push
holds the stream open with `FakeCli::gates`.

Two set-ups recur. **"The bare remote moved away"** means its directory is renamed while the
agent is held at a gate, so `origin`'s URL is unchanged and unreachable. **Where a case
needs the old holder's lease to have ended**, the board is built with
`LeaseTerm::Renewable(LEASE_LIFETIME)`, as 053's suite does, and the `TestClock` is advanced
past `expires_at`. `run_task_elsewhere` refuses while any lease row exists, so no case moves
a pin under a live one.

**The postcondition, in `crates/core/tests/runner_push.rs` (new):**

- `a_connected_run_whose_agent_pushed_succeeds_without_a_second_push`: the remote reflog for
  the branch has exactly one entry, and the task lands in `in_review`.
- `a_connected_run_that_did_not_push_is_pushed_by_the_runner_and_succeeds`: `origin` has the
  branch at `head_sha`, and the run is `succeeded`.
- `the_runner_push_names_only_the_task_branch`: a new local commit on the default branch, a
  second local branch and a local tag all stay off `origin`.
- `a_diverged_remote_branch_is_never_force_pushed`: `origin`'s branch holds a commit that is
  not in the run's history. The run is `failed` with the step-6 sentence, and `origin`'s
  branch is unchanged.
- `a_rejected_push_turns_success_into_failed_with_gits_reason`: the bare remote has a
  `pre-receive` hook that exits 1. `ls-remote` succeeds, the card's error is the exact
  step-6 sentence, and `origin` is unchanged.
- `an_unreachable_origin_turns_success_into_failed_with_the_error_on_the_card`: the bare
  remote moved away. The card's error is the exact step-5 sentence, `exit_class` is
  `fatal`, and the task is `failed` in `ready` with no `resume_after`.
- `a_push_that_does_not_finish_is_stopped_at_its_bound`: the `pre-receive` hook waits on a
  gate, the `TestClock` is advanced past `PUSH_TIMEOUT`, the git process group is stopped,
  and the card carries the step-6 sentence with the timeout detail.
- `a_run_left_off_its_branch_is_not_pushed`: the fake agent checks out a new branch before
  it exits. The step-1 sentence is on the card, and `origin` is unchanged.
- `a_run_whose_agent_repointed_origin_is_not_pushed`: the fake agent runs `git remote
  set-url origin` to a second bare remote. The step-3 sentence is on the card, and neither
  remote changes. Variants set `remote.origin.pushurl` and a `url.<base>.pushInsteadOf`
  instead, with the same result.
- `push_target_refuses_the_default_branch_and_anything_outside_the_namespace`: pure, with
  `main`, a recorded branch equal to the default, `feature/x` and `rimaia/`-prefixed names.
- `a_failed_run_is_not_pushed` and `a_cancelled_run_is_not_pushed`: `origin` is
  unchanged, and the card carries the run's own error, not a postcondition sentence. A
  variant with the bare remote moved away proves no `ls-remote` ran: the error is still the
  run's own.
- `a_fenced_lease_is_never_pushed_from`: while the fake agent is held at a gate, the clock
  passes the lease's expiry and a person moves the task to runner B with
  `run_task_elsewhere`. On release the runner's probe finds its lease in `fenced`, takes
  `on_fenced`, and `origin` is unchanged. A variant removes the runner's owner from the
  team (051) instead.
- `a_plan_edited_during_a_connected_run_is_still_pushed_and_finished`: a teammate edits the
  plan while the agent is held at a gate. The run succeeds, is pushed, `finish_run` is sent,
  and the task lands in `in_review`.
- `a_lease_check_the_board_cannot_answer_yet_is_retried_not_failed`: a port wrapper fails
  the first two heartbeats with a transport error. The `TestClock` is advanced through the
  backoff, the third answers, and the run is pushed and succeeds.
- `the_runner_push_uses_the_push_checks_environment`: pure. For an HTTPS credential, no
  credential, and an SSH remote with and without a parent `GIT_SSH_COMMAND`, the push's
  environment equals 054's shared builder's, including `BatchMode`, `GCM_INTERACTIVE`,
  `GIT_TERMINAL_PROMPT` and the removed `ASKPASS` variables.
- `a_push_error_reaches_the_card_through_the_push_checks_redaction`: stderr carrying the
  injected token raw and in its base64 header form, a user-configured
  `https://user:token@…` URL and the worktree path. The card's `{detail}` equals 054's
  shared redaction function's output, exactly.
- **Solo is unchanged:**
  - `a_solo_run_in_a_repository_with_no_remote_succeeds`;
  - `a_solo_run_never_pushes`: with a remote, the unpushed branch stays unpushed. With the
    bare remote moved away the run still succeeds, which proves nothing ran `ls-remote`.
- **Review loop**, through 021's machinery:
  - `an_implementation_that_cannot_be_pushed_never_starts_a_review`: loop on, bare remote
    moved away. The implementation is `failed`, and no review row exists.
  - `every_successful_phase_leaves_the_branch_on_origin`: implementation, review with
    findings, fix, clean review. After each succeeded row, `origin` is at that row's
    `head_sha`. The review rows add no reflog entries.
  - `a_fix_that_cannot_be_pushed_lands_in_review_with_the_last_pushed_head`: the fix row is
    `failed` with the step-6 sentence, and 044's `latest_successful_head` equals `origin`'s
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
- `run_elsewhere_requeues_a_failed_or_cancelled_pinned_task`: one case each.
- `any_member_of_the_team_may_run_a_task_elsewhere`: the caller is a `member` who is
  neither the assignee nor the owner of either runner.
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
  - `run_elsewhere_refuses_a_task_outside_ready`;
  - `run_elsewhere_refuses_a_task_that_is_not_pinned`;
  - `run_elsewhere_refuses_while_a_lease_is_live`;
  - `run_elsewhere_refuses_the_runner_it_is_pinned_to`;
  - `run_elsewhere_refuses_an_unpaired_target`;
  - `run_elsewhere_refuses_a_target_without_a_checkout`;
  - `run_elsewhere_refuses_a_target_the_task_is_not_eligible_for`;
  - `run_elsewhere_refuses_another_teams_task_and_runner_as_not_found`, including a caller
    who is not a member of the task's team.
- `a_reassigned_pinned_task_moves_to_the_new_assignees_runner`: this is 045's stuck case,
  unstuck. The new assignee's runner is on the default `assigned` policy with no
  `runner_pool_teams` row. It appears in `list_run_elsewhere_targets` with `refusal: None`,
  and `run_task_elsewhere` to it succeeds.
- `the_targets_are_the_paired_runners_of_the_teams_members_each_with_its_refusal`: an
  unpaired runner, and a runner whose owner is not a member of the task's team, are absent.
  Present, in `label` order: a member's runner on the `assigned` policy with no pool row,
  with `refusal: None`; the pinned runner, a runner with no checkout of the repository and
  a runner the task is not eligible for, each with its exact refusal.
- `in_solo_the_only_target_is_the_pinned_runner_itself`: the targets read returns one
  entry, the solo runner, with the exact `already waiting for` refusal.
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

**Unpairing and member removal:**

- `unpairing_a_runner_releases_its_pins_and_queues_its_waiting_tasks`: generation advanced,
  pin `NULL`, one change event per team. A pinned `failed` task is unpinned and stays
  `failed`.
- `unpairing_a_runner_closes_its_live_leases_as_interrupted_without_pinning`, with one
  strategy lease, which is deleted.
- `removing_a_member_releases_their_runners_pins_and_queues_their_waiting_tasks`: the
  generation advances, the old holder's lease ref is refused, and the waiting task is
  `queued`. `leave_team` behaves the same.
- `expiry_never_pins_to_an_unpaired_runner`, using a faked clock advanced past the lease's
  lifetime.
- `deleting_an_account_releases_its_runners_pins_like_unpairing`.

**Everything else:**

- `./scripts/check-command-wiring.sh` passes with the two new registry rows. The 31 frontend
  test files that mock `@tauri-apps/api/core`, and 049's HTTP mock, pass unchanged.
- 028's fixture coverage test passes, with rows for `run_task_elsewhere` and
  `list_run_elsewhere_targets` answering as Scope 4's Fixtures bullet says, and 028's bundle
  test still finds no fixture in the production build.
- The D31 amendment and the CLAUDE.md lines from Scope 7 exist.
- Both `.sqlx` caches are regenerated with D33's recipe, and no migration file is added or
  edited.
- Every CI check passes, exactly as CLAUDE.md lists them, on all three operating systems.
- Nothing here needs a person. The checks against a real forge need a production host for
  `HttpBoard`, which 058 builds, so they are in 058's hand-checked list.

## Notes

**Read first.**

- ADR-0033 point 4 (the postcondition) and its 2026-10-04 amendment (every successful
  phase), point 5 (why `head_sha` has to be on the remote) and point 6 (credentials on the
  runner).
- ADR-0031 point 4 (pins, "run this elsewhere", unpairing) and point 5 (per-runner
  recovery).
- ADR-0012 points 3 and 4 (never forced, never the default branch). The runner's push is
  held to the rules the agent is held to.

Then these seam entries:

- **D31**: points 3 (generation on every method), 4 (`claim`, `preview`, `heartbeat`), 5
  (the starter preflight), 6 (057's bullet), 11 (the one fence reaction, owned by 053) and
  13 (the contract suite).
- **D28**: `runner_leases`, `tasks.lease_generation`, `tasks.pinned_runner_id`,
  `runs.runner_id`, `runners.unpaired_at`, `runner_pool_teams`, `runner_repositories`, and
  the runner set's `worktrees.fenced_at`. Its D4 amendment is a prohibition here: there is
  no migration for this task.
- **D29**: points 4 and 5 ("newest" versus "latest successful").
- **D25** points 3 to 5: the injected git environment, and redaction over exactly what was
  injected. The push reuses both through 054's shared functions.
- **D9**, with its 2026-09-03 amendment: where an interrupted task lands.
- **D23** point 7: a requeued task starts a new session.
- **D32**: the registry, and point 9's parity note.
- **D30**: the run surface the new tools are refused on.
- **D33**: both caches.
- **D8**: no new error code. `Invalid`, `NotFound` and 043's `Conflict` cover everything.
- **D20**: the removal guards a fenced worktree still passes through.

**Files to start from.** On `main` today:

- `crates/core/src/runner/process.rs`: `run_task`, `override_as_fatal`, `RunnerConfig`,
  `signal_group`.
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

- `crates/core/src/board/` (036, 043, 044, 053): the port, `board::service`, the claim
  transaction and selection, and `lease.rs`'s `expire_in`.
- `crates/core/src/testing/board_contract.rs` (036) and
  `crates/runner/tests/board_port_http.rs` (052).
- `crates/core/src/api/registry.rs` and `crates/core/src/api/board/` (046).
- `crates/core/src/identity/` (047): `unpair_runner`. 051's `remove_member` and
  `leave_team`.
- `crates/core/src/repo/remote.rs` (054): `NormalizedRemote`. 054's push check: its
  environment and redaction.
- `crates/runner/`: `runner.db`'s `worktrees` (041), the runner loop (042), the per-runner
  reconcile (043), `on_fenced` (053), the checkout mapping and credentials (054), and
  `OutboxBoard` (056).
- `crates/core/src/worktree/bundle.rs` (033): `capture`, which runs first.

If any of these is not where this file says, or 043 writes pins in more than one place,
stop and ask rather than build a second copy.

**What the next tasks expect.**

- **058 and 059** set `branch_postcondition: OnOrigin` where they build `HttpBoard`, and
  send `worktrees` on every `claim(Next)`. 058's hand-checked list carries this task's
  checks against a real forge.
- **059**'s connected desktop runs `prepare` with `OnOrigin`. Its required fetch is what
  brings a teammate's commits into the local clone.
- **061** draws the pinned card, the target picker from `list_run_elsewhere_targets`, the
  fenced-worktree notice from `fencedAt`, and the push error.
- **064** checks the CLAUDE.md lines against CI.

**The known edges, accepted.**

- **The agent can push before its fence.** A laptop that wakes with its agent still running
  can push before its first heartbeat is refused. The push is never forced (ADR-0012 point
  3), so at worst it fast-forwards `origin`. If the new runner has already started from the
  older head, its own postcondition push is then rejected as a non-fast-forward. It fails
  with the step-6 sentence, and a person decides. Nothing is overwritten.
- **Unpushed commits stay behind.** A pinned run's commits that were never pushed stay in
  the fenced worktree. The new runner starts from `origin`, which is why the worktree is
  kept and why 061's copy has to say so.
- **An unreachable remote fails the run; an unreachable board does not.** Step 4 waits for
  the board, because `finish_run` has to reach it anyway. Steps 5 to 7 do not wait for
  `origin`: git exits 128 for an unreachable host and for refused credentials alike, so a
  retry loop could not tell an outage from a credential that will never work, and would
  hold the slot all night. ADR-0033 point 4 makes a branch that cannot be pushed `failed`,
  and the commits stay in the worktree. A requeue on the same runner reuses it, and that
  run's postcondition pushes them.
- **The stale-worktree skip is not a `SkipReason`.** ADR-0032 says a task is never silently
  skipped. This skip is per runner: it applies only to the runner a task moved away from,
  while it keeps the left-behind worktree. The task stays claimable by its pinned target
  and by every other runner, and the card already says which runner it waits for. The one
  person who could be surprised, that runner's owner, sees the fenced notice 061 draws from
  `fencedAt`, with the remedy in it.
- **A released `waiting_retry` task's last row still has a `resume_after`.** Its `run_state`
  is `queued`, and every reader that acts on `resume_after` gates on `waiting_retry` (D12's
  amendment, `reconcile::has_scheduled_resume`). Check this while implementing, and add a
  test if any reader does not.

**Size.** About 3,000 to 3,500 lines of diff, most of them git-backed tests. The production
code is two small modules (`remote.rs`, `postcondition.rs`), one `prepare` branch, one
board function with two doors, a claim filter, and three call sites on the runner side. If
it runs past 4,000 lines, stop after sections 1–4 and 6 and ask for section 5's
`ClaimTarget::Next` filter and the runner-side fencing to become their own task, with the
next free number. The known edge that section 5 then leaves open is the one its filter
bullet names. It fails at the postcondition, loudly, and never silently.
