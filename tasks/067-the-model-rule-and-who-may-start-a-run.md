---
id: "067"
title: The model rule, and who may start a run
milestone: v0.5
status: ready
depends_on: ["043"]
adrs: ["0031", "0016", "0026", "0012"]
size: M
---

# The model rule, and who may start a run

## Goal

Add the two claim rules from ADR-0031 that cannot fire while there is one runner and Claude is
the only production provider:

- **The model rule** (point 1). A runner is not given a task whose model its provider cannot
  run.
- **Run now names a runner, and only its owner may ask** (point 7). Whether the owner is at
  the machine decides the permission posture, and capacity does not apply.

Both were cut from task 043 before launch to keep 043 inside one session. Both sit on the
claim path 043 built: the model rule is a second rule in `board::lease::eligible`, and the
owner rule is one function every start door calls before it claims. **In solo nothing a user
can see changes.** Every solo door passes `AtRunner`, the solo user owns the solo runner, and
on a Claude-only build the model rule can never fire.

## Why now

Ordered after 043 and before 045. 045 adds consent to the same `eligible` function and to the
same remote posture, and it should find the model rule and `OwnerPresence` already there.
052 puts the browser's Run now on the network and calls `authorize_start` with `Remote`.
Neither rule is on 044's critical path.

## Scope

**The model rule (ADR-0031 point 1, ADR-0016).** ADR-0031 has the claim skip "a task whose
effective strategy names a model the runner's provider cannot run", and does not define the
phrase. The obvious reading ("the model is not in the catalogue resolved for this runner's
provider") is a behaviour change in solo. `tasks.model` is free text, `update_task` and the
MCP tools accept any string, and Claude runs a full model id (`claude-…`) that no catalogue
lists. Under that reading a queue that runs such a card tonight would skip it tomorrow. So:

> A model `m` cannot run on a runner whose provider is `P` when `m` is **not** an id in the
> catalogue the board resolves for `P`, **and** `m` **is** an id in the default catalogue of
> some other provider this build knows.

That skips what ADR-0031 is protecting against, a card set to one provider's model reaching
another provider's runner. It leaves a model no provider claims going to the CLI as it does
today. It needs `ProviderId::ALL` and `ProviderId::default_catalogue(self)`, which delegates to
each provider's own `default_catalogue`. Ledger's arm is `#[cfg(feature = "testing")]` like
the variant. The `AgentProvider` trait does not change. The rule is a pure function,
`strategy::catalogue::runs_on(model, provider, resolved: &Catalogue) -> bool`, which
`eligible` calls with the model the phase would spawn with:

- `implementation` and `fix`: `EffectiveStrategy::model`;
- `review`: `review_model` from 021's `RunContext::review`, when it names one, otherwise the
  effective strategy's model, as 021 resolves it;
- `strategy`: exempt, both for a `Plan` claim and for 043's inline planner claim, because a
  planner chooses from the runner's own catalogue.

Because it lives in `eligible`, it runs wherever 043 runs that function: in `selection::plan`'s
runner view (which 043 gave the `ProviderId`), inside the claim transaction, and in
`finish_run`'s `Continue` transaction, against the next phase's model. A `Continue` into a
phase the provider cannot run answers `Released`. The claiming runner's provider is the
in-process adapter's `provider` (D31 point 9), or the `ProviderId` the runner sends over HTTP
(D31 point 10, 052). A refused `claim(Run)` is `Invalid` with the sentence in the criteria.

**A known limitation, recorded and pinned.** `strategy_catalogue` is one stored document per
team, not one per provider, and `catalogue::resolve` fills it from the provider's defaults
field by field. Once a team edits `models` to list Claude's models, every provider's resolved
catalogue lists them, so `runs_on` is true for a Ledger runner and the rule stops firing. One
Claude-oriented edit turns the guard off for every other provider's runners. This is accepted
while no second production provider exists. The fix is a catalogue per provider, which needs
an ADR-0028 amendment, because ADR-0028 places `strategy_catalogue` as one team setting.
`runs_on`'s doc comment and the D31 amendment both say so, and a unit case pins today's
behaviour so that the change is deliberate when it comes.

**Run now, Retry now and Plan now name a runner (ADR-0031 point 7).**
`board::service::authorize_start(ctx, runner_id, presence: OwnerPresence) ->
Result<RunTrigger>`. Every start door calls it before it claims, and none restates its checks
(ADR-0006):

- `ctx` is the caller's context (038's actor), never the in-process adapter's `System` one.
- **The runner.** It must exist, and its `user_id` must be a member of `ctx`'s team in
  `team_memberships`. Otherwise the answer is `NotFound` (ADR-0029 point 5). A runner with
  `unpaired_at` set is `Invalid`: `this runner has been unpaired and can no longer start
  runs`.
