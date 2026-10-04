---
id: "021"
title: Review-and-fix loop
milestone: v0.4
status: ready
depends_on: ["035", "036"]
adrs: ["0017", "0016", "0021", "0009", "0011", "0012", "0004"]
size: L
---

# Review-and-fix loop

## Goal

After a successful implementation run, review the work with a fresh agent, fix what it
finds, and repeat a bounded number of times, so the morning starts from "here is what
could not be fixed" instead of from an unreviewed diff.

This task builds the **engine**: the phases, the budget, the exits, the instructions and
the configuration, and the data a view needs. The review history on screen, the settings
form, the cost figure next to the toggle and the ping-pong signal on the card are task
037's.

## Why now

This task was deliberately late, and `status: not-ready` until the backlog around it
caught up. Three things were open, and each is now decided somewhere else:

- **Where findings live.** Task 035's migration,
  `20261001120100_run_kinds_and_review_findings.sql`, creates `review_findings` and
  carries this task's three columns (`tasks.review_instructions`, `tasks.review_config`,
  `repositories.review_config`). The DDL is in seam-contract D28. This task adds no
  migration.
- **How a run reaches its own card without reopening the operator surface.** D30: the
  run-scoped handle is served as `rimaia-run`, and the operator-surface denial on `rimaia`
  is unconditional. Task 020 had denied `mcp__rimaia*` to every run by tool name, which
  would have blocked this task's write-back too; D30 records why a second server name is
  the only one of the three ways out that keeps "a run reaches its own card and nothing
  else" without a condition.
- **Who decides whether a loop continues when the runner is not the board.** D31: the
  runner reports facts and `finish_run` answers `NextStep`. The budget is ADR-0017's rule,
  so it is decided in the board's code.

It is written against task 036's `BoardPort`, because writing it against `run_task`'s
direct board writes would mean moving it again in 042 and 052.

The original condition was to revisit after task 017 had been used for real mornings. It
is a risk, not a gate, and 017 is not a dependency: nothing here uses its code. The loop is
off by default, so landing the engine commits nobody to paying for it. If real mornings
show that mechanical findings do not dominate, it stays off.

## Scope

### The loop

```
implement → review → findings? → fix → review → … → clean, or budget spent
```

- **`max_review_loops` counts fix phases.** A fix is always followed by a review. With
  the default of 2 there are at most three reviews and two fixes. `0` with the loop on is
  ADR-0017's report-only mode: one review and no fix.
- **A phase is a run of rows, not a row.** A review that hits a usage limit and resumes is
  one phase across two rows. A phase is a maximal run of contiguous rows sharing
  `(kind, session_id)`, D29 point 3's boundary. Three things read phases, never rows:
  - **the budget and every loop count.** A retried review never spends the budget twice.
    This amends D29 point 8, which counts review *rows* (below);
  - **the witness.** A review phase has **recorded** when any of its rows has
    `runs.findings_recorded_at` set (D30 point 7 and its 2026-09-30 amendment), read
    through task 035's store, never from a count of `review_findings` rows;
  - **`HEAD` moved.** The phase's last row's `head_sha` is compared with the `head_sha` of
    the row immediately before the phase's first row. A reviewer that committed in row 1
    and resumed in row 2 has still moved `HEAD`. Either value `NULL` reads as moved: a
    branch that cannot be compared is not a clean one.
- **The loop belongs to the newest implementation phase.** Rows after it make up the
  current loop. Run now on a task starts from implementation again, and the budget resets
  with it.
- **The run window bounds it.** A loop never starts a phase after
  `FinishRun::window_closes_at` (D24 point 4 is its source). The phase that is running
  finishes, and the task exits through the table below.
- **It never advances a task to `done`.** A human still approves (ADR-0017).

### Where each decision is made

**Board side.** One function decides, and every path that closes a row reaches it.

- `review_loop::decide` is pure. It takes the effective configuration, the current loop's
  rows and findings, the closed row's outcome, `window_closes_at` and `now`, and returns
  the `NextStep` and, on exit, where the task lands. Every exit in the table below is a
  case of it.
- It runs in `outcome::finish_run`'s task-side step, `apply_to_task`, which dispatches on
  the closed row's `kind` (D29 point 9). 035's `Review` and `Fix` refusals in
  `outcome::finish_run` are removed: a review or fix row closes like any other, and the
  task-side step lands the task by the exit table. `board::service::finish_run` answers
  the `NextStep` that step returns.
