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
