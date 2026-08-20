---
id: "013"
title: Run scheduling and windows
milestone: v0.2
status: ready
depends_on: ["009"]
adrs: ["0010"]
size: M
---

# Run scheduling and windows

## Goal

Start the queue at a chosen time, or on a recurring schedule, with an optional stop time —
so the user sets it up before leaving and the work happens without them.

## Why now

"Start a queue when I leave the office" is in the product's name. Manual start works, but
this is the intended trigger.

## Scope

- Schedule configuration (`schedules` table, populated in 002):
  - `Run now`
  - `Start at` — a one-shot wall-clock time, typically today at 18:30
  - `Recurring` — cron expression with an explicit IANA timezone
  - optional stop time
  - mode and `max_concurrency` per schedule
- Timer implementation in the scheduler. A wall-clock time in the past fires immediately
  rather than being skipped; the machine having been asleep is the common case.
- Stop time stops *starting* new tasks. In-flight runs finish rather than being killed
  mid-edit.
- Settings → Schedules: list, add, edit, enable/disable, and **the next fire time shown
  for each**, so a wrong cron expression is caught in the evening rather than discovered
  in the morning.
- A pre-flight summary before a scheduled queue starts: which tasks will run, in what
  order, and which are blocked and why.
- Optional OS notification when a scheduled queue starts and when it finishes.

## Out of scope

- Running with the app closed (ADR-0010 covers why; revisit only if needed).
- Per-task schedules.

## Acceptance criteria

- A schedule set for two minutes ahead starts the queue at that time.
- A recurring nightly schedule shows the correct next fire time, including across a DST
  boundary in the configured timezone.
- A stop time prevents new starts and lets the in-flight run finish.
- Schedules survive restart, and one whose time passed while the app was closed fires on
  next launch rather than being silently skipped.
- Disabling a schedule stops it firing without deleting its configuration.

## Notes

Use a maintained cron library with timezone support; hand-rolled cron parsing is a
reliable source of "why didn't it run last night".