- **Reconcile reaches the same step.** Until 043, `scheduler/reconcile.rs::reconcile_one`
  calls `outcome::finish_run` directly. An open review or fix row a crash left therefore
  lands `waiting_retry` when `retry::decide` gives a `resume_after` (D9's amendment), and
  `in_review` with `idle`, unreviewed, otherwise. `settle` then finds nothing to do.
  Reconcile's outcome is `interrupted`, never a success, so it never meets a `Continue`
  case.
- **The row-first ordering does not change.** The row's `UPDATE` commits before the
  task-side step, as `outcome::finish_run`'s doc comment and reconcile's header rely on.
  Inside the task-side step, `Continue` and the claim-side writes later tasks attach to it
  (043's lease purpose, 045's consent re-check, D31 points 4 and 6) are made together in
  one transaction. The effective configuration is read again there, so a setting changed
  mid-loop takes effect at the next phase boundary.
- While a loop continues, the task stays in its column with `run_state = running`, and
  nothing is written to `run_state`. `is_legal_run_state_transition` and its table do not
  change. The move to `in_review` happens once, when the loop exits, through
  `move_task_to_bottom`, with one change event.

**Runner side**, in `run_task`: one loop, one spawn per iteration.

- **Where it enters.** At `claim.resume.kind` when the claim carries a resume, reading
  `RunContext::review` from the claim's context; at implementation otherwise.
- **Each iteration.** On `NextStep::Continue { kind }` it calls `run_context` for the
  fresh `RunContext::review`, composes the phase, mints the `run_id` (D10) and the grant,
  calls `start_run` with that kind, and runs the process. Every phase is a `SpawnIntent`
  spawned through the implementation's `plan_spawn` and `execute` path, so D25's
  credential environment and redaction, D27.5's `CLAUDE_*` strip and D30 point 6's
  per-spawn resolver (055) apply to every phase without a second copy.
- **One claim.** It keeps D19's in-flight slot and the claim across phases, and releases
  them once.
- **Between phases, nothing ends through a bare `release`.** `release` moves a `running`
  task to `failed` (D31 point 4), which would throw away a succeeded implementation. So
  after a `Continue`, every exit before the next spawn is recorded as a row of the pending
  kind and closed through `finish_run`: `fatal` for a refusal (below), a `run_context`
  error or a missing worktree, and `cancelled` for a Cancel that arrives while no process
  is running. The exit table then lands the task. Only if that row cannot be written does
  the runner fall back to `release`. That case, and a crash between a `Continue` and the
  next `start_run`, land the task `failed`. They are the named residual: the board could
  not be written, or nothing was alive to write it.
- **A phase refused before it spawns** is such a row, with the refusal as its
  `error_message`. Examples: no MCP endpoint bound (the message names Settings → MCP, as
  D17.4 does), a `negotiate` refusal, an unenforceable denial on an unattended run, or a
  dirty worktree before a review (below). The history then shows it.
- **A row without a spawn gets an empty transcript file** at its `log_path`, so
  `startup::missing_run_logs` does not report it on every launch. This relaxes the initial
  schema's comment on `runs` ("exists here only once a process was spawned for it"), and
  ADR-0017's amendment says so.
- **A review's worktree is checked on both sides.** A shell can edit files that
  `AnyFileMutation` (`Write`, `Edit`, `NotebookEdit`) does not cover. Unchecked, a review
  could land clean on a dirty worktree, and the next fix would commit the reviewer's edits
  as its own. So:
  - before spawning a review, the runner refuses the phase if tracked files have
    uncommitted changes: the reviewer judges commits;
  - after it, a review that left tracked changes is rewritten to `fatal` with
    `The review changed the worktree without committing.`, the way `override_as_fatal`
    rewrites an outcome, before `finish_run`.

  Untracked files are ignored on both sides, because a test run leaves them. The check is
  a helper beside `worktree/git.rs`'s `is_dirty` that excludes untracked files.
- **Cancel.** A Cancel stops the running phase through the existing cancel path. The loop
  does not continue after it.
- **The runner never counts loops and never chooses to continue.** A runner that did would
  be a second copy of ADR-0017's budget, on a machine the board does not control (D31's
  Why).

**Resume by kind.** A resumed review or fix resumes as its own kind (D29 point 3). After
036 the non-`Implementation` refusal lives in one place:
`scheduler::attempts::resume_as_implementation`, applied by `board::service::claim` before
any edge. This task removes it, so `Claim::resume` carries any kind and `run_task` enters
at it. The two callers that pass that refusal on lose their special case: `try_step`'s
handling of an `Invalid` from a `continue_session: true` claim in `scheduler/queue.rs`, and
the manual starter in `crates/core/src/runner/start.rs`. `src-tauri/src/commands/runs.rs`'s
"Retry now" stays a thin caller of the starter.

**Additions to 036's DTOs** (D31 points 2 and 6):

- `RunContext::review: Option<ReviewContext>`, holding:
  - the effective review instructions and `EffectiveReviewConfig`;
  - the newest review's open blocking findings, which the fix phase is composed from;
  - the task's rejected findings;
  - the newest implementation phase's `session_id`, `base_ref` and `base_sha`;
  - the newest row's `head_sha` and its 033 bundle summary;
  - whether the current review phase has recorded, for the resume prompt.
- The first production return of `NextStep::Continue`.
- The contract suite in `crates/core/src/testing/board_contract.rs` gains 021's cases,
  listed under Acceptance criteria. They run through D31 point 9's in-process adapter.

### Exits

| Row that finished | Condition | Board answers | Task |
| --- | --- | --- | --- |
| implementation, any failure | — | as today | as today (ADR-0011) |
| implementation, success | loop off | `Released` | `in_review`, `idle`, no verdict (as today) |
| implementation, success | loop on, window open | `Continue { Review }` | stays, `running` |
| implementation, success | loop on, window closed | `Released` | `in_review`, `idle`, unreviewed |
| review, success, phase recorded, `HEAD` unmoved | no open blocking finding | `Released` | `in_review`, `idle`, **clean** |
| same | blocking findings, fixes spent < budget, window open | `Continue { Fix }` | stays, `running` |
| same | blocking findings, budget spent or window closed | `Released` | `in_review`, `idle`, findings remain |
| review, success | phase not recorded, or `HEAD` moved | `Released` | `in_review`, `idle`, unreviewed |
| fix, success | window open | `Continue { Review }` | stays, `running` |
| fix, success | window closed | `Released` | `in_review`, `idle`, unreviewed |
| review or fix | retryable, `resume_after` set | `Released { resume_after }` | `waiting_retry`; resumes **as the same kind** |
| review or fix | fatal, cancelled, or retry budget exhausted | `Released` | `in_review`, `idle`, unreviewed |

Why each row lands where it does:

- A failed or cancelled review or fix still lands in `in_review` with `idle`, because the
  implementation had already succeeded. Losing that to a reviewer's failure would be
  worse than the loop being off. This holds on every path that closes a row, reconcile's
  included.
