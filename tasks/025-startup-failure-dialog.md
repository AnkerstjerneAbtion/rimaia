---
id: "025"
title: A startup failure a double-clicked bundle can see
milestone: v0.3
status: ready
depends_on: ["018"]
adrs: ["0002", "0003"]
size: S
---

# A startup failure a double-clicked bundle can see

## Goal

When startup fails, tell the person who double-clicked the app — with a dialog, before the
process exits — instead of writing to a stderr nobody is attached to.

## Why now

Seam-contract D11 fixed what a startup failure does: the window never opens, the process
exits non-zero, and the reason goes to stderr and to `<app-data>/logs/rimaia.log`. **No
modal.** It then named the case it did not solve — *"a double-clicked `.app`, where nobody
reads stderr"* — and delegated that to task 018's preflight doctor.

**Task 018 found that the delegation was misplaced, and said so rather than working around
it** (D11's amendment). The doctor is a command inside a *running* app: it runs when
startup has already succeeded. A failed migration is exactly the case where no window
opens, no command surface exists, and nothing can be asked to check anything. The two
failures are disjoint — the doctor stops a *run* failing at 2am; this is the *process*
failing at launch — so no amount of doctor coverage reaches it.

This becomes worth doing now because task 018 also shipped packaging. A `.app` bundle is
precisely the case with nobody watching stderr, and until now there was no bundle to
double-click.

D11's original cost argument is also gone: *"a modal needs `tauri-plugin-dialog`, which is
not a dependency"* stopped being true at task 003, which added the plugin for the folder
picker. Rust, npm and `capabilities/default.json` all carry it today.

## Scope

- A blocking, native error dialog on the fatal startup path, shown **before** the process
  exits: the failing step, the reason, and the path of the log file that has the detail.
- The one place that decides this is `src-tauri/src/lib.rs`'s setup hook. Every fallible
  step there already logs at `error` level before propagating (D11: Tauri turns the
  returned `Err` into a panic at `RuntimeRunEvent::Ready`, and `panic!` does not go through
  `tracing`), so the dialog is one more thing that happens on a path that already exists —
  not a second error-handling story.
- The exit stays non-zero and the log line stays. The dialog is additive; nothing about
  D11's stderr-and-log contract is replaced.

## Out of scope

- Any change to the doctor. Task 018 closed the environment half and this is the other one;
  merging them would put a launch-path concern inside a command that cannot run on the
  launch path.
- Recovery, repair or a "try again" button. A migration that failed is not a thing a dialog
  should offer to retry — the log and the file are what a human works from.
- Warnings. Only the fatal path gets a dialog; a doctor warning already has a banner.

## Acceptance criteria

- A deliberately broken migration in a **bundled** build produces a visible dialog naming
  the step, the reason and the log path, and the process still exits non-zero.
- The dialog does not deadlock. `blocking_show()` on the setup hook's own thread is the
  mechanism, and whether that is safe there is the open question this task exists to
  settle — **verified against a real `npm run tauri build` bundle on macOS, not against
  `tauri dev`**, because the two do not schedule that thread the same way.
- A successful startup shows no dialog and is byte-identical in behaviour to today.
- D11 gains a further dated amendment recording what was verified and how.

## Notes

**This task exists because task 018 refused to guess.** It could have added the dialog
unverified; an unverified blocking call on the launch path is a worse failure than the
silence it replaces, because it turns "the app did not open and I do not know why" into
"the app hangs forever". It recorded the mechanism, the risk and the reason it stopped, and
left the case explicitly open in D11.

So the deliverable here is small and the *verification* is the whole job. **This needs a
human at a real bundle** — see `tasks/README.md`'s "Landed is not the same as proven"; the
PR carries the checklist.
