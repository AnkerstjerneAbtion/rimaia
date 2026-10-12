# 33. Repositories belong to the team; checkouts, branches and credentials belong to the runner

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

Today a repository *is* a local path. Registering one (task 003) validates a directory on
disk, and `repositories.path` is how every other part of Rimaia finds it. Worktrees live
under the data directory (ADR-0005), and their paths are on `tasks.worktree_path`. Credentials
are keychain items keyed by repository id (ADR-0020). Branch chaining resolves a dependent
task's base to a **local** branch name. `worktree/base_ref.rs` is explicit about it:

> A **local** branch name, never `origin/<branch>` … a dependency branch only exists locally
> until somebody pushes it.

That rule was correct when one machine ran every task. On a team, dependency A may run on
Alice's laptop and its dependent B on Bob's. B's runner has never seen A's local branch. The
remote is the only place the two machines share.

Review also assumed a local git working tree. ADR-0013 puts "the git diff summary … the
commits made on the branch" first in the run view, computed by running git against the
worktree. A browser, and a teammate's desktop app, have no such worktree.

## Decision

### 1. A team repository is identified by its remote

The server's `repositories` row describes the repository as the team knows it:

- the **normalised remote URL**: `github.com/owner/repo`, with scheme, credentials, `.git`
  suffix and case differences removed, so `git@github.com:Owner/repo.git` and
  `https://github.com/owner/repo` are one repository;
- the default branch;
- the team ceiling on unattended runs (ADR-0032);
- team-level repository defaults.

Registering needs no local path and can be done from the browser. Two repositories in one
team cannot share a normalised remote.

**Solo is the exception.** A solo repository may have no remote, which is a legitimate
local-only use. It is then identified by its clone alone, as today. It cannot later be moved
into a team until it has a remote, because a remote is the only identity two machines share.

### 2. A runner maps a team repository to a local clone

Each runner holds, in its own store (ADR-0028), a checkout mapping: team repository → path
of an existing local clone. Before saving the mapping, the runner confirms that the clone's
`origin` normalises to the team repository's remote. A runner claims tasks only for
repositories it has mapped (ADR-0031), and reports that set to the server so the board can
show, for each repository, which members' runners can serve it.

The user points at an existing clone, as task 003 does today. Cloning on demand into the data
directory is a reasonable later addition, and deliberately not part of this decision. It
needs credential handling at clone time that ADR-0020's per-run injection does not cover.

### 3. Worktrees are unchanged, and local

ADR-0005 holds as written on each runner: one worktree and branch per task, under that
runner's data directory, never inside the repository. The board records *which runner* holds
a task's worktree (`runs.runner_id`, ADR-0028), not where it is. "Open worktree in editor"
(task 026) and "reveal in Finder" are available only in the desktop app of the runner that
holds it. Every other client shows which machine has it.

### 4. Every run records the commit it ended on; a connected run must also push it

Every run that ends with commits records the commit its worktree ended on as `runs.head_sha`,
in every mode. It is the value point 5 branches from.

**For a connected or headless runner**, a run is **successful** (ADR-0011's `success`, which
moves the card to `in_review`) only if its branch is on the remote at `head_sha`:

- After the agent exits, the runner checks this with `git ls-remote`.
- If the agent did not push, the runner pushes the branch itself: a plain push of the task's
  `rimaia/…` branch. It is never forced, which ADR-0012 point 3's denied operations already
  forbid, and it never targets the default branch. ADR-0012 point 4 asks the agent not to
  push there, and the runner's own push enforces it in code by naming only the task branch.
- A run whose branch cannot be pushed is not `success`. It is `failed`, with the push error
  on the card. Otherwise the board would say the work exists when no teammate can reach it.

This makes something the base instructions already ask of every run into a postcondition
Rimaia verifies, and it gives the team what chaining needs.

**For a solo runner** nothing changes. The push stays what the base instructions ask for, not
a condition of success, because a solo dependent is created on the same machine and
`head_sha` is already in the local object store.

**With the review-and-fix loop (ADR-0017, task 021)** the postcondition applies once, to the
commit the whole loop ends on. `head_sha` is that final commit, because it is what the task
hands to review and to its dependents.

### 5. A dependent task branches from its dependency's recorded commit

This amends ADR-0008 and supersedes `base_ref.rs`'s "local branch, never `origin/`" rule for
every mode, solo included, so there stays one code path:

- **Satisfaction is unchanged.** A dependency is satisfied by its column alone, `in_review`
  or `done`. ADR-0008's amendment point 2 says so precisely so that a task a person finished
  by hand does not block its dependents forever, and nothing here changes it.
- **Choosing the dependency to branch from is unchanged.** ADR-0008's amendment point 3
  still picks it by column rank, then position.
- **The base is that dependency's latest successful `runs.head_sha`**, not a branch name. A
  connected runner fetches first, and that commit is on the remote by point 4. A solo runner
  already has it.
