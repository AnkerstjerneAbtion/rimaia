# Rimaia

*Review in the morning, agent in the afternoon.*

A local-first desktop app for queueing implementation plans and letting Claude Code work
through them unattended — in git worktrees, on your own subscription — so the results are
waiting to be reviewed the next morning.

- **Kanban board** — Not ready · Ready · In review · Done. Board order is execution order.
- **Plans in, branches out** — each task carries a plan, extra instructions, and links.
- **Git worktree per task** — your checkout is never touched.
- **Local MCP server** — hand plans to Rimaia from the Claude Code session you planned in.
- **Unattended runs** — sequential, scheduled, resuming through usage limits.

Rimaia does not bundle Claude Code and does not proxy an API key. It drives the `claude`
CLI you already have, signed in as you already are.

## Status

The MVP walking skeleton runs: the board, the store, the worktree service, the runner, the
sequential queue, the MCP server, run history, per-task strategy, and the preflight doctor.
Thirteen of twenty-four tasks have landed. See [`tasks/README.md`](tasks/README.md) for
what is done and what is next — a task with a `landed:` line is built.

It has not yet been through a long unattended night on someone else's machine. Treat it as
working software you should still watch the first few times.

## Prerequisites

Rimaia checks all of these for you at launch — see [The doctor](#the-doctor) — but it
cannot install them.

| Requirement | Why | Minimum |
| --- | --- | --- |
| [Claude Code](https://claude.com/claude-code) on `PATH`, signed in | Rimaia spawns it for every run | 2.1.234 |
| `git` | One worktree per task | 2.17 |
| [`gh`](https://cli.github.com), authenticated | Only if your base instructions ask for a pull request | any |
| Free disk space | Worktrees and transcripts | 1 GB to run, 5 GB comfortable |

`claude` is a **prerequisite, not a dependency**: it is never bundled and never installed
for you (ADR-0004). Verify it with `claude --version` and `claude auth status` before
expecting a queue to work.

To build from source you also need Node.js 20+, Rust stable via `rustup`, and your
platform's [Tauri prerequisites](https://tauri.app/start/prerequisites/) — Xcode Command
Line Tools on macOS; MSVC build tools and the WebView2 runtime on Windows;
`webkit2gtk-4.1`, `libappindicator3`, `librsvg2` and `patchelf` on Linux.

## First run

The app opens on a four-step welcome screen. Each step is marked done from your actual
configuration rather than from having been clicked, so you can stop halfway, quit, and pick
up where you left off — and skip anything you had already set up.

1. **Register a repository.** Rimaia works in worktrees created *outside* your checkout,
   under its own data directory. Your working tree is never touched.

2. **Enable unattended runs, per repository.** An overnight run cannot stop to ask
   permission, so it uses `--permission-mode bypassPermissions` behind an explicit
   per-repository opt-in (ADR-0012). Settings states the grant in full before you agree,
   and it is broad: **the agent can run any command in that repository's worktree,
   including network access and package installation, without asking.** Off by default, and
   worth granting only for repositories you would let a contractor push branches to.

3. **Set base instructions.** Prepended to every run's prompt: how to branch, when to
   commit, whether to open a pull request, what "done" means. This is the highest-leverage
   text in the app — a queue is only as good as the standing instructions every task
   inherits.

4. **Add the MCP server.** Lets a Claude Code session hand a finished plan straight to the
   board instead of implementing it there and then. Settings → MCP shows the exact command,
   built from the address the server actually bound:

   ```bash
   claude mcp add --transport http rimaia http://127.0.0.1:4517/mcp
   ```

   Copy it from Settings rather than from here — the port is configurable, and if it was
   taken at startup the real address will differ.

Then put a task in **Ready** and press Start. Board order is execution order; there is no
separate priority field (ADR-0007).

## The doctor

Every check below corresponds to a way an overnight queue can waste a night. It runs at
launch, before a scheduled queue starts, and whenever you press **Check again** in
Settings → Environment.

| Check | What it prevents |
| --- | --- |
| `claude` on `PATH`, recent enough | Every run failing immediately |
| `claude` signed in | Every run failing with an auth error |
| `git` new enough for worktrees | Worktree creation failing |
| `gh` present and authenticated, per repository | Asking for a pull request that cannot be opened |
| App data directory writable | Nothing persisting |
| Free disk space | Worktrees and logs failing mid-run |
| Registered repository paths still valid | Runs failing at worktree creation |
| MCP port free | Handoffs from other sessions silently going nowhere |

**A failure blocks the queue from starting and says exactly what to fix. A warning does
not.** The split is whether the queue can still do its job: no `claude` binary is a
failure, an unauthenticated `gh` is a warning that names the repository it affects, because
the runs still work and only the pull-request step is skipped.

The refusal lives in the core service, on queue start and resume, so the MCP server and any
future caller inherit it rather than each re-implementing it. It is a gate at the door, not
a guard in the corridor: a queue already running is never halted mid-flight by a transient
blip. See [seam-contract D21](docs/seam-contract.md).

## Running from source

```bash
npm install
npm run tauri dev
```

## Building a bundle

```bash
npm run tauri build
```

Bundles are written to `src-tauri/target/release/bundle/`. **Tauri does not
cross-compile** — build each target on its own OS or in CI. Expect the release profile to
want several GB on top of any debug build you already have.

### macOS

`npm run tauri build` produces `Rimaia.app` and a `.dmg`. Unsigned, it runs on the machine
that built it; Gatekeeper will refuse it on anyone else's until it is signed and notarised.
To sign, set these before building:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="app-specific-password"   # not your Apple ID password
export APPLE_TEAM_ID="TEAMID"
```

A "Developer ID Application" certificate from a paid Apple Developer account is what allows
distribution outside the App Store. Without notarisation, first launch needs
right-click → Open, or `xattr -dr com.apple.quarantine /Applications/Rimaia.app`.

The bundle's icons are still Tauri's default placeholder artwork. Replacing them is a
design decision nobody has made yet; `npm run tauri icon path/to/icon.png` regenerates the
whole set from a single 1024×1024 source.

### Windows and Linux

Build on the target OS. Windows produces an MSI and an NSIS installer and needs the WebView2
runtime present; Linux produces a `.deb` and an AppImage. Neither is signed, and neither has
been exercised as carefully as the macOS path.

## Where your data lives

Everything is local. There is no server, no account, and no telemetry.

| What | Where |
| --- | --- |
| Database, logs, worktrees | The OS application-data directory — Settings → Storage shows the exact path and opens it |
| Worktrees | Under that directory, **never inside a repository** |
| Run transcripts | Beside the database; prunable by age from Settings → Storage |

If a double-clicked bundle fails to open at all, the reason is in
`<app-data>/logs/rimaia.log`. Startup failures exit before any window exists, so that file
is the first place to look — the doctor cannot report them, because it only runs once
startup has succeeded ([seam-contract D11](docs/seam-contract.md)).

## Cost

Runs inherit your Claude Code configuration by default, including your MCP servers, because
those are capability a run may need. Inheriting costs roughly **3.6× per run** versus a
stripped environment. One toggle in Settings switches to `strict_local`
(`--strict-mcp-config --setting-sources project,local`) if you would rather pay less and
hand runs less. Inherited `CLAUDE_*` environment variables are always stripped either way —
those are process identity, not user config.

## Design

- [Architecture decision records](docs/adr/README.md) — 22 ADRs covering the stack, how
  Claude Code is invoked, worktree strategy, dependency semantics, permission posture,
  testing, and scope.
- [Seam contract](docs/seam-contract.md) — decisions too small for an ADR but shared by
  more than one task.
- [Task backlog](tasks/README.md) — 24 tasks; the table defines the order.
- [CLAUDE.md](CLAUDE.md) — the working agreement for agents implementing this.

## Layout

| Path | Purpose |
| --- | --- |
| `crates/core/` | `rimaia-core` — all logic. Must not depend on `tauri` |
| `src-tauri/` | Tauri shell: commands, window, state wiring. Thin |
| `src-tauri/migrations/` | SQLite migrations, append-only once shipped |
| `src/` | React 19 + TypeScript frontend |
| `docs/adr/` | Architecture decision records |
| `tasks/` | Task backlog |

## Tests

```bash
npm run typecheck
npm run test                  # vitest
cargo test -p rimaia-core     # no system dependencies needed
```

Logic-first, and no E2E. Git runs against real repositories in temporary directories; the
Claude CLI is faked by replaying recorded `stream-json` fixtures rather than by mocking a
trait. The full set of commands CI runs is in [CLAUDE.md](CLAUDE.md).