- A review that moved `HEAD` has changed the branch it was asked to judge. It has become an
  unreviewed fixer, so it counts as a failed review.

### The verdict: what "flagged" means, derived and never stored

```rust
pub fn verdict(rows: &[LoopRow], findings: &[ReviewFinding], config: &EffectiveReviewConfig)
    -> Verdict
```

It is pure. `LoopRow` is the projection of a `runs` row the loop reads: id, `kind`,
`attempt`, `status`, `exit_class`, `session_id`, `head_sha` and whether
`findings_recorded_at` is set. Its loader reads that column through
`crates/core/src/review/findings.rs`, so 035's store stays its one reader. The values:

- `None`: the loop is off and the task has no loop rows.
- `Clean`.
- `FindingsRemain { open_blocking }`.
- `Unreviewed { reason }`, where `reason` is one of `not_reviewed`, `review_failed`,
  `nothing_recorded`, `review_changed_branch` or `fix_not_reviewed`. A review rewritten to
  `fatal` for a dirty worktree is `review_failed`, and its row's message says why.

A task is **flagged** when its verdict is `FindingsRemain` or `Unreviewed`. There is no
column for it, for D29 point 8's reason and because D28 gives this task none.

One case has to read the current configuration rather than the rows: a succeeded
implementation with no loop rows after it. It reads as `not_reviewed` when the loop is
effectively on now, and as `None` when it is off. That statement is true whenever it is
shown, which is the property that matters.

### Phases

**Review phase.**

- **Session.** A fresh session, a new `session_id`, never `--resume`. It gets no
  implementation context. That is the mechanism: a session reviewing its own work grades
  itself generously (ADR-0017).
- **Model and effort.** `review_model` and `review_effort` from the effective
  configuration. When absent, the task's effective strategy (`RunContext::strategy`).
- **Environment.** It follows `run_environment`, `inherit` by default. Unlike the
  planner, a review exists to run the user's own review skill or slash command, and
  `strict_local` would hide it. The operator `rimaia` surface is denied whatever the
  setting (D30 point 2).
- **Posture.** The posture the implementation's claim trigger gives (ADR-0012,
  ADR-0031 point 7), plus two changes:
  - `ForbiddenOperation::AnyFileMutation` is added to the implementation's denials.
  - `required_tools` is exactly `[record_review_findings]` (D30 point 7), spelled at the
    handle's server (D30 point 3). An intent with `required_tools` and no handle is
    `negotiate`'s `HandleInjection` refusal, and so a refused phase.

  Shell commands are not denied, because a review that cannot run the test suite is
  guessing. The worktree checks above are what keep the shell from editing unnoticed.
- **Handle.** `RunHandles::grant(task_id, Grant::Review { run_id })`, served as
  `rimaia-run`. It is revoked on `Drop` at the end of the phase.
- **Rows.** The row records the implementation row's `base_ref` and `base_sha`
  (D29 point 4), and D18's capture columns as any row does.

**Fix phase.**

- **Session.** `fix_session: fresh` (the default) opens a new session. `resume` continues
  **the newest implementation row's** session, read with a `kind = 'implementation'`
  filter, never through `resume_point` (D29 point 3). A resume with no such session, or
  with a provider that cannot resume, falls back to fresh and records a warning on the
  row.
- **Model and effort.** The task's effective strategy, exactly as the implementation
  used.
- **Posture and environment.** The implementation's, plus `required_tools` of exactly
  `[resolve_review_finding]` through `Grant::Fix { run_id }`.

### Prompts (ADR-0009's composition rules; exact strings under test)

All live in `crates/core/src/runner/prompt.rs`. Each uses level-1 headings and
`SECTION_SEPARATOR`, omits empty sections together with their heading, and has no trailing
newline. Tool names come from the provider's `tool_handle` at `RUN_MCP_SERVER_NAME`
(D30 point 1); `{tool}` below is that spelling.

- **`# Task context` names the row's base.** `task_context` gains a `base_ref` argument.
  `compose_prompt` passes `repo.default_branch`, so its exact strings do not change. The
  review and fix prompts pass `ReviewContext`'s recorded `base_ref`, which since task 011
  can be a dependency's branch, so it agrees with `# The change`.
- **`compose_review_prompt`**:
  - Sections, in order: `# Your job` · `# Task context` · `# Plan` ·
    `# Extra instructions` · `# The change` · `# Review instructions` ·
    `# Findings already rejected` · `# How to answer`.
  - **It has no base-instructions parameter**, for the planner's reason (ADR-0009's
    2026-08-28 amendment): base instructions say "open a pull request", and a reviewer
    that opens one is a defect.
  - `# The change` names the base ref, the base commit and the head commit, gives the
    033 bundle's diff stat and file list when a bundle exists, and gives the two `git`
    commands that read the whole branch against its base. It does not embed the patch:
    the worktree holds it, and a capped copy would be truncated silently.
  - `# Findings already rejected` lists each rejected finding with its reason, so the
    reviewer does not raise it again.
  - `# How to answer` requires exactly one call, with `findings: []` when there is
    nothing to report. It names the four severities and the location fields, and says
    not to edit, commit or push.
- **`compose_review_system_append`**: the orchestrator facts a reviewer must not
  negotiate. It did not write this change. The tool call is its only answer. It must not
  commit.
- **`compose_fix_prompt`** (fresh):
  - Sections, in order: `# Base instructions` · `# Task context` · `# Plan` ·
    `# Extra instructions` · `# Findings to address` · `# How to answer`.
  - Base instructions are included, because the fix is implementation work: commit, run
    the suite, push.
  - `# Findings to address` renders each finding's `id`, severity, location, title and
    body.
  - `# How to answer` says a finding is advice, not an order. For each finding, the fixer
    calls `resolve_review_finding` with `fixed` and what it changed, or with `rejected`
    and why.
