---
id: "071"
title: Close the hosted MCP parity gaps
milestone: v0.5
status: ready
depends_on: ["060"]
adrs: ["0021", "0034", "0035", "0029", "0006"]
size: L
---

# Close the hosted MCP parity gaps

## Goal

Give every board command that still has no MCP tool its tool, so that ADR-0034 point 1's
rule holds as written: every board command is reachable over HTTP **and over MCP**. 060
made the rule a test and pinned each gap as a row of `crates/core/src/mcp/parity.rs` reading
`ADR-0021 point 1 defect, closed by task 071.` This task turns each of those rows into a
tool and deletes the row. When it lands, `parity.rs` has no defect row left, and every
`NoTool` entry is a deliberate choice that cites its ground.

The rows, as 060 will have left them (read `parity.rs`, not this list):

| Command | Kind of gap | From |
| --- | --- | --- |
| `update_repository`, `remove_repository` | Owner-only repository writes | D32 point 9 |
| `set_task_run_state` | Run-state transition | D32 point 9 |
| `update_task_link`, `reorder_task_link` | Task links (the other two have tools) | D32 point 9 |
| `get_blocking_reason` | Task read | D32 point 9 |
| `set_base_instructions` | Owner-only team setting (`get_` has a tool) | D32 point 9 |
| `get_run_cost_summary` | Aggregate read | D32 point 9 |
| `start_task_run`, `cancel_task_run` | Run controls | D32 point 9, flipped by 052 |
| `get_run_tail` | Live read | D32 point 9, flipped by 048 |
| `list_runs_for_task`, `list_runs`, `get_run` | Run records, the review bundle | D32 point 9 |
| `read_run_transcript_page`, `search_run_transcript`, `summarize_run_transcript` | Transcripts | D32 point 9, flipped by 056 |
| `list_runners`, `unpair_runner` | The caller's own runners | 050 |
| `list_runner_doctor_reports` | The caller's runners' doctor results | 054 |

Four local commands carry the same defect on the local router, and close here too because
their board halves do: 056's `read_local_run_transcript_page`, `search_local_run_transcript`
and `summarize_local_run_transcript` (056 recorded them as inheriting the board reads' gap,
"which 060's parity work closes together"), and 066's `get_run_log_path`.

## Why now

- **The rule is a test, and the gaps are named.** 060's `parity.rs` lists each row with
  this task's number. `every_board_command_is_in_the_parity_table_exactly_once` keeps the
  list from growing behind anyone's back, so it can only shrink, and this task shrinks it to
  zero.
- **Every function already exists.** Each command is a registry row over a `rimaia-core`
  function (046). A tool is a second adapter over the same function (ADR-0006), with an
  argument type, a view, a run-scope decision and a team decision. Nothing in the services
  changes.
- **The hosted endpoint makes the gaps real.** Before 060, an agent missing `get_run` could
  still ask the person at the desktop. A planning session in a cloud sandbox with only the
  hosted `/mcp` cannot see a run's outcome, its review bundle or its transcript, cannot
  start the run it just planned on its user's own runner, and cannot tell why a task is
  blocked. Those are the questions ADR-0035's remote planner asks first.
- **Every safety net is already keyed to `tools/list`.** 060's
  `every_hosted_tool_has_a_cross_team_case`, `every_tool_has_a_team_decision`,
  `every_team_tool_is_refused_for_want_of_a_team_and_no_other_is` and
  `every_owner_only_tool_on_the_hosted_endpoint_has_a_member_case`, and 020's
  `every_registered_tool_has_a_run_scope_decision`, each fail until a new tool has its case.
  This task cannot add a tool that skips isolation without a red test.

## Scope

Read ADR-0021, ADR-0034 point 1, ADR-0035 points 2 and 6, D16 (snake_case, `list_tasks`
without plan text), D30 (the run-scoped handle), D32 points 7 to 9 with the appendix, and
060's seam entry (`select`, `takes_team`, the parity tables, the hosted member table) before
starting.

