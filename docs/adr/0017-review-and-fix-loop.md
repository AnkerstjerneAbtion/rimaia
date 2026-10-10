# 17. Post-implementation review-and-fix loop

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

An unattended implementation run produces a branch. Whether that branch is any good is
currently discovered by a human, in the morning, which is the expensive part of the loop.

Much of what a morning review catches is mechanical: the tests were not run, a case in the
plan was missed, an obvious bug, a lint failure, a half-finished edit. An agent with fresh
context and a review brief catches most of it — and can fix it — while the human is
asleep. What should reach the morning is the work that needs judgement, not the work that
needs another pass.

This lands late. It only pays off once implementation runs are reliably producing
reviewable branches, and it multiplies token spend per task.

## Decision

After a successful implementation run, a task may enter a bounded **review-and-fix loop**
before it reaches `in_review`.

### The loop

```
implement → review → findings? → fix → review → … → clean, or loop budget spent
```

- **Review phase**: a fresh Claude Code session in the same worktree, with no
  implementation context, given the diff, the original plan, and the review instructions.
  Fresh context is the point — a session reviewing its own work grades itself generously.
- **Findings** are written back to the task through the scoped MCP handle from ADR-0016,
  each with severity and location.
- **Fix phase**: a run — resumed implementation session or fresh, configurable — that
  addresses the findings and commits.
- **Loop budget**: `max_review_loops`, default 2. Also bounded by the run window.
- **Exit**: review returns no findings above the configured severity → `in_review`, clean.
  Budget spent with findings remaining → `in_review` with the findings attached, flagged.
  Review run fails → `in_review` unreviewed, flagged. **The loop never sends a task to
  `done`**; a human still approves.

### Review instructions

A global `review_instructions` setting, alongside base instructions (ADR-0009), plus an
optional per-task override. Composed the same way, into the review prompt.

Because runs execute the user's own Claude Code, review instructions may simply invoke an
existing review skill or slash command by name. Rimaia does not ship a review methodology
— it schedules whatever the user already trusts.

### Configuration

Per task and per repository: loop enabled, `max_review_loops`, severity threshold for what
counts as blocking, model and effort for the review phase (a review is often worth more
effort than the implementation was), and whether the fix phase resumes or starts fresh.

### What the morning sees

The review history is part of the task: each loop's findings, what was fixed, what
remains. The review view (task 015) shows the final diff plus the unresolved findings —
so the human starts from "here is what the reviewer could not fix" rather than from
nothing.

## Consequences

- Morning review starts higher up the stack. This is the point.
- Token cost per task multiplies by roughly the loop count. Hence: off by default, opt-in
  per repository or per task, and explicitly bounded.
- Fresh-context review catches what self-review does not, at the cost of the reviewer not
  knowing why a decision was made. Findings are advisory to the fix phase, not commands —
  the fix run is told it may reject a finding with a reason, and the rejection is recorded.
- A loop that ping-pongs (fix introduces a new finding, review flags it, repeat) is
  bounded by the budget rather than by cleverness. Ping-ponging is itself a signal worth
  surfacing on the card.
- This composes with ADR-0016: the review phase is a natural place for a higher-effort
  model than the implementation used.
- Risk of false confidence — a task marked "reviewed, clean" that is not. Mitigated by
  never auto-advancing to `done`, and by showing loop count and findings history rather
  than a green tick.

## Alternatives considered

- **Review as a separate task type on the board.** Fits the existing model, and makes
  every implementation task require a manually created partner task. More board noise, and
  the dependency chain does the wrong thing on failure.
- **Self-review inside the implementation run** ("check your work before finishing").
  Nearly free, and the weakest form — same context, same blind spots. Worth keeping in the
  base instructions regardless; it is not a substitute.
- **Unbounded looping until clean.** Sounds better, ends with a task consuming the entire
  night on a finding it cannot fix.
- **Review without fix** (report only). Cheaper and safer, and leaves the human doing the
  mechanical work the loop exists to remove. Available as a configuration —
  `max_review_loops: 0` with review enabled — rather than as the design.

---

## Amendment, 2026-10-09 — the engine (task 021)

What building the loop decided, each one a rule the code holds in one place.

### Phases, not rows

A **phase** is a maximal run of contiguous `runs` rows sharing `(kind, session_id)`, seam-contract
D29 point 3's budget boundary: a review that hits a usage limit and resumes is one phase across
two rows. Three things read phases and never rows:

- **The budget and every loop count.** `max_review_loops` counts **fix phases**, and a fix is
  always followed by a review, so the default of 2 is at most three reviews and two fixes. `0`
  with the loop on is the report-only mode: one review, no fix. A retried review never spends
  the budget twice. The digest's loop number counts review phases (D29 point 8's amendment).
- **The witness.** A review phase has recorded when any of its rows has
  `runs.findings_recorded_at` set (D30 point 7), never from a count of findings.
- **`HEAD` moved.** The phase's last row's `head_sha` against the row immediately before its
  first. A reviewer that committed and then resumed has still moved `HEAD`. Either value
  missing reads as moved: a branch that cannot be compared is not a clean one.

The loop belongs to the newest implementation phase; the rows after it are the current loop.
Run now starts from implementation again, and the budget resets with it. A loop never starts a
phase after the runner's run window closes (`FinishRun::window_closes_at`, D24 point 4); the
phase that is running finishes.

### The exits, decided by the board on every path that closes a row