- **`compose_fix_continuation`** (a fix phase that starts on the implementation's session,
  `fix_session = resume`): only `# Findings to address` and `# How to answer`. A resumed
  session already holds the rest (ADR-0009: "Resumed runs do not re-send the composed
  prompt").
- **Resumed after a retryable exit**, in place of `compose_resume_prompt`, whose "Continue
  the task" would tell a reviewer to implement. Single paragraphs, exactly:
  - `compose_review_resume`, phase not yet recorded:
    `Continue the review of "{title}" from where you stopped. The change and the review instructions are earlier in this session — do not start over. Do not edit, commit or push. Finish by calling {tool} exactly once, with findings: [] if you found nothing.`
  - `compose_review_resume`, phase already recorded:
    `Continue the review of "{title}" from where you stopped. Your findings are already recorded — do not call {tool} again, and do not edit, commit or push. Stop once you have finished what you were doing.`
  - `compose_fix_resume`:
    `Continue addressing the review findings on "{title}" from where you stopped. The findings and the instructions are earlier in this session — do not start over. Call {tool} for each finding you have not resolved yet, with fixed and what you changed, or rejected and why.`

  A second `record_review_findings` call within one phase is not refused by 035's
  per-run check, so the recorded variant is what keeps a resumed reviewer from recording
  twice; `decide` reads the phase as recorded either way.
- **Review instructions.**
  - The task's `review_instructions`, when non-blank, **replaces** the global setting
    rather than adding to it. An override that added would run two review skills on one
    change.
  - A blank override falls back to the global setting.
  - ADR-0009's template variables are expanded the way base instructions are, and
    unknown variables are left verbatim.
  - Rimaia ships no review methodology. When neither level sets instructions, the section
    is omitted and `# Your job` stands alone.

### Findings

Task 035 owns the store (`crates/core/src/review/findings.rs`), the two tools and their
arguments. This task fills in the two behaviours 035 left to it:

- **The fingerprint.**
  - `fingerprint = normalise(file) + "|" + normalise(title)`. Here `normalise` trims,
    lowercases and collapses each run of whitespace to one space, and a NULL `file` is
    the empty string.
  - The line is left out on purpose, because a fix moves lines.
  - It is computed in `review::findings::record`, the one writer of `review_findings`.
- **Rejected findings are not raised again as new.** A finding whose fingerprint matches
  a `rejected` finding on the same task is stored with `status = 'rejected'` and
  `resolution = "Rejected earlier as <id>: <reason>"`. It never counts as blocking and
  never reaches a fixer. The row is kept rather than dropped, so the history shows that
  the reviewer raised it again.
- **Blocking** means `severity >= blocking_severity`, ordered
  `critical > high > medium > low`.
- **The fix phase gets the newest review's open blocking findings, and only those.**
  Findings below the threshold stay open and attached, and are advisory. Open findings
  from earlier reviews that a fixer neither fixed nor rejected stay in history. If they
  still apply, the next fresh review raises them again.

### Ping-pong detection (data only; 037 draws it)

For each review phase after the first in the current loop, the history reports:

- `regressed`: findings whose fingerprint matches one the preceding fix marked `fixed`.
- `new_after_fix`: blocking findings whose fingerprint the preceding review did not
  raise.

`ping_pong` is true when either list is non-empty. It is a signal, not a verdict. A fresh
reviewer may simply notice something the first one missed, and the card says "may be
going in circles", not "the fix broke it". The budget still bounds the loop. The signal
is there so a ping-pong is not silently absorbed by the budget (ADR-0017).

### Configuration

`ReviewConfig` is a JSON document (D28: "review_config is JSON"). Every field is
optional, and an absent field inherits:

```json
{ "enabled": "off" | "on_cost_acknowledged",
  "max_review_loops": 2,
  "blocking_severity": "critical" | "high" | "medium" | "low",
  "review_model": "opus", "review_effort": "high",
  "fix_session": "fresh" | "resume" }
```

- **Precedence, field by field.** Task (`tasks.review_config`), then repository
  (`repositories.review_config`), then global (`settings["review_config"]`), then the
  built-in defaults: `off`, `2`, `medium`, the task's strategy, `fresh`.
- **Enabling means acknowledging the cost, and the spelling is the record.** There is no
  boolean. `on_cost_acknowledged` is the only "on" value, following D20's
  `on_done_acknowledged`. A door given `true`, `"on"` or any other value refuses it as
  `Error::invalid` (D8). A stored value that does not parse (for example a hand-edited
  `true`) logs a warning and reads as absent, which means off. That is D17.2's tolerance
  rule, and it keeps a typo from enabling a spend.
- **Limits.** `max_review_loops` ranges over `0..=5`, and anything above is refused. The
  ADR's word is "bounded", and five fixes is already a night.
- **Model and effort.** `review_model` and `review_effort` are validated against the
  strategy catalogue the way `set_strategy_defaults` validates them.
- **`settings["review_instructions"]`** holds the global text. The global `review_config`
  is a second settings key. Both are owned by `crates/core/src/review_loop/config.rs` in
  D3's shape, and the accessor says both are **team** placement (D28 point 4, ADR-0028
  point 2). No row is seeded, because an absent key is off.

### Doors (ADR-0021 parity; ADR-0006: one core function behind each)

There are four operator commands. Each is a thin Tauri command in
`src-tauri/src/commands/review.rs`, the module task 034 created for its six review commands
and 035 extended with `list_review_findings`; this task adds to it and does not create it.
Each is registered in both `generate_handler!` lists in `src-tauri/src/lib.rs`, and each is
an MCP tool of the same name:

