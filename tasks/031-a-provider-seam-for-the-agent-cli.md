---
id: "031"
title: A provider seam for the agent CLI
milestone: v0.4
status: ready
landed: "#32"
depends_on: []
adrs: ["0026", "0004", "0011", "0012", "0015", "0016"]
size: L
---

# A provider seam for the agent CLI

## Goal

Draw the seam [ADR-0026](../docs/adr/0026-a-provider-seam-for-the-agent-cli.md) decides,
while Claude Code is still the only implementation: `RunIntent → SpawnPlan` for how an
intent becomes a child process, and `line → RunEvent` for what one line of its output
means. Everything else — process groups, signals, the transcript, classification, retry,
the board, the scheduler — stays Rimaia's and does not learn that a provider exists.

**This task does not add a second provider.** It proves the seam with a deliberately
alien test-only one, and Claude stays the only thing production ever spawns.

## Why now

The refactor is cheap against one implementation and expensive against two. Three modules
carry the entire Claude vocabulary inside `runner/` — `process.rs`'s argv, `events.rs`'s
wire extraction, `outcome.rs`'s terminal words — and everything around them is already
neutral. Drawing the seam now means the next provider is "implement the trait"; drawing
it later means untangling a second set of assumptions from whatever was built on top of
the first.

The failure mode this task exists to prevent is a trait that is Claude's flag surface
with renamed methods. A second provider then arrives as a degraded Claude: a session id
it must fabricate, a tool blocklist it must silently drop, an `--append-system-prompt` it
must fold into the prompt. ADR-0026's Context names the four structural mismatches the
design is validated against. **They are constraints on the design, not deliverables** —
nothing about any real second vendor belongs in this diff.

## Scope

**The module layout.** New: `runner/provider/mod.rs` (`AgentProvider`, `ProviderId`,
`SpawnPlan`, `Capabilities` and its axis enums, `negotiate`, `RunPlan`, `Refusal`),
`runner/provider/intent.rs` (`RunIntent`, `ForbiddenOperation`, `ForbiddenKind`,
`SessionIntent`, `RimaiaHandle`), `runner/provider/claude.rs` (`ClaudeProvider` — the
moved `args()` body, `CLAUDE_CLI`, `SETTING_SOURCES`, `DEFAULT_DISALLOWED_TOOLS`, the
`ForbiddenOperation` → rule-string table, the `{"mcpServers":…}` document, and the Claude
wire parsing moved out of `events.rs`), and `testing/provider.rs`. Modified:
`runner/{mod,process,events,outcome,strategy}.rs`, `mcp/scope.rs`,
`testing/{cli,fixtures,mod}.rs`.

Every constant that moves is **re-exported at its old path**: `CLAUDE_CLI` and
`DEFAULT_DISALLOWED_TOOLS` keep working from `runner::process`, because the doctor imports
the first and `tests/runner_process.rs` imports both, and neither import line should
change. If someone later drops a re-export it is a compile error, never a silent break.

**The trait**, five methods, each justified by a Rimaia need rather than a Claude flag:
`id`, `default_program`, `capabilities` (so a run can be refused before state is
written), `plan_spawn` (so one attempt starts), `parse_line` (so the tail, the PR watch,
the turn counter and the classifier each keep exactly one consumer). Object-safe: no
generics, no `Self` by value, no `async fn`, no associated types. Every method is `&self`
and pure — a provider is a translation table, not a service.

**`RunIntent`**, which replaces `Invocation`. Nine fields keep their names and meanings;
two change and they are the whole point: `disallowed_tools: Vec<String>` becomes
`forbidden: Vec<ForbiddenOperation>` (git operations, plus a `ProviderRule { provider,
rule }` arm so an operator's own rule string is tagged rather than handed to a provider
that never spoke it), and `mcp_config: Option<String>` becomes `rimaia_handle:
Option<RimaiaHandle>`. `allowed_tools: Vec<String>` becomes `required_tools:
Vec<&'static str>` holding Rimaia tool names, which the provider spells in its own
convention. `RunIntent` stays `PartialEq + Eq`; the provider never goes on it.

