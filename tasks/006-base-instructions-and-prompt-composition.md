---
id: "006"
title: Base instructions and prompt composition
milestone: mvp
status: ready
depends_on: ["004"]
adrs: ["0009", "0012"]
size: S
---

# Base instructions and prompt composition

## Goal

Global instructions applied to every run, and the deterministic function that composes
them with a task's plan into the exact prompt the agent receives.

## Why now

Task 008 needs a prompt to send. Building composition separately keeps it unit-testable
without spawning a process.

## Scope

**Settings → Instructions**

- Markdown editor for `settings.base_instructions`.
- A default seeded on first launch, reflecting the intended workflow:

  > Commit as you work, with focused commits and clear messages.
  > Run the project's tests and linters before you finish.
  > When the work is complete, push the branch and open a pull request describing what
  > changed and why.
  > If you cannot complete the task, stop, commit what you have, and explain what is
  > blocking you.

- Live preview showing a fully composed prompt for a chosen task.

**Composition (`runner/prompt.rs`)**

- `compose_prompt(base, task, repo) -> String`, in fixed order (ADR-0009): base
  instructions → task context → plan → extra instructions.
- Labelled Markdown headings per section; empty sections omitted with their heading.
- Task context includes title, repository name, branch, base ref, and links rendered as a
  list.
- Template variables in base instructions: `{{task.title}}`, `{{task.branch}}`,
  `{{repo.name}}`, `{{repo.default_branch}}`, `{{task.links}}`. Unknown variables pass
  through verbatim — a typo must not kill an overnight queue.
- `compose_system_append(task, repo) -> String` for `--append-system-prompt`: the
  orchestrator constraints from ADR-0012 (unattended, nobody can answer questions, do not
  push to the default branch, stop and report rather than guess).
- `compose_resume_prompt(task) -> String`: the short continuation instruction used by
  retries, which must not re-send the full prompt.

## Out of scope

- Per-repository instruction overrides.
- Sending anything to a process (008).

## Acceptance criteria

- Unit tests assert exact composed output for: all sections present, empty plan, empty
  extra instructions, no links, unknown template variable, and every known variable.
- The preview in Settings matches what a run would receive, byte for byte.
- Editing base instructions does not alter any already-stored run prompt.

## Notes

Test against exact strings, not substrings. This function's output is the contract with
the agent; a stray heading change should fail a test, not surprise someone at 2am.
