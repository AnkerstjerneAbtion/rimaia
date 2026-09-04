---
id: "026"
title: Open a task's worktree in the tool the user actually works in
milestone: v0.3
status: ready
depends_on: ["005", "007"]
adrs: ["0005", "0015", "0021"]
size: M
---

# Open a task's worktree in the tool the user actually works in

## Goal

A card whose task has a worktree gets an **Open in** control offering VS Code, Cursor, Zed,
a terminal and the OS file manager — and offering **only the ones that are actually
installed on this machine**, never a menu entry that opens nothing.

## Why now

The product's whole shape is "queue in the evening, review branches in the morning". The
review is the part that happens in a human's editor, and right now the only way in is the
task detail panel's "Open in Finder/Explorer" — one target, one click deep, on a panel the
user has to open first. Every actual next step after reading a card is *look at this branch
somewhere else*, and the card is where that decision is made.

The detail panel already proves the plumbing: `reveal_task_worktree` (task 007) opens a
worktree path in the file manager, and `Task.worktree_path` is already on every card's own
row, so no board read has to change (seam-contract D12 stays as it is). What is missing is
the second half — the other four targets — and the thing that makes a menu of them
honest, which is knowing which of them exist.

Also: task 016 shipped worktree cleanup and task 017 will ship the morning review flow.
Both make the worktree a thing the user is invited to look at rather than an implementation
detail, and neither gives them a way to open it in an editor.

## Scope

**A new `rimaia-core` module owning target detection and the argument vector for each
target** — `crates/core/src/openers/`. It is the whole of the logic in this task, and it is
written the way `doctor::checks` is written and for the same reason: **a function over
injected inputs, never over whatever happens to be installed on the machine running the
tests.** Detection takes injected candidate paths (the shape of `doctor::Programs`) plus an
injected probe, so the suite can assert "Zed present, Cursor absent" on a runner that has
neither.

- **Five targets:** VS Code, Cursor, Zed, terminal, file manager. The file manager is
  always present by definition and needs no probe; the other four do.
- **Detection is two probes per editor, not one.** A CLI shim on `PATH` (`code`, `cursor`,
  `zed`) is the launch mechanism, but on macOS it is *installed separately from the app* —
  VS Code ships it only after the user runs "Install 'code' command in PATH". An app that is
  installed but has no shim must still be offered (via the application bundle), and an app
  with neither must not be offered at all. Getting this backwards in either direction is the
  exact failure this task exists to prevent.
- **Detection is cached and refreshed deliberately, never per render.** Same argument D22
  makes for the doctor: five subprocess probes per card, on a board with forty cards that
  re-renders on every drag, is a different feature from the one being asked for. Probe when
  the window opens and when the user asks; a menu built from a stale probe fails at the
  point of *opening*, which is already handled below.
- **Two Tauri commands, both thin:** one returning the detected targets, one opening a given
  task's worktree in a given target.
- **The card control.** Rendered only when `task.worktreePath !== null` — the card does not
  check the disk. `getWorktreeStatus` per card is N fetches for a predicate that changes the
  wording of an error and nothing else; a path that no longer resolves produces the open
  command's own `RimaiaError`, rendered where `TaskCard`'s existing `runError` line is
  rendered. `TaskCard` also has to isolate the control from the drag surface the way "Run
  now" already does (`onPointerDown`/`onKeyDown` stoppers), and the menu has to be operable
  from the keyboard — the card claims Enter and the arrow keys for its own navigation.

## Out of scope

- **A configurable list of targets, or a "default editor" setting.** Five detected targets
  is the feature; a settings surface for editing them is a different one and nothing yet
  asks for it.
- **Opening anything but a worktree.** Not the repository, not the log file (task 015's
  `reveal_run_log` already has that), not a specific file inside the diff.
- **Any new dependency.** `tauri-plugin-opener` is already wired (Rust, npm and
  `capabilities/default.json`) and is the first thing to try. If a target genuinely cannot be
  launched through it, spawn it with an argument vector — **never `sh -c`**, since worktree
  paths contain spaces by construction and the fixtures put one there on purpose. A new npm
  or Cargo dependency needs a D6 amendment: stop and ask rather than adding one.
- Windows and Linux *verification*. The detection code is written for all three platforms and
  unit-tested against injected inputs on all three; the acceptance criteria below only claim
  what a human actually clicked, which will be macOS. Say so in the PR rather than implying
  more.

## Acceptance criteria

- On a machine with some subset of VS Code / Cursor / Zed installed, the card's menu lists
  exactly that subset, plus the terminal and the file manager. **An uninstalled editor never
  appears** — that is the requirement, not a nicety.
- An editor installed **without** its `PATH` shim is still offered, and opening it works.
- Every target opens the worktree directory of the card it was invoked from, on a repository
  path containing a space.
- A card whose task has never run shows no Open-in control at all — not a disabled one.
  "No worktree yet" is the normal state of most of the board (task 007's own instruction),
  not a failure to report.
- A worktree path that no longer resolves on disk produces the service's error on the card,
  and does not crash or silently no-op.
- Clicking, and keyboard-activating, the control never lifts the card, never opens the detail
  panel, and never starts a run.
- Detection has unit tests over injected inputs covering: shim present, bundle present with
  no shim, neither present, and all three editors absent (the menu is then terminal plus file
  manager). No test depends on what is installed on the machine running it, and none spawns
  a real editor.
- Seam-contract D20 point 6 gains an appended amendment naming these two commands as
  desktop-only, so ADR-0021 point 1 ("a Tauri command without an MCP tool is a defect")
  stays literally true. The ground is the one `reveal_task_worktree` already states in
  `src-tauri/src/commands/worktree.rs`: an MCP client is a protocol, not a desktop that could
  be shown a window — this is not a capability being withheld from agents, it is a capability
  agents have no referent for. Append; do not edit the entry.

## Notes

**The detection half is the task; the launching half is three lines.** Judge the diff on
whether `openers` can be tested without VS Code installed, and on whether a wrong probe
produces a missing entry rather than a broken one. A menu that lists Cursor and does nothing
when clicked is worse than a menu that never mentions Cursor, because the second one is
merely incomplete while the first one is lying.

**Two targets are shaped differently from the other three and it is worth knowing before
starting.** "Terminal" is not one program: macOS has Terminal.app and iTerm, Windows has
Windows Terminal and `cmd`, and Linux has a dozen with no common launcher. "File manager" is
the opposite — it always exists, and `reveal_task_worktree` already opens it. Neither is a
reason to drop them from the menu; both are a reason not to model them as a fourth and fifth
editor.