- **Owner only.** The caller must be `runners.user_id`, otherwise `Invalid`: `only the owner of
  this runner can start a run on it; assign the task to them, or leave it ready for their
  queue`. A teammate makes a task claimable by assigning it. Starting a process on someone
  else's machine is not a board action.
- **Presence decides the posture.** `OwnerPresence::AtRunner` returns `RunTrigger::Manual`
  (ADR-0012 point 6, `acceptEdits`). `OwnerPresence::Remote` returns `RunTrigger::Queued`,
  which runs as an unattended run and is therefore subject to ADR-0012's per-repository
  opt-in, and from 045 to consent, exactly as a queued run is. Presence is decided by the
  door, never by a request field. Every door in this task is a local command or the loopback
  operator MCP server on the runner's own machine, so each passes `AtRunner`. 052's browser
  route passes `Remote`.
- **Capacity does not apply (D19 point 5).** `claim(Run)` takes no `FreeCapacity`, and the
  board applies none. The runner's `LocalSlot::acquire_unbounded` is unchanged.
- **Plan now** goes through the same function for the owner rule. The planner's own posture is
  unchanged.

The doors: the core starter `runner/start.rs` (`start_task_run`, `retry_task_now`), and
`claim_for_planning` (the Plan now command, and the operator MCP's `plan_task_strategy` and
`plan_tasks_strategy`). The name is `OwnerPresence`, not `Presence`, because 058's
`rimaia_runner::host::Presence` is the runner's own view of its machine and never crosses to
the board.

**The contract harness.** D31 point 13's `Harness` gains two members:

- `fn runner_on(&self, which: Which, provider: ProviderId) -> Arc<dyn BoardPort>`, a runner
  whose claims carry that provider. The in-process harness builds the adapter with that
  provider. 052's HTTP harness builds its `HttpBoard` with that `ProviderId`.
- `async fn add_member_with_runner(&self) -> (String, String)`, which adds a second user as a
  member of the team, with a runner of their own, and returns both ids.

**The seam contract.** A D31 amendment, "What task 067 decided": the model rule, its phases and
its known limitation; `authorize_start`, its checks and its order; `OwnerPresence`; and the two
harness members. Add the "How to use this" row for 067: D8 · D17 · D19 · D27 · D28 · D29 · D31
· D32 · D33.

**Caches.** `authorize_start` adds board queries, so the board's offline cache is regenerated
with D33 point 3's recipe.

## Out of scope

- **Consent, assignment and the repository ceiling** (045). 045 extends `eligible` and the
  remote posture.
- **Any HTTP route**, the browser's Run now, and relaying a request to a runner (052).
- **"Runner offline" refusals** (053).
- **A catalogue per provider.** Recorded above as the follow-up, with its ADR.
- **UI.** No control picks a runner, and no card explains a skip (061).

## Acceptance criteria

**The model rule**

- `strategy::catalogue::runs_on` has unit tests for five cases: a model in the runner's
  catalogue runs; another provider's default model does not; a model no provider lists runs;
  an edited catalogue that lists another provider's model runs; and
  `an_edited_catalogue_listing_claudes_models_lets_a_ledger_runner_take_them`, which pins the
  limitation above.