- `get_review_settings`: the global instructions and `ReviewConfig`.
- `set_review_settings`.
- `set_repository_review_config`.
- `set_task_review`: the task's `review_instructions` and `ReviewConfig`.

Scope rules:

- **Every one is `Refused` for every grant.** They are D30 point 5's "everything else",
  and ADR-0021 point 4's "reconfigures the installation". A run that could enable its own
  loop would be spending on its own authority. A fixer that could rewrite its own review
  instructions would be marking its own homework.
- The task's fields are not added to `TaskPatch`, because `update_task` is `OwnTaskOnly`
  for a strategy grant.
- `get_task` and `TaskDetail` gain the task's two raw fields and
  `review_loop: Option<ReviewLoopSummary>`, which holds the effective `enabled` and
  `max_review_loops`, the fixes spent, the verdict, `open_blocking` and `ping_pong`. They
  are mirrored in `src/types.ts`.
- The frontend wrappers in `src/lib/commands.ts` are 037's.
- **D32's appendix gains four rows in the commit that adds the commands** (D32 point 8:
  every command added before 046 is classified when it is added, so 046 migrates it
  without judging it). They are appended under the dated "added after 728a049" sub-heading
  task 034 created, after 035's `list_review_findings` row, and the appendix's counts,
  which describe `main` at 728a049, are left as they are. All four are module `review`,
  kind `board`, From `046`, and each cites ADR-0021 points 3 and 4 and this task:

  | Command | Effect | Note |
  | --- | --- | --- |
  | `get_review_settings` | Read | A team setting (D28 point 4, ADR-0028 §2). Refused to every grant (ADR-0021 §4) |
  | `set_review_settings` | Write | A team setting. Refused to every grant: a run must not enable its own loop (ADR-0021 §4). `review_model` and `review_effort` are validated against the catalogue from `BoardHost.provider`, as `set_strategy_defaults` is (D32 point 2) |
  | `set_repository_review_config` | Write | As `set_review_settings`, per repository. The config is a column on `repositories`, so it is board state, not a per-checkout runner setting (ADR-0033 §1) |
  | `set_task_review` | Write | As `set_review_settings`, per task. 045 makes `review_instructions` consent-gated content with a revision (ADR-0032 §3); the handler stays on the board |

  None of the four touches the disk, spawns or reads a worktree, so none is a "local until
  then" row.

### Reads: pure builders, thin loaders

The rules are pure functions over loaded rows, findings and effective configuration, so
037 can call them per card over one batched read without copying a rule:

- `verdict(rows, findings, config) -> Verdict` (above);
- `summary(rows, findings, config) -> ReviewLoopSummary`;
- `phases(rows, findings) -> ReviewHistory`: the current loop's phases, each review with
  its findings, the fix that followed it and what that fix resolved, and the ping-pong
  lists. Earlier loops, meaning those before a re-run implementation, are included and
  marked as earlier.

`review_loop::history(ctx, task_id)`, `get_task`'s summary and 035's digest entry are
single-task loaders over them. **The digest's loop count becomes the phase count.** 035's
`DigestLoop::reviews_since_implementation` is computed from `phases`, so the digest and
`ReviewLoopSummary` cannot disagree about a retried review. 037 adds the history's command
and tool.

### Amendments (dated, appended, nothing above them edited)

- **Seam-contract D29 point 8:** a loop number counts review *phases* after the newest
  implementation phase, a phase being contiguous rows sharing `(kind, session_id)`, and
  035's digest count follows it.
- **ADR-0017**: the decisions under the headings above. These are:
  - phases, the phase-level witness, `HEAD` moved across a phase, and the budget;
  - the exits table and the verdict, on every path that closes a row;
  - the fingerprint and the rejected carry-over;
  - override-replaces;
  - the enable spelling and the `0..=5` bound;
  - the run-window boundary;
  - the review's worktree checks;
  - rows recorded without a spawn (a refused phase, an exit between phases), their empty
    transcript, and the initial schema comment they relax;
  - the residual: a crash, or an unwritable board, between phases lands `failed`;
  - the ping-pong definition.
- **ADR-0009**: the review and fix prompts' section lists and the two resume prompts, in
  the form the planner's amendment uses.
- **ADR-0004**: review and fix phases follow `run_environment`, unlike the planner, and
  why.
- **ADR-0012**: two rows for the posture table, review and fix, including why the shell
  stays allowed and what the worktree checks close. CLAUDE.md forbids widening the posture
  without this amendment.

## Out of scope

- **Every screen: task 037.** That covers the review history on the task, the settings
  form and its cost figure (`observed_run_cost`), the ping-pong signal and verdict on the
  card, and D12's card field that carries them (a D12 amendment that 037 makes).
  `commands.ts` wrappers and `RunFilter::kind`'s control also belong there.
- **The findings table, the two findings tools, `runs.kind`, `Grant`, the `rimaia-run`
  name and the audit of every `runs` reader: task 035.** The retry-budget boundary per
  phase is also 035's (D29 point 3).
- **The port, the in-process adapter and the move of `run_task`'s board writes: task
  036.** This task adds fields and one `NextStep` variant, and nothing else, to its types.
- **Consent.** Task 045 covers the revision of `review_instructions` and findings as
  consent-gated content (ADR-0032 point 3), and the re-check on `Continue`.
- **The runner's model and effort cap on the review phase:** 045.
- **Lease purposes moving at `start_run`, and per-runner reconcile:** 043.
  **`record_review_findings` over the port:** 055. **The push postcondition on every
  successful phase** (ADR-0033 point 4, as amended 2026-10-04): 057.
- **The morning review (017) and review actions (034).** A human's approve, reject and
  needs-changes are theirs. The loop never takes any of them.