**1. One tool per defect row.** For each row, in `crates/core/src/mcp/server.rs`'s board
router (041's split), or the local router for the four local commands:

- **The tool has the command's name**, snake_case arguments (D16.1), and a request type in
  `crates/core/src/mcp/requests.rs` that mirrors the command's input. Optional fields stay
  optional; nothing is added that the command does not take, except `team` where point 3
  says so.
- **The handler calls the function the registry row's handler calls**, with the context
  `select` narrowed (060 point 2) where the tool takes a team. If the command's handler in
  `crates/core/src/api/board/` does more than call one function (a check, a lookup, a
  shape conversion that carries a rule), move that into the service first, in its own
  commit, so the two adapters stay thin (ADR-0006). A rule enforced only in the command's
  handler is a bug this task fixes, not one it copies.
- **The answer is a view in `crates/core/src/mcp/responses.rs`**, built from the same
  service result the command's DTO is built from, in snake_case. List views omit large
  text, as D16.6 omits plan text from `list_tasks`: `list_runs_for_task` and `list_runs`
  carry no prompt, transcript or patch; `get_run` carries the review bundle as the command
  does, with 033's capped patch. Transcript tools return the service's page, matches or
  summary and nothing more.
- **A `Tool` variant**, in `Tool::ALL`, with its `run_access` row (point 2) and its
  `takes_team` arm (point 3). Both matches are exhaustive with no wildcard, so the variant
  does not compile without them.
- **The `parity.rs` row becomes `BoardParity::Tool(Tool::…)`.** The local commands have no
  board row; their tools are checked by 060's `the_loopback_endpoint_serves_every_local_tool`
  and `no_local_tool_is_served_on_the_hosted_endpoint`.
- **Descriptions say what the tool does in the operator's terms**, as the existing tools'
  do, and say where the answer differs by endpoint: `get_run_tail` returns the tail the
  server relays (048), which is empty for a run whose runner is offline; the transcript reads
  return the server's copy (ADR-0036 point 1), and a summaries-only runner's transcript is
  readable only through the local tools on that runner's own desktop (ADR-0036 point 5).

**2. Run scope: every new tool is `Refused` for every grant, except the two link tools**
(035's per-grant table, D30).

- `start_task_run`, `cancel_task_run` and `set_task_run_state` control runs, and ADR-0021
  point 4 keeps "any future queue or run control" off the run-scoped surface permanently.
- `update_repository`, `remove_repository` and `set_base_instructions` reconfigure what
  governs runs, which is point 4's second ground.
- `list_runners`, `unpair_runner` and `list_runner_doctor_reports` are about the person; a
  run has no business enumerating or retiring its owner's machines.
- The reads (`get_run`, `list_runs*`, `get_run_tail`, `get_blocking_reason`,
  `get_run_cost_summary`, the transcript reads, `get_run_log_path`) are refused because the
  run-scoped handle grows only by decision (D30), and none is argued here. A run already has
  its own card through `get_task`. Widening any of them for a review run is 021's or a later
  task's decision, with a D30 amendment, not a default this task picks.
- The task links (`update_task_link`, `reorder_task_link`) follow their siblings.
  `add_task_link` and `remove_task_link` are `OwnTaskOnly` on `main` (`scope.rs`, "a run may
  read and amend the card it was started for"), so these two are too, for every grant that
  has the siblings, and `run_tools::call` (055) gains their arms with the same `authorize`
  call. Editing a link's label or order on its own card is no wider than adding or removing
  one. If 035's per-grant table has since narrowed the siblings for some grant, follow it
  there as well, so the four always agree.

**3. Team decisions**, by 060's rule (`takes_team` is `true` when the tool can act without
naming an entity). Check each against its handler; the expected answers are:

- `true`, `Refuse` when omitted under several teams: `list_runs` (it lists, as `list_tasks`
  does) and `set_base_instructions` (as `get_base_instructions`).
- `true`, `Span`: `get_run_cost_summary`, because it aggregates over the caller's teams'
  runs (D32's appendix note), exactly as `get_analytics` does. Add it to 060's
  `get_analytics` exception in `every_team_tool_is_refused_for_want_of_a_team_and_no_other_is`
  by name.
- `false`: every tool that takes a task, run or repository id, and `list_runners`,
  `unpair_runner` and `list_runner_doctor_reports`, which are about the caller (050 and 054
  scope them to `runners.user_id = caller.user_id`).

**4. Rules the tools inherit, and must be shown to inherit.**

- **`start_task_run` over the hosted endpoint makes work claimable and spawns nothing on
  the server**, which is ADR-0035 point 6's line between a request and a process. It is the
  function 052's command calls: a claim for one named runner that only that runner's owner
  may ask for (ADR-0031 point 7), refused with 043's and 067's sentences otherwise. On the
  embedded and loopback endpoints it starts the run on this machine's runner, as Run now
  does.
- **`cancel_task_run`** reaches the holding runner through its lease, with 052's rule on who
  may ask (a team owner may stop a member's run).
- **`unpair_runner`** checks that the runner is the caller's and calls 047's
  `identity::tokens::unpair_runner`, with 057's pin release. It narrows access, so it is in
  the class 060's Risks already accept for a leaked owner token (it can remove, never add),
  and it is undone by pairing again at the machine.
- **`remove_repository`** keeps `repo::remove`'s refusal while any task references the
  repository, so the tool cannot delete a plan and is not `delete_task` by another name
  (ADR-0021 point 5). If 054 changed that function so a removal can delete tasks, stop and
  ask before giving it a tool.
- **Owner-only:** `update_repository`, `remove_repository` and `set_base_instructions` refuse
  a member with 051's sentences. The check is in the service (051); this task adds their rows
  to 060's hosted member table.

**5. What stays without a tool, unchanged.** The deliberate rows of `parity.rs`
(`delete_task`, `retry_task_now`, `delete_team`, `delete_account`, 047's credential rows, the
three membership tools on the hosted endpoint) and the deliberate local gaps (049's folder
commands, 056's `set_transcript_upload`, 063's updater commands, D25's credentials). This
task adds no tool to any of them and relabels none.

**6. Records.**

- A seam-contract entry with the next free `D` number, *Task 071's cross-cutting choices*,
  in the four-part shape: the tools added, every run-scope and team decision with its
  ground, the inherited rules of point 4, and that `parity.rs` holds no defect row. It binds
  064 and every later task that adds a board command.
- A D32 amendment closing point 9: no board command lacks a tool except the deliberate rows,
  and the parity test is the record.
- `docs/adr/0021-mcp-first-capability-parity.md` and ADR-0034 get no edit.

## Out of scope

- **New capabilities.** Every tool here has a command already. A capability with neither is
  a different task.
- **Widening the run-scoped handle** for any new tool (point 2).
- **Any change to the services' rules.** If a tool cannot be written without one, stop and
  say so.
- **Any migration or dependency** (D4, D6, D34).
- **The relay.** 059's relay forwards every non-local tool to `/mcp`, so the board tools
  added here reach a connected desktop's agent without a change to it.

## Acceptance criteria

**Parity** (`crates/core/tests/mcp_parity.rs`):

- `no_parity_row_is_a_defect`: no `NoTool` reason contains `ADR-0021 point 1 defect`. 060's
  `every_parity_defect_names_its_closing_task` is deleted with the last defect row, in the
  same commit.
- 060's parity tests pass unedited otherwise:
  `every_board_command_reaches_the_hosted_endpoint_or_says_why`,
  `every_hosted_tool_is_a_board_capability`, `no_local_tool_is_served_on_the_hosted_endpoint`
  (now including the four local tools), `the_loopback_endpoint_serves_every_local_tool` and
  `no_endpoint_has_two_tools_with_one_name`.

**Each tool** (`crates/core/tests/mcp_tools.rs`):

- `each_new_tool_answers_what_its_command_answers`: over one fixture board, a case table
  with one row per new tool calls the tool and the command (through `api::dispatch` with the
  solo `Caller`) with equivalent arguments, and asserts the same ids, states and counts. For
  writes, the resulting rows and the published change events are equal.
- `list_runs_views_carry_no_prompt_transcript_or_patch`, and
  `get_run_carries_the_review_bundle`.
- `remove_repository_over_mcp_keeps_the_task_reference_refusal`: the exact sentence, and the
  repository row is still there.
- `start_task_run_over_hosted_mcp_claims_for_the_named_runner_and_spawns_nothing`: a lease
  row exists for that runner, no child process starts in the server, and another user's
  runner is refused with 043's or 067's sentence, byte for byte.
- `cancel_task_run_over_hosted_mcp_reaches_the_holding_runner`: the cancel is listed on that
  runner's next heartbeat (052's arrangement).
- `list_runners_over_mcp_lists_only_the_callers_runners`, under a token held by one of two
  users in a shared team; `unpair_runner_over_mcp_refuses_another_users_runner_as_not_found`.
- `the_local_transcript_tools_are_served_only_on_loopback`: listed on the embedded server
  with `LocalTools`, absent from the hosted `tools/list`.

**Doors and isolation:**

- `every_registered_tool_has_a_run_scope_decision` passes, and
  `the_new_tools_are_refused_to_every_grant` calls each through a run-scoped handle for each
  grant and gets the refusal, except the two link tools, which get 055's own-task cases:
  allowed on the run's own task, refused on another with the existing sentence, in process
  and through `HttpBoard`'s `run_tool`.
- 060's `every_hosted_tool_has_a_cross_team_case` and
  `a_hosted_tool_cannot_see_another_teams_ids` pass with a case for every new board tool.
- 060's `every_tool_has_a_team_decision` and
  `every_team_tool_is_refused_for_want_of_a_team_and_no_other_is` pass, with
  `get_run_cost_summary` beside `get_analytics` as the spanning exception.
- 060's `a_member_cannot_do_what_only_an_owner_can_over_hosted_mcp` has rows for
  `update_repository`, `remove_repository` and `set_base_instructions`, each with its exact
  sentence, and `every_owner_only_tool_on_the_hosted_endpoint_has_a_member_case` passes.
- `get_run_through_a_connected_loopback_reaches_the_server`, in
  `crates/runner/tests/mcp_relay_http.rs`: one new board tool through 059's relay, unchanged.

**Everything else:**

- The seam entry and the D32 amendment exist as point 6 describes.
- No migration and no dependency was added. The `.sqlx` caches are regenerated with D33's
  recipe only if a query changed (point 1's moves into services may add one).
- Every CI check passes, as 060's file lists them, and `./scripts/check-command-wiring.sh`
  passes with no command added or reclassified.

## Notes

**Seam entries to read:** this task's row in "How to use this". D16, D30, D32 (points 7 to
9, the appendix, its amendments), 060's entry, and the entries of 048, 050, 052, 054 and 056
for the commands flipped or added there.

**Files to start from**, where 060 and its predecessors left them:
`crates/core/src/mcp/{parity,scope,server,requests,responses,team}.rs`,
`crates/core/src/mcp/run_tools.rs` (055), `crates/core/src/api/board/` and
`api/registry.rs` (046), `crates/core/tests/{mcp_tools,mcp_scope,mcp_parity}.rs`,
`crates/server/tests/mcp.rs` (060), and `crates/runner/tests/mcp_relay_http.rs` (060).

**What the previous tasks provide.** 060: the hosted endpoint, `select`, `takes_team`, the
parity tables with these rows, the cross-team suite keyed to `tools/list`, and the hosted
member table. 052: the run controls as board rows and who may ask. 048: `get_run_tail` as a
board row. 056: the transcript reads, board and local. 050 and 054: the runner rows. 066:
`get_run_log_path`. If any of those rows is still `local` when this task starts (052 and 056
each had a cut that keeps rows local), it is not a board defect: give its tool to the local
router if it has a local form, leave its `parity.rs` row out, and say so in the seam entry.

**What the next tasks expect.** 064's docs pass describes one MCP surface that matches the
command list, less the deliberate rows, with no gap list to explain.

**Size.** L. Roughly 23 tools at 40 to 60 lines each for the request type, handler, view
and the two decision arms (~1,200), service moves where a command handler holds a rule
(~150), tests (~1,100), seam entry and amendment (~100): about 2,550 lines. If it runs over,
land the reads first (`get_run`, `list_runs*`, `get_blocking_reason`, `get_run_tail`,
`get_run_cost_summary`, the transcript reads, the runner reads), then move the remaining
write rows to a follow-up task with the next free number, placed directly after this one,
and change their `parity.rs` reasons to name it in the same commit. Never land a tool
without its cross-team case, its run-scope refusal, or, for an owner-only write, its member
case.