- `a_runner_is_not_offered_a_task_whose_model_belongs_to_another_provider`: runner B is on the
  Ledger provider, and a task is set to `opus`. B's `claim(Next)` passes it over, B's plan does
  not list it as next, and A's claim takes it. B's `claim(Run)` refuses with `this task asks
  for the model "opus", which Ledger cannot run. Change the task's model, or run it on a runner
  whose provider offers it.`, using the provider's `display_name`. Nothing is written.
- `a_continue_into_a_review_whose_model_this_provider_cannot_run_is_released`: B, on Ledger,
  finishes an implementation whose loop would continue into a review with `review_model`
  `opus`. The answer is `Released`, the lease is gone, and no review run is started.
- `a_model_no_provider_lists_still_reaches_the_cli`: a task set to `claude-opus-4-5` is claimed
  and its argv carries `--model claude-opus-4-5`, as on `main`.
- A `Plan` claim, and a fresh claim leased as `strategy` for the inline planner, are not
  subject to the rule.

**Who may start a run**

- `only_a_runners_owner_can_start_a_run_on_it`: the solo user asks to start a run on the runner
  of a second member. The answer is `Invalid` with the owner sentence above, exactly. Nothing is
  written. Plan now is refused the same way.
- `a_runner_outside_the_callers_team_is_not_found`, and `an_unpaired_runner_is_refused` with
  the unpaired sentence above, exactly.
- `run_now_is_not_bound_by_capacity`: with `FreeCapacity { total: 0, .. }`, `claim(Next)`
  returns `None` and `claim(Run)` for the same runner and task claims.
- `presence_decides_the_permission_posture`: `AtRunner` gives `RunTrigger::Manual`
  (`acceptEdits`). `Remote` gives `RunTrigger::Queued` (`bypassPermissions`), and on a
  repository without the unattended opt-in the claim is refused with the existing opt-in
  sentence before anything is written.
- `start_task_run`, `retry_task_now`, `plan_task_strategy` and `plan_tasks_strategy` (each
  command, and each operator MCP tool that exists) all reach `authorize_start` with
  `OwnerPresence::AtRunner`.

**Everything else**

- The D31 amendment exists as described, and `runs_on`'s doc names the limitation and its
  follow-up.
- No test sleeps.
- Every CI check passes, run with `SQLX_OFFLINE=true`: the full list in CLAUDE.md, including
  `cargo test -p rimaia-runner` and its clippy line.

## Notes

**Read first.** ADR-0031 points 1 and 7, ADR-0016 (the effective strategy's precedence chain),
ADR-0026 (the provider seam), ADR-0012 point 6, and ADR-0029 point 5.

Seam entries: **D31** with 043's amendment; **D27** (the provider module and how a provider is
held); D19 point 5 (manual starts and capacity); D17 (the planner's budget comes from the
runner's catalogue); D28 (`runners`, `team_memberships`); D29 (`RunKind`, for the phase); D32
point 3; D33; D8.

**Files to start from.** `crates/core/src/board/lease.rs` (`eligible`, 043),
`crates/core/src/board/service.rs`, `crates/core/src/runner/start.rs`,
`crates/core/src/runner/strategy.rs` (`claim_for_planning`), `crates/core/src/mcp/server.rs`,
`crates/core/src/strategy/catalogue.rs`, `crates/core/src/runner/provider/mod.rs`
(`ProviderId`), `crates/core/src/testing/provider.rs` (Ledger's catalogue),
`crates/core/src/testing/board_contract.rs`.

**If `eligible` is not one function, or a start door claims without going through
`runner/start.rs` or `claim_for_planning`, stop and ask.** Do not add a second check beside it.

**What the next tasks expect.**

- 045 adds consent to `eligible` and to the `Remote` posture.
- 052's route calls `authorize_start` with `OwnerPresence::Remote` and does not restate its
  checks. Its relay's read-only `eligible` check refuses a pin or a model before relaying, with
  043's and this task's sentences. It sends the `ProviderId` the model rule reads, and its HTTP
  harness implements the two new harness members.
- 058's `Presence::Absent` never reaches `authorize_start`: a headless runner has no start door
  of its own.
- 061 renders a skip and the runner picker.