- **Re-running only the review on a task already in `in_review`.** Run now starts from
  implementation, and the budget resets with it.
- **Shipping a review methodology.** Rimaia schedules the one the user already trusts.

## Acceptance criteria

- **A planted finding is fixed unattended.** This is the engine's version of the original
  criterion, run with the fixture harness and no tokens.
  `a_planted_finding_is_fixed_and_the_second_review_is_clean`:
  - Setup: a real repository in a `TempDir`, `FakeCli`, a `TestClock`, and the loop
    enabled.
  - The implementation commits.
  - Review 1 records one `high` finding through the real `/mcp/run/<token>` route.
  - The fix commits and resolves it `fixed`.
  - Review 2 records `findings: []`.
  - Result: the task is in `in_review` with `idle` and a `Clean` verdict. There are
    exactly four spawns and four `runs` rows, with kinds
    `implementation, review, fix, review` and attempts `1..=4`. Every row carries the
    implementation's `base_ref` and `base_sha`.
- **The budget holds.**
  `an_unfixable_finding_ends_the_loop_at_budget_with_findings_attached`:
  - The same blocking finding is raised on every review, and the fix never resolves it.
  - With `max_review_loops = 2` there are exactly six spawns.
  - The task lands in `in_review`, `idle`, with `FindingsRemain`. The findings are
    readable through `get_task`'s `review_loop` and through `review_loop::history`.
- **A rejected finding.**
  `a_rejected_finding_raised_again_is_stored_rejected_and_not_blocking`:
  - The rejection's reason is stored.
  - The same finding, raised again with a different line and different case, is stored
    `rejected` with the "Rejected earlier as" resolution.
  - It does not start a fix, and it appears under `# Findings already rejected` in the
    next review prompt.
- **Off by default, and enabling needs the acknowledgement.**
  - `the_loop_is_off_on_a_fresh_database`.
  - `enabling_the_loop_requires_the_cost_acknowledged_spelling`: `true` and `"on"` are
    refused at the service, the command and the tool.
  - `a_hand_edited_true_in_stored_config_reads_as_off`.
  - `a_successful_implementation_with_the_loop_off_lands_exactly_as_before`: a golden
    taken from today's behaviour, asserted explicitly. One `runs` row (`implementation`,
    `succeeded`, attempt 1); the exact `ChangeEvent` sequence the close publishes, recorded
    from the code before this task's first commit and asserted as a literal list; the task
    in `in_review` with `idle`; `review_loop` `None`.
- **Every exit in the table has a case** in `review_loop::decide`'s unit tests. Required
  names:
  - `findings_below_the_blocking_severity_do_not_start_a_fix`
  - `max_review_loops_zero_reviews_once_and_never_fixes`
  - `a_fix_is_always_followed_by_a_review_while_the_window_is_open`
  - `a_closed_run_window_ends_the_loop_at_the_next_phase_boundary`
  - `a_review_that_records_nothing_lands_unreviewed_and_never_clean`
  - `a_review_that_moves_head_lands_unreviewed`
  - `a_review_that_committed_before_its_usage_limit_still_lands_unreviewed`
  - `a_resumed_review_that_recorded_before_the_limit_is_not_nothing_recorded`
  - `a_fatal_or_cancelled_review_lands_in_review_idle_unreviewed`
  - `the_budget_counts_phases_not_retried_rows`
  - `a_rerun_implementation_starts_a_fresh_budget`
  - `no_loop_decision_moves_a_task_to_done`, exhaustive over the enumerated domain: every
    `RunKind` × `ExitClass` × {recorded, not recorded} × {`HEAD` moved, unmoved} ×
    {window open, closed} × {budget left, spent} × {blocking finding, none}.
- **Loop counts agree.** `a_retried_review_counts_once_in_the_digest_and_the_summary`:
  a review that hit a usage limit and resumed is one phase in 035's digest entry and in
  `ReviewLoopSummary`.
- **Crash recovery.**
  `a_review_left_open_by_a_crash_is_reconciled_into_in_review_or_a_review_resume`: drive
  `reconcile_interrupted` on a task whose newest row is an open review. With retry budget
  left it lands `waiting_retry` with a `resume_after`; with the budget spent it lands
  `in_review`, `idle`, unreviewed. Never `failed`.
- **Resume by kind**, using the clock, not a sleep:
  - `a_review_waiting_on_a_usage_limit_is_resumed_by_the_queue_as_a_review`: the second
    review spawn carries `--resume` with the review's session and `compose_review_resume`'s
    text, and "Retry now" does the same.
  - `a_fix_waiting_on_a_usage_limit_resumes_as_a_fix_with_the_fix_resume_prompt`.
  - `a_resumed_fix_continues_the_implementation_session_not_the_reviews` covers
    `fix_session = resume`.
- **Fresh context.** `a_review_phase_opens_a_fresh_session_and_never_resumes`.
- **Argv and environment are pinned byte for byte, per the rules in
  `tests/runner_process.rs`:**
  - `a_review_phase_argv_carries_rimaia_run_and_denies_file_mutation`: the `--mcp-config`
    key is `rimaia-run`, `--allowedTools` is exactly the one review tool, the file-mutation
    denials are present, and the operator-surface denial is present with no
    `mcp__rimaia-run` pattern.
  - `a_fix_phase_argv_allows_only_resolve_review_finding`.
  - `a_fix_phase_spawn_carries_the_repository_credentials_and_strips_claude_vars`: D25's
    variables are present and redacted from the transcript, and D27.5's are absent.
- **The grant is scoped and short-lived.**
  - `a_phase_grant_is_revoked_when_the_phase_ends`: the token answers not-found
    afterwards.
  - `a_review_grant_cannot_call_resolve_review_finding` and its converse.