`review_loop::decide` is one pure function, called from `outcome::finish_run`'s task-side step,
which is reached from the board's `finish_run` and from reconcile alike. The runner reports and
never counts.

| Row that finished | Condition | Next | Task |
| --- | --- | --- | --- |
| implementation, any failure | — | as before | as before (ADR-0011) |
| implementation, success | loop off, or window closed | released | `in_review`, `idle` |
| implementation, success | loop on, window open | review | stays `running` |
| review, success, recorded, `HEAD` unmoved | no open blocking finding | released | `in_review`, **clean** |
| same | blocking, fixes spent < budget, window open | fix | stays `running` |
| same | blocking, budget spent or window closed | released | `in_review`, findings remain |
| review, success | not recorded, or `HEAD` moved | released | `in_review`, unreviewed |
| fix, success | window open (and the loop still on) | review | stays `running` |
| fix, success | window closed | released | `in_review`, unreviewed |
| review or fix | retryable, `resume_after` set | released | `waiting_retry`; resumes as the same kind |
| review or fix | fatal, cancelled, or retries spent | released | `in_review`, unreviewed |

A failed or cancelled review or fix lands in `in_review`, never `failed`, because the
implementation had already succeeded — reconcile's path included. While the loop continues the
task stays in its column, `running`, and nothing is written to it; the move to `in_review` is
made once, when the loop exits. **No exit moves a task to `done`.** The configuration is read
again at each boundary, so a setting changed mid-loop takes effect at the next one; a loop
turned off mid-way lands its task after the phase that is running.

### The verdict, derived and never stored

`None` (the loop is off and never touched the task), `Clean`, `FindingsRemain`, or
`Unreviewed` with one of `not_reviewed`, `review_failed`, `nothing_recorded`,
`review_changed_branch` and `fix_not_reviewed`. A task is flagged when its verdict is the last
two. A succeeded implementation with nothing after it reads the configuration now: unreviewed
while the loop is on, `None` while it is off. The builders that compute it are pure, so task 037
calls them per card over one batched read.

### Findings

- **The fingerprint** is the file and the title, each trimmed, lowercased and with whitespace
  collapsed, joined by `|`; the line is left out, because a fix moves lines. It is computed by
  the store's one writer, never supplied by a reviewer.
- **A rejection stands.** A finding whose fingerprint matches one a fixer rejected on the same
  task is stored `rejected`, resolved "Rejected earlier as <id>: <reason>" after the first such
  rejection. It never blocks and never reaches a fixer, and it is kept rather than dropped, so
  the history shows the reviewer raised it again. A reviewer is shown the rejections, with their
  reasons, so it need not.
- **Blocking** is `severity >= blocking_severity`, ordered `critical > high > medium > low`.
  A fix receives the newest review's open blocking findings and nothing else; the rest stay
  attached as advisory, and an earlier review's unresolved ones are raised again by the next
  fresh review if they still apply.
- **Ping-pong** is data: for each review after the first, the findings the preceding fix marked
  fixed and the reviewer raised again (`regressed`), and the blocking findings the preceding
  review did not raise (`new_after_fix`). It is a signal on the card — "may be going in circles"
  — and never changes an exit; the budget bounds the loop.

### Configuration

Per task, per repository and globally, field by field, then the built-in defaults: off, 2,
`medium`, the task's own strategy for the review's model and effort, a fresh fix session. **There
is no boolean**: `enabled` is `off` or `on_cost_acknowledged`, following D20's
`on_done_acknowledged`, and a door refuses `true` or `"on"`. A stored value that does not parse
— a hand-edited `true` — reads as nothing set, which is off, so a typo cannot enable a spend.
`max_review_loops` is bounded at five: "bounded" is this ADR's word, and five fixes is a night.
A review model or effort must be an id the strategy catalogue lists. The review instructions'
task override **replaces** the global text rather than adding to it, and a blank override falls
back. The global settings are two team-placed keys, `review_instructions` and `review_config`;
no row is seeded. The four configuration doors are refused to every run grant: a run that could
enable its own loop would spend on its own authority.

### The phases themselves

A review is a fresh session that never resumes and gets no implementation context; a fix opens a
fresh session, or with `fix_session = resume` continues the newest implementation row's session
(never the review's, and never through `resume_point`), falling back to a fresh one with a note
on the row's transcript when there is none or the provider cannot continue. Both follow
`run_environment` (ADR-0004's amendment) and take the claim's posture with one pre-approved tool
(ADR-0012's amendment). A review's worktree is checked for tracked changes before it spawns and
after it exits.

### Rows recorded without a spawn, and the residual

Between phases nothing ends through a bare `release`, which would move a succeeded
implementation's task to `failed`. A phase that cannot start — no MCP endpoint bound, a refusal
by the provider, a missing worktree, a dirty one before a review, a board read that failed — is
recorded as a row of its kind with the refusal as its `error_message`, and closed `fatal`
through `finish_run`; a Cancel that arrives while no process runs is a `cancelled` row. The exit
table then lands the task, and the history shows why. Such a row has an empty prompt and gets
an empty transcript at its `log_path`, so the startup survey does not report it missing. This
relaxes the initial schema's note on `runs` that a row "exists here only once a process was
spawned for it".

**The named residual:** if that row cannot be written, the runner falls back to `release`, and
the task lands `failed`; so does a crash between the board's answer to continue and the next
row's `start_run`, which reconcile finds `running` with no open row. In both, the board could
not be written, or nothing was alive to write it.
