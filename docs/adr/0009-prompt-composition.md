# 9. Prompt composition: base instructions + plan + extra instructions

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Every run needs two kinds of instruction:

- **Workflow rules that apply to every task** — commit as you go, push the branch, open a
  PR when done, run the test suite before finishing. The user writes these once.
- **The task itself** — the plan produced during planning, plus a short per-task note
  ("skip the migration, it already landed").

These must combine into one prompt, deterministically, so that what the agent received can
be reconstructed later from a failed run.

## Decision

The run prompt is assembled by the backend as a single composed string, in a fixed order:

```
1. Global base instructions   (settings.base_instructions)
2. Task context               (title, repository, branch, base ref, links)
3. The plan                   (task.plan)
4. Extra instructions         (task.extra_instructions)
```

Each section is wrapped in a labelled Markdown heading so the boundaries are unambiguous
to both the model and a human reading the transcript.

- The composed prompt is delivered to the CLI **on stdin**, not as an argv string —
  plans routinely exceed safe argument lengths.
- Base instructions go in the **prompt**, not `--append-system-prompt`. They are workflow
  requirements the agent should treat as part of the job and may reason about, not
  invisible system-level constraints. `--append-system-prompt` is reserved for
  orchestrator facts the agent must not treat as negotiable (ADR-0012).
- **The exact composed prompt is persisted with the run** (`runs.prompt`). When a run goes
  wrong, the first question is always "what did it actually get" — reconstructing it from
  three tables and a settings row that has since changed is not an answer.
- Base instructions are edited in Settings, with a live preview showing a composed sample.
- **Template variables** are supported in base instructions:
  `{{task.title}}`, `{{task.branch}}`, `{{repo.name}}`, `{{repo.default_branch}}`,
  `{{task.links}}`. Unknown variables are left verbatim rather than erroring — a typo in a
  settings field should not kill an overnight queue.
- Empty sections are omitted entirely, including their heading.
- Resumed runs (ADR-0011) do **not** re-send the composed prompt. They send a short
  continuation instruction; the original prompt is already in the session.

## Consequences

- One place to change "always open a PR", applied to every future run.
- Failed runs are debuggable: the transcript and the exact prompt are both on disk.
- Changing base instructions does not retroactively alter past runs, because each run
  stored its own copy.
- Ordering is a real decision: instructions first sets expectations before the plan is
  read, extra instructions last so per-task overrides land closest to the model's most
  recent context. If this ordering proves wrong it is one function to change — the
  composition lives in a single module with unit tests over the output.

## Alternatives considered

- **Base instructions via `--append-system-prompt`.** Keeps the visible prompt clean, but
  hides operational requirements from the transcript and makes "why didn't it open a PR"
  harder to answer.
- **A per-repository `RIMAIA.md` in the repo.** Attractive, and partly redundant with
  `CLAUDE.md`, which Claude Code already reads. Base instructions are about *how Rimaia
  runs tasks*, not about the repository; keeping them in app settings avoids polluting
  every repo with orchestrator config.
- **No global instructions; put everything in each plan.** Guarantees drift and makes the
  planning agent responsible for remembering the workflow every single time.

---

## Amendment, 2026-08-28 — a fifth section, and where it goes

ADR-0016's planner (task 020) produces guidance the implementation run has to read: a
workflow shape, and when it fans out, a phase breakdown. That is a fifth section, and the
fixed order becomes:

```
1. Global base instructions   (settings.base_instructions)
2. Task context               (title, repository, branch, base ref, links)
3. The plan                   (task.plan)
4. Execution strategy         (task.strategy_plan, rendered as guidance)
5. Extra instructions         (task.extra_instructions)
```

**Fourth, not last.** The Consequences above already settled the tail: extra instructions
go last "so per-task overrides land closest to the model's most recent context". A sentence
the user typed for this one task outranks a proposal a planner generated, so letting
generated text be the last thing read would invert a rule this ADR argued rather than
inherited. Between the plan and the overrides is also where it belongs on its own merits —
it is about *how* to execute the plan immediately above it.

It obeys every rule the other four obey. Level-1 heading; omitted entirely, heading
included, when there is nothing to say — which is what keeps a task with no proposal
composing byte for byte what it composed before task 020, and task 006's preview criterion
intact. It renders only for a `multi_agent` proposal or one with phases; a single-agent
proposal has nothing to add to a prompt.