- **Rows without a spawn are recorded.**
  - `a_review_refused_before_spawn_is_recorded_as_a_failed_review_row`: with no MCP
    endpoint bound, a `review` row finishes `fatal` with a message naming Settings → MCP,
    the task lands `in_review`, unreviewed, and `startup::missing_run_logs` does not report
    the row.
  - `a_cancel_between_phases_lands_in_review_unreviewed`: a `cancelled` row of the
    pending kind, and no `failed`.
- **The worktree checks.**
  - `a_review_that_leaves_tracked_changes_lands_unreviewed`.
  - `a_review_is_refused_when_the_implementation_left_tracked_changes`.
- **Phases share one claim.**
  - `the_in_flight_slot_is_held_across_phases_and_released_once`.
  - `the_task_stays_running_between_phases_and_moves_to_in_review_once`, asserting one
    column-change event.
  - `cancelling_during_a_review_ends_the_loop_in_review_unreviewed`.
- **Prompts are exact strings** in `crates/core/tests/prompt.rs`:
  - `review_prompt_composes_its_eight_sections_in_order`
  - `review_prompt_omits_empty_sections_with_their_heading`
  - `review_prompt_without_a_bundle_names_the_commits_only`
  - `review_and_fix_task_context_name_the_rows_base_ref`
  - `a_task_review_override_replaces_the_global_instructions`
  - `a_blank_task_override_falls_back_to_the_global_instructions`
  - `review_instructions_expand_template_variables_and_keep_unknown_ones`
  - `review_system_append_is_exact`
  - `fresh_fix_prompt_composes_base_instructions_plan_and_findings_in_order`
  - `resumed_fix_sends_only_the_findings_and_how_to_answer`
  - `review_resume_prompt_is_exact_before_and_after_recording`
  - `fix_resume_prompt_is_exact`
- **Fingerprint and ping-pong:**
  - `fingerprint_ignores_line_case_and_whitespace`
  - `the_fix_receives_only_the_newest_reviews_open_blocking_findings`
  - `a_new_blocking_finding_after_a_fix_is_reported_as_ping_pong`
  - `a_fixed_finding_raised_again_is_reported_as_regressed`
  - `a_single_review_never_reports_ping_pong`
- **Configuration:**
  - `task_config_overrides_repository_overrides_global_field_by_field`
  - `max_review_loops_above_five_is_refused`
  - `a_review_model_outside_the_catalogue_is_refused`
- **The board contract suite** gains, through the in-process adapter:
  - `finish_run_continues_to_a_review_when_the_loop_is_on`
  - `finish_run_releases_an_implementation_when_the_loop_is_off`
  - `finish_run_releases_a_clean_review_and_lands_the_task`
- **MCP.** The four configuration tools are registered.
  `every_registered_tool_has_a_run_scope_decision`
  and `tests/mcp_scope.rs` assert each is refused on the `Strategy`, `Review` and `Fix`
  grants. A parity test shows that the command and the tool reach the same service
  function.
- **The four commands are in 034's `src-tauri/src/commands/review.rs`**, beside 034's six
  and 035's one, and no second review command module exists.
- **D32's appendix has the four rows** from Scope, under 034's "added after 728a049"
  sub-heading, each with module `review`, kind `board`, its Effect (`Read` for
  `get_review_settings`, `Write` for the three setters), From `046`, its Note, and the
  ADR-0021 citation. They land in the same commit as the commands (D32 point 8), and the
  appendix's 728a049 counts are unchanged.
- **The engine never spends in tests.** No test in this task spawns anything but the
  `FakeCli` stand-in. Review and fix write-backs go over the real run-scoped HTTP route,
  in the way `tests/runner_strategy.rs`'s planner write-back does.
- **The amendments exist**, dated and appended. The row for 021 in the seam-contract "How
  to use this" table lists the entries in the Notes.
- **No migration was added.** If a column 035 did not add turns out to be needed,
  **stop**: D4 and D28 make that a stop-and-ask. `.sqlx/` is regenerated for every
  changed or new query (D5, with `--all-targets`) and committed.
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.
- **Needs a human, carried as a checklist in the PR body:** at the end of milestone v0.4,
  run one real unattended night with
  `RIMAIA_DATA_DIR=/tmp/rimaia-m1 npm run tauri dev`, a task with a deliberately planted
  bug, and the loop enabled. The review has to catch the bug and the fix has to fix it.

## Notes

**Seam entries to read.**

- D3: settings keys and accessors. D8: no new error codes. D10: ids.
- D4 and D6 as prohibitions: no migration, no new dependency. The fingerprint is a
  normalised string, not a hash. D5: the `.sqlx` cache.
- D9 and its 2026-09-03 amendment: where an interrupted review lands.
- D12: read only. The card field is 037's.
- D17, points 2, 4, 5 and 9: tolerance, the handle's mechanism, the planner's precedent,
  and "no printed fallback".
- D18: capture columns on review and fix rows.
- D19: the in-flight slot across phases.
- D20, point 3: the `…_acknowledged` spelling.
- D23 point 7, as D29 amends it.
- D24 point 4: the run window behind `window_closes_at`.
- D25: credentials and redaction on every phase's spawn. D27, point 5: the identity strip.
- D28: point 4's team placement, and the 035 file.
- D29, all of it: points 3, 4, 8 and 9 are this task's instructions, and point 8 is
  amended here.
- D30, all of it: point 1's name in prompts, point 3's spelling and `HandleInjection`
  refusal, points 2, 5 and 7, and point 8's fixture.
- D31, points 2–6, 9 and 13.
- D32 point 8 and its appendix, including 034's "added after 728a049" sub-heading: the
  four configuration commands are classified when they are added. Point 2 for why the
  model check reads `BoardHost.provider` from 046 onward.

