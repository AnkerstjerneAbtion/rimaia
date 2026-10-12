---
id: "068"
title: Transcript retention on the server
milestone: v0.5
status: ready
depends_on: ["056"]
adrs: ["0036", "0022", "0029", "0021"]
size: M
---

# Transcript retention on the server

## Goal

Make ADR-0036 point 6 true: the server keeps a team's transcripts for a period the team
chooses, 90 days by default, then deletes the transcript file and the review bundle's patch
and keeps the row (ADR-0022 point 2). Solo mode never prunes by retention.

## Why now

056 put transcripts on the server and wrote the columns this task needs
(`transcript_pruned_at`, `review_bundles.patch_pruned_at` from 033), and its reader already
renders `pruned`. Without a sweep, a server's disk grows for as long as it runs. 062's
storage metrics and its backup runbook assume transcripts are bounded. This was split out of
056 so that each fits one session.

## Scope

### 1. The team setting

- **`transcript_retention`**, in `team_settings` through a typed accessor (D3; team
  placement by exclusion, D28 part 4). Its value is a decimal number of days in
  `7..=3650`, or `until_task_deleted`. An absent key means `90`. The ceiling is arbitrary
  and stated: beyond ten years is "until deleted" in practice, and a bound keeps the date
  arithmetic from overflowing. Anything else is `invalid`:
  `transcript retention is a number of days from 7 to 3650, or until_task_deleted`.
- **`get_transcript_retention` (`Read`) and `set_transcript_retention` (`Write`)** are
  board rows. Setting is owner-only, through 039's team-placement accessor and 051's role
  refusal, so a member gets exactly `only an owner of this team can change the team's
  settings` (ADR-0029 point 3). Each is an MCP tool with `RunAccess::Refused`, because
  ADR-0021 point 1 makes a command without a tool a defect, and a run has no business
  changing retention.

### 2. The sweep

`transcripts::retention::sweep(ctx, hold, now)` runs only with `TranscriptHold::Server`. The
server binary starts it after 053's `prepare` returns, then every hour through
`Clock::sleep_until`. It never runs in the solo shell: there the "server's copy" is the
runner's own file, and ADR-0013 keeps that until the task is deleted or the user prunes.

For each team, under a context scoped to that team (039), it takes runs with
`ended_at < now − retention`, `transcript_pruned_at IS NULL` and `transcript_key IS NOT
NULL`, of every kind (D29 point 6), 500 at a time. For each batch it:

1. calls `store.delete` for each key;
2. then, in one transaction, sets `transcript_pruned_at = now` and `transcript_bytes = 0`
   on the runs, and `patch = NULL, patch_pruned_at = now` on their `review_bundles` rows.
   It deletes no row, and changes no other bundle column;
3. publishes the run ids under the team.

The file goes first. A failure between the two steps leaves a row that the next sweep prunes
again, and deleting a missing key is not an error. The other order would leave a file that
no row can find. `ended_at` rather than `started_at`, so a long run is kept for the whole
period after it ended.

A run kept on its runner is swept like any other: its patch goes, and
`transcript_pruned_at` records that the server's period has passed. 056's `TranscriptState`
still reports it as `keptOnRunner`, because the runner's copy is untouched. Do not change
that order.

### 3. The retention control

`src/views/settings/StorageSection.tsx` gains a retention control, shown only when
`getClientCapabilities().mode` is `connected` or `browser` (049). It offers 7, 30, 90, 180
and 365 days and "Until the task is deleted". A stored value that is not a preset (MCP can
set `45`) is shown as its own selected option, `45 days`, rather than snapped to a preset.
The caller's role comes from 050's `list_teams` entry for the selected team: for a member
the control is disabled with the reason `Only a team owner can change this.` The solo
desktop does not show it. Both commands get `board<T>` wrappers in `src/lib/commands.ts`
and rows in 028's fixture transport.

## Out of scope

- **Deleting server files when a task, team or account is deleted.** 056 does that.
- **Storage per team and its metrics.** 062.
- **Pruning the runner's local files.** ADR-0013 and `prune_run_logs`: a runner's files are
  its owner's (ADR-0029 point 6).
- **The MCP case for a member.** The local MCP serves the solo owner only, so a member
  caller on MCP first exists with 060's hosted `/mcp`. 060 adds that case.

## Acceptance criteria

- The accessor's cases: absent is 90, and `7`, `3650` and `until_task_deleted` are
  accepted. `6`, `3651`, `-1`, `90d` and `""` are refused with the exact sentence.
- `set_transcript_retention` is a row in 051's `a_member_cannot_do_what_only_an_owner_can`,
  dispatched through `api::dispatch` with `FixedCaller` as a member, answering exactly
  `only an owner of this team can change the team's settings`. The same call as an owner
  succeeds.
- `crates/server/tests/commands.rs` has a cross-team case for both rows, and
  `every_board_command_has_a_case` and `a_team_cannot_see_another_teams_ids` pass.
- `every_registered_tool_has_a_run_scope_decision` passes with both tools `Refused`, and
  039's two-team tool refusal covers both.
- `retention_deletes_the_transcript_and_the_patch_and_keeps_the_row_and_the_bundle_summary`:
  every capture column (D18) and `files_changed`, `insertions`, `deletions`, `files` and
  `commits` are unchanged.
- `retention_takes_every_run_kind`.
- `retention_never_touches_a_run_that_has_not_ended`.
- `until_task_deleted_never_prunes`.
- `retention_is_counted_from_when_a_run_ended`.
- `a_kept_run_past_retention_loses_its_patch_and_still_reads_as_kept_on_runner`.
- `solo_never_prunes_by_retention`: the solo shell starts no sweep, and a solo board's
  90-day-old run keeps its file.
- `the_sweep_runs_hourly_on_the_injected_clock`, with no `sleep`.
- `a_sweep_interrupted_after_the_delete_prunes_the_row_next_time`.
- Vitest: the control renders for a server client, is absent for the solo desktop, is
  disabled with its reason for a member, and shows a stored `45` as `45 days`. 028's
  fixture transport has rows for both commands, and `npm run screenshot` covers the
  control.
- Every command in CLAUDE.md passes, with `SQLX_OFFLINE=true` exported, and both `.sqlx/`
  caches are regenerated if a query changed.

## Notes

**Read first.** ADR-0036 point 6, ADR-0022 point 2, ADR-0029 points 3 and 6, ADR-0021
point 1. Seam entries: D3 and D28 part 4 (placement), D18 (capture columns survive), D29
point 6 (every kind), and 056's D31 and D32 amendments. Add a "How to use this" row for 068:
D3 · D18 · D28 · D29 · D32.

**Migration.** None. Every column exists by 056 and 033. A column found missing is a
stop-and-ask, never an edit to an earlier file.

**What the chain provides.** 033: `review_bundles.patch_pruned_at`, of which this task is
the first writer. 039: per-team contexts and the team-placement accessor. 051: the role
refusal and its test table. 053: `prepare`, after which background work may start. 056:
`TranscriptHold`, the store, `TranscriptState`, and the `pruned` notice.

**What the next tasks expect.** 060 adds the member-over-MCP refusal for
`set_transcript_retention`. 062 starts the server binary as it is, so the sweep runs in the
container with no extra step, and reports storage from `transcript_bytes`, which this task
zeroes on prune.
