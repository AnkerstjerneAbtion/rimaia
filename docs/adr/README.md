# Architecture Decision Records

Rimaia — *Review in the morning, agent in the afternoon.*

These records capture the decisions that shape the product. They are written in a
lightweight [MADR](https://adr.github.io/madr/) style: context, decision, consequences,
alternatives.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-local-first-tauri-desktop-app.md) | Local-first Tauri desktop app | Accepted |
| [0003](0003-sqlite-as-the-local-store.md) | SQLite as the local store, schema owned by Rust | Accepted |
| [0004](0004-drive-claude-code-via-headless-cli.md) | Drive Claude Code through the headless CLI | Accepted |
| [0005](0005-git-worktree-per-task.md) | One git worktree and branch per task | Accepted |
| [0006](0006-embedded-local-mcp-server.md) | Embedded local MCP server over HTTP | Accepted |
| [0007](0007-task-model-and-kanban-columns.md) | Task model, four Kanban columns, position as priority | Accepted |
| [0008](0008-dependency-semantics-and-branch-chaining.md) | Dependencies unblock on successful run, with branch chaining | Accepted |
| [0009](0009-prompt-composition.md) | Prompt composition: base instructions + plan + extra instructions | Accepted |
| [0010](0010-execution-scheduler.md) | Execution scheduler: sequential or parallel, run windows | Accepted |
| [0011](0011-resilience-usage-limits-and-resume.md) | Resilience: usage-limit detection, backoff, session resume | Accepted |
| [0012](0012-permission-posture-for-unattended-runs.md) | Permission posture for unattended runs | Accepted |
| [0013](0013-run-logging-and-observability.md) | Run logging: JSONL transcripts plus indexed summaries | Accepted |
| [0014](0014-mvp-scope-and-non-goals.md) | MVP scope and non-goals | Accepted |
| [0015](0015-testing-strategy-and-crate-split.md) | Testing strategy and core/shell crate split | Accepted |
| [0016](0016-per-task-execution-strategy.md) | Per-task execution strategy: model, effort, planned workflows | Accepted |
| [0017](0017-review-and-fix-loop.md) | Post-implementation review-and-fix loop | Accepted |
| [0018](0018-core-to-shell-change-events.md) | Change events from core to the shell | Accepted |
| [0019](0019-mutation-source-and-service-context.md) | Mutation source, and where it lives on the service context | Accepted |
| [0020](0020-per-repository-git-credentials.md) | Per-repository git credentials, held by Rimaia | Accepted |
| [0021](0021-mcp-first-capability-parity.md) | MCP-first: the tool surface is the whole product | Accepted |
| [0022](0022-what-a-run-is-remembered-by.md) | What a run is remembered by, and what survives pruning | Accepted |

## Conventions

- Filenames: `NNNN-kebab-case-title.md`, numbered sequentially, never renumbered.
- Status is one of `Proposed`, `Accepted`, `Superseded by ADR-NNNN`, `Deprecated`.
- An ADR is never edited to change its decision. Write a new one that supersedes it.
- Tasks in [`tasks/`](../../tasks/README.md) reference the ADRs they implement.
