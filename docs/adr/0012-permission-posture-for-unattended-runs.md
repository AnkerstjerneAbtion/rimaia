# 12. Permission posture for unattended runs

- **Status:** Accepted
- **Date:** 2026-08-20

## Context

Claude Code prompts for permission before edits and before commands that could be
destructive. Unattended runs have nobody to answer those prompts — a run that blocks on a
permission dialog at 1am is a run that accomplished nothing by morning.

The available headless modes are `acceptEdits`, `auto`, `bypassPermissions`, `dontAsk`,
`plan`, and `manual`. The work Rimaia is built for — implement a plan, run tests, commit,
push, open a PR — needs file edits *and* arbitrary bash, because test runners, package
managers, `git`, and `gh` are all bash.

This is the decision with the largest blast radius in the product, and it deserves to be
stated plainly rather than buried in a default.

## Decision

**Unattended runs are executed with `--permission-mode bypassPermissions`, and this is an
explicit, informed, per-repository opt-in.**

Mitigations, all of which are part of the feature rather than nice-to-haves:

1. **Per-repository opt-in.** Registering a repository does not make it runnable. The user
   must tick "allow unattended agent runs" for that repository, behind a dialog that
   states in plain language what it permits. Un-opted repositories can hold tasks; those
   tasks cannot be started.
2. **Worktree isolation** (ADR-0005). Runs execute in a dedicated worktree, never the
   user's checkout, so the worst common outcome is a bad branch rather than lost local
   work.
3. **Denied tools.** `--disallowedTools` blocks the operations with no legitimate place in
   an unattended implementation run — force-pushing, hard resets against remotes, branch
   deletion on the remote. The list is a setting so it can grow with experience.
4. **Orchestrator constraints via `--append-system-prompt`.** The run is told, at system
   level: it is unattended, nobody can answer questions, it must not push to the default
   branch, and it must stop and report rather than guess when it cannot proceed.
   ADR-0009 keeps this channel reserved for exactly this.
5. **Full transcript** (ADR-0013). Every tool call is recorded and reviewable.
6. **A conservative default for interactive runs.** A run started manually with the app in
   the foreground defaults to `acceptEdits`; bypass is for the unattended path.

The dialog does not soften this. It says: the agent can run any command in this
repository's worktree, including network access and package installation, without asking.

## Consequences

- Unattended runs actually complete, which is the product.
- The security posture is honest and localized: it is written down here, surfaced in the
  UI, and scoped per repository, rather than being an undocumented flag.
- A prompt-injected or simply mistaken agent can do real damage inside the worktree, and
  can reach the network. The isolation limits *where*, not *what*. A user running this on
  a machine with production credentials in the environment should know that.
- The denied-tools list is a blocklist, and blocklists are incomplete by construction. It
  reduces the common accidents; it is not a sandbox.
- Post-MVP options if this proves too permissive: an allowlist mode (`--allowedTools`) for
  repositories that only need a known set of commands, and OS-level sandboxing of the
  child process.

## Alternatives considered

- **`acceptEdits` for unattended runs.** Allows edits but still prompts on many bash
  commands, so runs stall silently overnight — the exact failure the product exists to
  avoid.
- **`--allowedTools` allowlist.** Materially safer and genuinely appealing, but every
  repository needs a different list (its own test runner, build tool, package manager),
  and an incomplete list produces the same overnight stall. A good post-MVP addition once
  there is data on what real repositories need.
- **Container or VM per run.** The strongest isolation, and it breaks subscription auth,
  local toolchains, and `~/.claude` configuration — the things that make the run behave
  like the user's own Claude Code (see ADR-0005).

---

## Amendment, 2026-08-28 — the planner is a third posture, narrower than both

This ADR names two invocation shapes: `bypassPermissions` for unattended runs, and
`acceptEdits` for a run the user started with the app in front of them. ADR-0016's strategy
run (task 020) is a third, and it is narrower than either.

| Shape | Permission mode | What it may write |
| --- | --- | --- |
| Unattended implementation run | `bypassPermissions` | Edits and arbitrary bash, behind the per-repository opt-in |
| Interactive implementation run | `acceptEdits` | Edits; bash still prompts, and someone is there to answer |
| **Strategy run (planner)** | **`acceptEdits`** + `--allowedTools mcp__rimaia__set_task_strategy` | **Nothing. `Write`, `Edit`, `NotebookEdit` and `Bash` are denied** |

`acceptEdits` for a run that is unattended looks like a direct contradiction of the
Decision above, whose whole argument is that an unattended run stalls on prompts. Both hold
at once — but **only because the planner's one tool is pre-approved by name.**

> **Correction, 2026-08-31.** This paragraph first claimed that "a stall needs a tool that
> asks, and the planner has none," and that with the four writing tools denied there was
> "nothing left in its reach that Claude Code would prompt about." That is false, and it was
> false in the way that matters most: **`acceptEdits` auto-approves file edits and does not
> auto-approve MCP tool calls.** The planner's single `mcp__rimaia__set_task_strategy` call
> raises a permission request like any other, an unattended session has nobody to grant it,
> and the CLI refuses it. Observed on the first real run — the transcript records
> `"Claude requested permissions to use mcp__rimaia__set_task_strategy, but you haven't
> granted it yet."`, the run then ended looking successful, and every planned task fell back
> to the default. Recorded rather than quietly edited because the wrong claim is a plausible
> one, and the next person to reason about a narrow posture for an unattended run will reach
> for it again.

The fix keeps the narrow posture rather than abandoning it: the planner is granted **exactly
its own write-back** through `--allowedTools`, and nothing else. That is strictly tighter
than `bypassPermissions`, which would approve every tool the blocklist has not already taken
away. The weaker mode still buys what it was chosen for: if a future CLI ships a writing tool
this blocklist does not name, `acceptEdits` is what stops it from being used unattended, and
`bypassPermissions` would not.

**An unattended run must be able to answer every prompt it can raise, by construction.** For
an implementation run that means `bypassPermissions`, because its needs are open-ended. For
the planner it means one named tool, because its needs are exactly one. Either way the test
is the same, and it is the test this amendment originally failed to apply.

The denied list is the implementation blocklist **plus** those four, not instead of it —
the planner has no more business force-pushing than an implementation run does. This ADR's
caveat that a blocklist is incomplete by construction still applies, but it applies more
weakly here than anywhere else in the product: "read the plan and answer" is close to
expressible as a denial of four tools, whereas an implementation run's needs are
open-ended by definition, which is exactly why mitigation 3 could not carry the whole
argument for it.

Nothing else is relaxed. The per-repository opt-in (mitigation 1) is checked once, before
either child is spawned, so a repository that has not opted in runs no planner either.
Worktree isolation (mitigation 2) is unchanged: the strategy run creates no worktree and no
branch of its own, and executes with its cwd set to the worktree the implementation run is
about to use. `--append-system-prompt` (mitigation 4) still carries the orchestrator facts,
now including the task id the run may address and the tool it must answer with. The
transcript (mitigation 5) is written for the planner too, under its own name — seam-contract
D17.
