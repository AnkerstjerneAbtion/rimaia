---
id: "021"
title: Review-and-fix loop
milestone: v0.4
status: not-ready
depends_on: ["015", "017", "020"]
adrs: ["0017", "0016"]
size: L
---

# Review-and-fix loop

## Goal

After a successful implementation run, review the work with a fresh agent, fix what it
finds, and repeat a bounded number of times — so the morning starts from "here is what
could not be fixed" instead of from an unreviewed diff.

## Why now

It doesn't yet. This is deliberately late: it only pays off once implementation runs
reliably produce reviewable branches, and it multiplies token spend per task. Recorded now
so the runner, prompt composition, and scoped MCP handle are built with it in mind rather
than retrofitted around it.

`status: not-ready` — the design is settled (ADR-0017), the details are not. Revisit after
task 017 has been used for real mornings, which is what will show whether mechanical
findings actually dominate the review.

## Scope

**Loop**

```
implement → review → findings? → fix → review → … → clean, or budget spent
```

- **Review phase**: fresh Claude Code session, same worktree, **no implementation
  context** — given the diff, the original plan, and the review instructions. Fresh
  context is the mechanism; a session reviewing its own work grades itself generously.
- **Findings** written back to the task through the scoped MCP handle from task 020, each
  with severity and location.
- **Fix phase**: addresses findings and commits. May reject a finding with a recorded
  reason — findings are advisory, not commands.
- **Budget**: `max_review_loops`, default 2, also bounded by the run window.
- **Exits**: clean → `in_review`. Budget spent with findings → `in_review`, flagged, with
  findings attached. Review run failed → `in_review`, flagged unreviewed.
- **Never advances a task to `done`.** A human still approves.

**Instructions**

- Global `review_instructions` setting alongside base instructions, plus a per-task
  override. Composed by the same function (ADR-0009).
- Because runs execute the user's own Claude Code, review instructions may invoke an
  existing review skill or slash command by name. **Rimaia ships no review methodology of
  its own** — it schedules the one the user already trusts.

**Configuration** (per repository and per task)

- Loop enabled, `max_review_loops`, blocking severity threshold, review-phase model and
  effort (often higher than implementation used), fix phase resumes or starts fresh.

**UI**

- Review history on the task: each loop's findings, what was fixed, what was rejected and
  why, what remains.
- Task 015's run view shows the final diff plus unresolved findings.
- Ping-pong detection — a fix introducing a finding the next loop flags — surfaced as a
  signal on the card, not silently absorbed by the budget.

## Acceptance criteria

- A task with a deliberately introduced bug is caught by the review phase and fixed by the
  fix phase within the loop budget, unattended.
- Loop budget is respected; an unfixable finding does not consume the night.
- A task reaching the budget with findings still lands in `in_review`, flagged, with
  findings readable in the review view.
- A rejected finding records its reason and is not re-raised as new.
- Loop disabled by default; enabling requires acknowledging the token cost.
- Review runs are exercised by the fixture harness without spending tokens.

## Notes

**The scoped handle is denied to implementation runs today, and this task has to
undo that deliberately.** Task 020 found that an implementation run inherits the
operator's Claude Code config, which registers the *unscoped* `/mcp`, and that
`bypassPermissions` auto-approves MCP calls — so every run silently held
`move_task`, `create_task` and every configuration tool `RunScope` marks
`Refused`. The fix denies `mcp__rimaia*` to implementation runs regardless of the
operator's blocklist (`runner::process::rimaia_tools_denied_to_a_run`).

That denial is by tool name, and a scoped handle registers under the same server
name, so it will block this task's write-back too. Three ways out, to be chosen
rather than stumbled into: give the run-scoped handle its own server name;
apply the denial only when no grant was minted for that run; or establish that
`--allowedTools` overrides `--disallowedTools`, which task 020 did not verify.
Whichever is chosen, the property to preserve is the one the denial bought —
a run reaches its **own** card and nothing else.

The failure mode to design against is false confidence: a task marked reviewed and clean
that is not. Hence no auto-advance to `done`, and loop count plus findings history shown
rather than a green tick.