**Capabilities and negotiation.** `negotiate(caps, intent) -> Result<RunPlan, Refusal>` is
a free function, not a trait method, so the rules are Rimaia's and every provider is
judged by the same ones. It is called in `run_task` **before** `probe_cli`, before
`prepare_worktree`, before `claim` — so a refusal leaves no worktree, no claim and no
`runs` row — and in `strategy::plan`, which maps a `Refusal` onto its existing
`Planned::Failed` route. `verify_permission_mode`'s rule is preserved exactly: a provider
reporting a posture other than the one asked for is fatal, for every provider, always.

**Session identity.** `runs.session_id` stops meaning "the provider's session id" and
starts meaning "the id of the conversation this attempt belongs to" — which is what
`scheduler::attempts` has always used it for. `NOT NULL` holds, `attempts::fold` and
`retry::AttemptHistory` are untouched, and **no migration is added**.

**The end and usage vocabulary.** `ResultEvent`'s `terminal_reason`/`subtype`/`is_error`
become `end: EndReason`; `RateLimitEvent` becomes `UsageWindow` with `UsageState` and
`WindowReopen::{At, After}`; `InitEvent.permission_mode` becomes typed. `EventStream`
latches `Exhausted`, and stamps `observed_at` off `ctx.clock` at the moment the line is
read. `RunOutcome::usage_limit_resets_at` keeps its type and its documented meaning;
`retry::decide`, `USAGE_LIMIT_FALLBACK_POLL`, the jitter, the run-window cap and
`pause::note_usage_limit` are not touched.

**The test-only provider**, `ledger` — not "mock", not "fake", and deliberately not named
after a real product, because naming it after one would invite a later agent to read its
fixtures as evidence about that product. It differs from Claude on **every** axis the
design claims to abstract: a `run` subcommand rather than `-p`, `--emit ndjson` rather
than `--output-format stream-json --verbose`, a self-minted conversation id, `--trust
full` rather than `--permission-mode`, a preamble file rather than
`--append-system-prompt`, `LEDGER_HOME` with a `tools.toml` inside rather than
`--mcp-config`, **no tool denial at all**, `kind`/`body` rather than `type`, a `finished`
event with a `why` and no `terminal_reason`, exit **137** rather than 143, a relative
usage window on every turn, **no cost figure**, and **two** identity prefixes.

Its fixtures live in `crates/core/tests/fixtures/ledger/`, a sibling of `cli/`, with a
`README.md` whose first sentence says nothing in it was recorded from any real program.
`testing/cli.rs`'s `write_script` is parameterised rather than copied, and its
`unhandled` mechanism widens from `$1` to a scan of the whole argv — that one change is
what catches a Claude flag entering through a second code path.

## Out of scope

`runner/prompt.rs` in its entirety — `tests/prompt.rs` asserts those strings exactly and
changing one byte is a noisy behaviour-visible diff. The doctor.
`ENVIRONMENT_SETUP_COST_USD`. `DEFAULT_CATALOGUE_JSON`. `probe_cli`'s hardcoded
`--version` and its "install Claude Code" sentence. `runs/transcript.rs`'s second,
independent Claude-shaped parser. `PERMISSION_DENIAL_MARKER` and `line_reports_a_denial`
(diagnostics only, by their own doc — for another provider `denied_tool_calls` is simply
0). All of `src/`, all of `src-tauri/`, every migration, `.sqlx/`. All of that is
[task 032](032-provider-vocabulary-outside-the-runner.md)'s, whose file this task also
writes.

The per-repository acknowledgement that would let an operator defeat a capability
refusal is out of scope too, and for a stated reason: it needs a column, which needs a
migration, which seam-contract D4 says is a stop-and-ask. Until it exists, an unattended
run on a provider without a blocklist simply refuses.

**Adding a real second provider is out of scope.** There must be no vendor name other
than Claude's anywhere in the diff.

## Acceptance criteria

- ADR-0026 exists with its eight numbered decisions, is listed in
  [`docs/adr/README.md`](../docs/adr/README.md), and no existing ADR was edited to change
  a decision.