**It never restates model or effort.** Those are `--model` and `--effort`, applied by the
CLI. Putting them in prose as well invites the run to reason about a decision that has
already been made and cannot be changed from inside the session.

The section ends with the sentence that puts ADR-0016's boundary in the prompt rather than
only in an ADR: *"Rimaia does not run these phases; you do, in this session, with your own
subagents."*

### The strategy run composes a different prompt, and deliberately not this one

The planner's own prompt is built by a separate function that **takes no base instructions
at all** — not "passes an empty string", but has no parameter for them, so it cannot be
wired up by a later reader who assumes the omission was an oversight. Base instructions are
implementation workflow: commit as you work, run the suite, open a pull request. A planner
that opens a pull request is a defect, and the cheapest place to make that impossible is
the signature. Its section list is fixed in seam-contract D17.

---

## Amendment, 2026-10-09 — the review and fix prompts (ADR-0017, task 021)

Two more composed prompts, built by the same rules as the other two: level-1 headings, the
same separator, an empty section omitted with its heading, and no trailing newline. Tool
names are the provider's spelling at the run-scoped server, `rimaia-run`.

**The review prompt**, in order:

```
1. Your job                   (fixed: a fresh reviewer, judging what is committed)
2. Task context               (title, repository, branch, the loop's base ref, links)
3. The plan                   (task.plan)
4. Extra instructions         (task.extra_instructions)
5. The change                 (base ref, base and head commit, the bundle's stat and files,
                               and the two git commands that read the whole branch)
6. Review instructions        (the task's override, else the global text; expanded)
7. Findings already rejected  (each with the fixer's reason)
8. How to answer              (exactly one record_review_findings call; [] when clean)
```

Like the planner's, **it has no base-instructions parameter**: base instructions say "open a
pull request", and a reviewer that opens one is a defect. `# The change` never embeds the
patch: the worktree holds it whole, and a capped copy would be truncated silently.

**Review instructions are an override, not an addition.** A task's `review_instructions`,
when it says anything, replaces the global text; a blank one falls back to it. An override
that added would run two review skills on one change. ADR-0009's five template variables
expand there as in base instructions, and an unknown one stays verbatim. With neither set
the section is omitted and `# Your job` stands alone: Rimaia ships no review methodology.

**The fix prompt**, in order: `# Base instructions` · `# Task context` · `# Plan` ·
`# Extra instructions` · `# Findings to address` · `# How to answer`. Base instructions are
included, because a fix is implementation work. `# Findings to address` renders each of the
newest review's open blocking findings with its id, severity, location, title and body, and
nothing else. `# How to answer` says a finding is advice, not an order, and asks for one
`resolve_review_finding` per finding, `fixed` with what changed or `rejected` with why.

A fix that continues the implementation's own session (`fix_session = resume`) is sent only
the last two sections: the session already holds the rest, the rule this ADR states for every
resumed run.

`# Task context` names the base the loop's rows recorded, which since task 011 can be a
dependency's branch, so it agrees with `# The change`. The implementation and planner prompts
still pass the repository's default branch, and their bytes are unchanged.

**Resumed after a retryable exit**, in place of the implementation's continuation — whose
"continue the task" would tell a reviewer to implement — one paragraph each:

- a review that has not yet recorded: *Continue the review of "{title}" from where you
  stopped. The change and the review instructions are earlier in this session — do not start
  over. Do not edit, commit or push. Finish by calling {tool} exactly once, with findings: []
  if you found nothing.*
- a review that already recorded: *Continue the review of "{title}" from where you stopped.
  Your findings are already recorded — do not call {tool} again, and do not edit, commit or
  push. Stop once you have finished what you were doing.*
- a fix: *Continue addressing the review findings on "{title}" from where you stopped. The
  findings and the instructions are earlier in this session — do not start over. Call {tool}
  for each finding you have not resolved yet, with fixed and what you changed, or rejected and
  why.*

The two review variants exist because the store refuses a second record call from one row,
not from a second row of the same review; the recorded variant is what keeps a resumed
reviewer from recording twice. The exact strings are pinned in `tests/prompt.rs`.
