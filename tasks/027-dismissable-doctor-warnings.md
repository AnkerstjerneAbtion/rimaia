---
id: "027"
title: A doctor warning the user can put down
milestone: v0.3
status: ready
depends_on: ["018"]
adrs: ["0006", "0021"]
size: S
---

# A doctor warning the user can put down

## Goal

A `warn` row the user has read and decided about can be dismissed, and stays dismissed until
the thing it warns about actually changes. A `fail` row cannot be dismissed — it collapses.

## Why now

`DoctorBanner` renders above every view, on every screen, for as long as any check is
non-passing, and there is no way to make it go away except by fixing the environment. That
is right for a `fail`: the queue genuinely will not start, and a banner is the only place
that gets said before 2am.

It is wrong for a `warn`, and D22 already says why without drawing the conclusion. A warn is
by construction the case where *the queue can still do its job* — an unauthenticated `gh`
means runs work and the PR step is skipped, a `claude` below the pinned minimum means "this
may well still work". Those are decisions a user makes once. Having made it, they get a
permanent band across the top of the board restating it, and the predictable outcome is that
they stop reading the banner at all — including the day it turns into a `fail`. **A notice
that cannot be acknowledged trains the user to ignore the channel it arrives on**, which
costs exactly the blocking case it was built to protect.

Task 018 also already shipped the precedent for the shape of this: `ONBOARDING_DISMISSED` is
a settings key, written by `dismiss_onboarding`, for the same "the user has seen this and is
done with it" reason.

## Scope

- **A dismissal is per row, not per banner, and it is keyed on the row's content.** `check` +
  `repository` + `detail` — not `check` alone. `RepositoryPath` warns about a *named*
  repository (`CheckResult::repository`), and "I know about that one" must not silence the
  same check firing about a different one. `detail` is in the key because it is what changes
  when the underlying condition changes: dismiss "claude 2.1.200 is older than the pinned
  minimum", upgrade to a version that is still too old, and the new sentence is a new warning
  the user has not seen. A dismissal is an answer to a specific sentence, not a mute button
  on a check.
- **Storage is a settings key holding JSON**, alongside `ONBOARDING_DISMISSED` in
  `crates/core/src/db/settings.rs`, with the typed accessor there like every other key (D3).
  Deliberately not a column: seam-contract D4 closed the migration list, and a set of strings
  that only the doctor reads is exactly what the key/value table is for. No migration.
- **Filtering happens in `rimaia-core`, and the report says what it filtered.** The
  `DoctorReport` gains the dismissed rows' count or the rows themselves marked — settle it in
  the implementation, but the Settings → Environment panel must be able to show a dismissed
  warning and un-dismiss it, so the data cannot simply be dropped on the way to the client.
  A dismissal the user cannot find again is a leak, not a feature.
- **`fail` is not dismissible. It collapses instead.** The banner keeps one line naming how
  many blocking problems there are and stays expandable; the rows go away, the fact does not.
  The user's actual complaint — vertical space on every screen forever — is answered without
  hiding the sentence that explains why tonight's queue will refuse to start.
- **Parity, per ADR-0021.** Dismissing and clearing are settings mutations and get MCP tools,
  `RunAccess::Refused` — point 4's "anything that reconfigures the installation" clause
  verbatim, and with a sharper edge than most: a run-scoped agent that could silence the
  doctor could silence the report on the environment it is itself running in.
  `every_registered_tool_has_a_run_scope_decision` is what catches this being forgotten.

## Out of scope

- **Any change to what blocks the queue.** The refusal is `QueueHandle::start`/`resume`'s and
  reads `doctor::run`'s own report (D22 point 1). It does not read the dismissal set, now or
  ever — see the acceptance criteria.
- **Auto-expiry, snoozing, or "remind me in a week".** A dismissal ends when the row's
  content changes or the user clears it. A timer is a third rule with no argument behind it.
- **Dismissing `pass` rows.** They are already not in the banner.
- **Changing which checks warn and which fail.** That line is D22 point 3's and it is not
  this task's to move. A user who wants a `fail` to stop nagging wants the queue to start,
  and that is a different request.

## Acceptance criteria

- Dismissing a warn row removes it from the banner, and it is still gone after the app is
  restarted and after Re-check is pressed with the environment unchanged.
- The same check warning about a *different* repository still appears after the first was
  dismissed.
- A dismissed warn whose `detail` changes appears again, without the user clearing anything.
- Dismissing every warn on a report that also has a `fail` leaves the banner present, in its
  collapsed form, still naming the blocking count.
- Settings → Environment lists dismissed warnings and restores them.
- **A test that dismisses every row and then calls `QueueHandle::start` against a blocking
  environment, and asserts it still refuses with the same error and still writes no
  `queue_state`.** This is the one that matters: it is the assertion that dismissal is
  presentation and the refusal is the rule (ADR-0006), and it fails loudly if a later change
  ever wires the dismissal set into the gate.
- Both new tools are registered with a run-scope decision, and both are refused for
  run-scoped handles.
- D22 gains an appended dated amendment recording that dismissal exists, that it is scoped
  to `warn`, and that the refusal path does not read it. Append; do not edit the entry.

## Notes

The temptation to watch for in review is a single `doctor_banner_dismissed: bool`. It is
smaller, it is what the request literally asks for, and it is wrong in the way that is
expensive later: the next genuinely new warning — a repository that lost its remote, a disk
filling up — arrives silently into a banner the user turned off six weeks ago for an
unrelated `gh` token. Keying on content is what makes "dismiss" mean *I have read this one*
rather than *stop telling me things*.
