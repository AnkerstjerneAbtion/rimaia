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
