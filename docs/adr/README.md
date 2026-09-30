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
| [0023](0023-an-overridable-data-directory.md) | An overridable data directory, so one branch cannot break every other | Accepted |
| [0024](0024-a-calm-interface-that-travels-to-the-web.md) | A calm interface, and one that travels to the web | Accepted |
| [0025](0025-archiving-a-task-and-what-it-may-clean-up.md) | Archiving a task, and what an archive is allowed to clean up | Accepted |
| [0026](0026-a-provider-seam-for-the-agent-cli.md) | A provider seam for the agent CLI, drawn from Rimaia's needs | Accepted |
| [0027](0027-a-server-for-the-board-and-runners-for-the-work.md) | A server for the board, runners for the work | Proposed |
| [0028](0028-the-server-owns-the-board-and-each-runner-keeps-its-own-store.md) | The server owns the board's database, and each runner keeps its own store | Proposed |
| [0029](0029-teams-membership-and-isolation.md) | Teams, membership, and isolation between them | Proposed |
| [0030](0030-identity-people-sign-in-machines-pair.md) | Identity: people sign in, machines pair, sessions carry tokens | Proposed |
| [0031](0031-runners-claim-work-with-leases.md) | Runners claim work with leases, and retries stay on the machine that started them | Proposed |
| [0032](0032-assignment-and-consent-to-run-on-a-machine.md) | Assignment, and consent to run someone's plan on your machine | Proposed |
| [0033](0033-repositories-belong-to-the-team-checkouts-to-the-runner.md) | Repositories belong to the team; checkouts, branches and credentials belong to the runner | Proposed |
| [0034](0034-one-api-for-the-web-and-the-desktop.md) | One API for the web and the desktop, split into board and local commands | Proposed |
| [0035](0035-mcp-when-the-board-is-remote.md) | MCP when the board is remote | Proposed |
| [0036](0036-transcripts-and-review-artifacts-leave-the-machine.md) | Transcripts and review artifacts leave the machine | Proposed |
| [0037](0037-hosting-backups-and-version-skew.md) | Hosting, backups, and version skew between the server and its runners | Proposed |

## Conventions

- Filenames: `NNNN-kebab-case-title.md`, numbered sequentially, never renumbered.
- Status is one of `Proposed`, `Accepted`, `Superseded by ADR-NNNN`, `Deprecated`.
- An ADR is never edited to change its decision. Write a new one that supersedes it.
- Tasks in [`tasks/`](../../tasks/README.md) reference the ADRs they implement.