**Files to start from.**

- `crates/core/src/runner/process.rs`: `run_task` becomes the phase loop.
  `forbidden_operations`, `override_as_fatal` and `RIMAIA_TOOL_SURFACE` are here.
- `crates/core/src/runner/prompt.rs`: `task_context`, and the new compose functions beside
  `compose_prompt`, `compose_resume_prompt` and the strategy pair.
- `crates/core/src/runner/strategy.rs`: the precedent for a handle-carrying intent, its
  grant and its "did it write" check.
- `crates/core/src/runner/outcome.rs`: `finish_run`, `apply_to_task` and
  `move_to_in_review`. Reached from 036's `board::service::finish_run` and, until 043,
  from `scheduler/reconcile.rs`.
- `crates/core/src/board/service.rs`'s `claim`, `crates/core/src/runner/start.rs` (036)
  and `scheduler/queue.rs`'s `try_step`: the resume refusal and its two pass-throughs.
- `crates/core/src/scheduler/attempts.rs`: `history`, `resume_point` and
  `resume_as_implementation`.
- `crates/core/src/review/findings.rs` (035): `record`, `recorded_at`. `review/digest.rs`
  (034, 035): the loop count.
- `crates/core/src/mcp/scope.rs`, `server.rs`, `requests.rs` and `responses.rs`: the four
  configuration tools.
- `crates/core/src/db/settings.rs`: D3's key-owning shape.
- `crates/core/src/tasks/service.rs`: `TaskDetail` and `get_task`.
- `crates/core/src/worktree/git.rs`: `is_dirty`, beside which the tracked-only check goes.
- `crates/core/src/testing/cli.rs`: `FakeCli`. A general `calls_tool_on_attempt` helper,
  lifted from `tests/runner_strategy.rs`'s planner write-back, keeps each test from
  hand-writing the `curl`.
- `src-tauri/src/lib.rs`: both `generate_handler!` lists.
- `src-tauri/src/commands/review.rs` (034, extended by 035): the four configuration
  commands go beside `approve_task` and `list_review_findings`. It is not a new file.
- `docs/seam-contract.md`: D32's appendix, under 034's dated sub-heading.
- New files:
  - `crates/core/src/review_loop/` with `mod.rs`, `config.rs`, `decide.rs` and
    `history.rs`. It is a sibling of `review/`, which 034 and 035 created for the human's
    actions and the findings store. 034's "task 021 adds the loop" beside them is met by
    the sibling: the loop is the engine, and `review/` stays what a human or a reviewer
    writes.
  - `crates/core/tests/review_loop.rs`.

**Migration.** None of its own. Its columns ride in 035's
`src-tauri/migrations/20261001120100_run_kinds_and_review_findings.sql` (D28, D4
amendment).

**What the chain provides.**

- **033:** `runs.head_sha`, `runs.base_sha`, and a review bundle for every finished row
  of every kind.
- **035:** `RunKind`; `NewRun`/`StartRun` with a required `kind`; `review_findings` and
  `review::findings::{record, resolve, list, recorded_at}`; `runs.findings_recorded_at`,
  set by every `record` call including an empty one; `rimaia-run` and the unconditional
  denial; `Grant::{Review, Fix}` with their `run_access` columns; `resume_point` and the
  refusals this task removes; the loop fields on 034's digest; and the fixture showing
  that `--disallowedTools mcp__rimaia` does not deny `mcp__rimaia-run__*` (D30 point 8).
- **036:** `BoardPort` and `InProcessBoard`; `run_task(board, ctx, paths, config, claim,
  request)`; `finish_run` as the board's decision point, and `NextStep`; the manual
  starter in `runner/start.rs`; the contract suite.

**What the next tasks expect.**

- **037** wraps this task's four commands in `commands.ts`, adds commands for
  `review_loop::history`, and calls `summary` per card over one batched read. It renders
  `ReviewLoopSummary` on the card through a D12 amendment, and puts the cost from
  `observed_run_cost` next to the acknowledgement.
- **043** moves the lease's purpose at each `start_run`, relies on phases never leaving the
  claim, and takes reconcile per runner, which must keep reaching the same task-side step.
- **045** adds consent to the `Continue` decision and a revision to
  `review_instructions`.
- **055** carries `record_review_findings` over the port for connected runners.
- **057** applies the push postcondition to every successful phase, so the commit the loop
  ends on is always on the remote (ADR-0033's 2026-10-04 amendment).

**Size.** About 4.5k lines of diff, half of it tests. That is above task 035's
3.5–4k, so plan the cut before starting rather than at the ceiling. The decision function
and the prompts are small. The phase loop in `run_task`, the rows recorded between phases
and the argv tests are the bulk. If the diff runs past 4.5k, **cut the four configuration
doors** (commands, tools, their parity tests and D32 rows) and move them to 037, which has
their only UI consumer; 037's Notes, "If 021 cut its configuration doors", already carry
the matching instruction. The service functions and their tests stay here, because the
engine reads them. If the cut is taken, these criteria move to 037 with the doors: the
command and tool halves of `enabling_the_loop_requires_the_cost_acknowledged_spelling`
(the service half stays), the MCP criterion, the parity test, and the two criteria on
`commands/review.rs` and D32's appendix rows. Do not cut the verdict,
the builders or the ping-pong data. 037 cannot derive them without core SQL of its own.

**The failure mode to design against is false confidence**: a task marked reviewed and
clean that is not. That is why:

- nothing auto-advances to `done`;
- a clean verdict needs an explicit empty call;
- a review that changed the branch or the worktree is not a review;
- a failed phase lands as unreviewed and never as clean, on every path that closes a row;
- the loop count and the findings history are shown rather than a green tick.