- Seam-contract D27 exists in the four-part shape with its six points, and both 031 and
  032 have rows in its "How to use this" table.
- `tasks/031-*.md` and `tasks/032-*.md` exist and have rows in
  [`tasks/README.md`](README.md).
- `AgentProvider` has exactly five methods, is object-safe, and `RunnerConfig` still
  derives `Clone` and `Debug`.
- **`Invocation::args` no longer exists**, and no Claude flag string appears anywhere
  under `crates/core/src/runner/` outside `provider/claude.rs`.
- `mcp::RunHandles::mcp_config_json` no longer exists, and `crates/core/src/mcp/` contains
  no `mcpServers` string.
- `classify` is **not** a trait method, and `crates/core/src/runner/outcome.rs` contains
  no occurrence of `terminal_reason`, `aborted_streaming`, `error_max_turns` or
  `"allowed"`.
- An unattended run against a provider whose `enforceable` is missing an operation is
  refused with no worktree, no claim and no `runs` row. A manual run against the same
  provider proceeds and records what it could not enforce.
- A planner run against a provider that cannot inject a handle falls back to the
  `default` chain with a `failed` envelope on the card, and spawns no planner.
- `a_run_driven_by_the_second_provider_reaches_in_review` passes: a real child, a real
  pipe, a non-Claude stream, a task landing in `in_review` with its transcript on disk
  byte-identical to the fixture.
- `crates/core/tests/scheduler.rs` is byte-identical to `main`, and so are the argv
  assertion bodies in `crates/core/tests/runner_process.rs` and both existing structural
  tests in `crates/core/tests/harness.rs`. The Ledger corpus lives in its own directory.
- **No migration was added** and `.sqlx/` needed no regeneration. If a query changes,
  **stop** — the design drifted into needing a column, and D4 says that is a
  stop-and-ask.
- Every CI check passes: `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`.

## Notes

**The evidence that this refactor moved no behaviour is the set of test files that do
not change.** `tests/scheduler.rs` is the largest of them and the non-negotiable one:
anything forcing a change there means the provider was threaded through the scheduler,
which it must not be — order, capacity and retry are not provider-shaped.
`tests/runner_strategy.rs` stays byte-identical too, which is *achieved* by putting the
provider on `RunnerConfig` rather than in `strategy::resolve`'s parameters.
`tests/runner_events.rs` changes only where `permission_mode` becomes typed, because
`EventStream` takes its provider through a builder that defaults to Claude.
`tests/runner_outcome.rs` changes legitimately — the vocabulary it asserts on is the thing
being neutralised — but every test there keeps driving its input from a fixture on disk
through `EventStream`. **The moment one starts constructing a payload by hand, the
evidence is gone.**

**On CLAUDE.md's rule that the CLI is faked by replaying recorded fixture streams, not by
mocking a trait.** That rule is about the *test double*, not production architecture:
what `runner::process` must be right about is pipes, argv, stdin, exit codes and process
groups, and a trait double cannot exercise any of it. A second provider that still spawns
a real `#!/bin/sh` stand-in replaying a real fixture over a real pipe is the same
technique applied to a second vocabulary, and does not violate the rule. The one
genuinely uncomfortable place is classification, and the mitigation is a discipline rather
than a type — see the paragraph above.

**Where a leak would hide.** `EventStream`'s builder default and `RunnerConfig::default()`
both mean Claude, which is what keeps test churn near zero and also what would let a
missed wiring in `execute` pass silently. The guard is
`a_run_driven_by_the_second_provider_reaches_in_review`, where a missed wiring returns the
wrong `ExitClass`. `the_default_provider_is_still_claude_code` is the other half: it
catches the refactor shipping the test provider, which is a live risk once a second
`impl` lives in the same crate.

**Claude's `Capabilities` must be written so every intent the current code produces
negotiates clean.** A `negotiate` that refuses something that works today is the one way
this task can break a running queue, and `tests/scheduler.rs` staying byte-identical is
the proof that it did not.