- **A dependency with no `head_sha` cannot be a base.** For example, a task moved to `done`
  by hand, with no run. Resolution falls through to the next candidate and then to the
  default branch, with the existing warning. This is ADR-0008's amendment point 3 rule ("a
  dependency with no branch cannot be a base"), restated for commits.
- **`runs.base_ref` (ADR-0008 amendment point 4) records the commit**, not only the branch
  name, so a review in the morning measures the diff against exactly what the run started
  from, on any machine.

A commit rather than `origin/<branch>` because a branch moves. If A is retried after B has
started, B was built on A's earlier commit, and the record should say so.

### 6. Credentials stay on the runner

ADR-0020 holds as written, with one change of key. The keychain item is keyed by (team
repository id, runner), in the runner's keychain. The team never stores a forge token, and
one member's token is never used for another member's run. The run pushes as the person whose
runner ran it, and the PR on the forge shows that.

### 7. Review artifacts are uploaded with the run

Because no other client can run git against the worktree, a runner finishing a run uploads a
**review bundle** alongside the transcript (ADR-0036):

- the diff summary (files changed, insertions, deletions);
- the commits made on the branch;
- `head_sha` and the recorded base commit;
- the PR URL when one was opened;
- the patch itself, up to a size cap.

ADR-0013's diff-first run view and task 017's morning review render from this bundle on
every client. The full diff beyond the cap is on the forge, one link away. The desktop app
that holds the worktree may still compute a live diff, and the bundle is what everyone else
sees.

### 8. On-archive cleanup is runner configuration

ADR-0025's per-repository on-archive script is a command executed on a machine. It moves to
the runner's store, per checkout mapping. An archive on the board is broadcast as a change
event. The runner holding that task's worktree runs its own script and its own ADR-0025
cleanup rules, and reports the result. No team member can author a command for another
member's machine (ADR-0032 point 5).

## Consequences

- **Chains work across machines.** A's commit is on the remote before B can be claimed, and B
  starts from exactly that commit wherever it runs.
- **Every successful connected run leaves its work on the forge.** Before, work could exist
  only in a worktree on one laptop. Now it survives that laptop being closed, lost or wiped.
- **A connected run with no push access now fails, where it used to succeed.** The doctor
  (task 018) reports a mapped repository whose remote the runner cannot push to before the
  queue starts, not after. Solo keeps today's behaviour, including local-only repositories.
- **Registering a repository becomes two steps on a team:** an owner registers the remote
  once, and each member maps their clone. The board shows repositories with no runner able to
  serve them, so a task that nobody can run is visible before the night, not after.
- **Review no longer needs the worktree to exist.** Pruning a worktree (task 016, ADR-0025)
  stops removing the ability to review its run.

## Alternatives considered

- **Branch from `origin/<dependency-branch>`.** Close, and it moves: if the dependency is
  retried or amended, the dependent's base silently changes between attempts. That is the
  drift ADR-0008's amendment point 4 already refused.
- **Keep a chain on one runner (affinity).** No push needed between tasks in a chain.
  Rejected as the mechanism because assignment is per person (ADR-0032): A assigned to Alice
  and B to Bob is a normal plan, and affinity would make it impossible. The push is needed
  anyway for review and PRs.
- **The server clones repositories and computes diffs.** A diff anywhere, with no upload.
  Rejected: it needs forge credentials on the server for every team's repositories, which is
  the hosted component with ambient push rights that ADR-0020 and ADR-0030 both refuse.
- **Identify repositories by a local path plus a team-chosen name.** No remote parsing.
  Rejected because two members' clones of the same repository would then be two
  repositories, and dependency and eligibility rules would break at the join.

## Amendment, 2026-10-04 — with the review loop, every successful phase is checked

Task 057 implements point 4 and found its last paragraph unimplementable as written.
Appended rather than edited into the body above, so that everything written against the
original text inherits both the rule and the change to it.

**The runner cannot know which phase is last.** It decides the postcondition before it
calls `finish_run` (seam-contract D31 point 6), and only `finish_run`'s answer, `Continue`
or `Released`, says whether the loop goes on. "Once, to the commit the whole loop ends on"
names a commit the runner can identify only after the moment it has to decide.

**So on a connected or headless runner the postcondition applies to every phase whose
outcome is `success`**: implementation, review and fix alike. A failed, cancelled,
interrupted or retryable phase is not checked, as before. `head_sha` on the task is still
the commit the loop ends on, and that commit is still checked, because it is the last
success.

**This is also the reading the rest of point 4 needs.** Task 021's exit table lands a task
whose review or fix failed in `in_review`, *because the implementation had already
succeeded*. If that success had not been checked on the remote, an unpushable branch would
reach `in_review` through a later phase's failure, which is exactly the outcome point 4
exists to prevent. Checking every success means the head a teammate is handed is always
one they can reach.

**The cost is small.** A review that clears the branch leaves `HEAD` where it was, so its
check is one `git ls-remote` and pushes nothing. A fix pushes what it committed, which the
base instructions ask of it anyway.
