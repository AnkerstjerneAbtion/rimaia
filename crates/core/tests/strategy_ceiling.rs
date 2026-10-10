//! The runner's strategy ceiling in the claim (ADR-0032 point 3's last
//! paragraph, task 045).
//!
//! `consent::ceiling::judge` is unit-tested in its own file; these ask the
//! claim the runner sends, so what is asserted is what `lease::eligible`
//! does with the ceiling a claim carries: a named start refused in Scope 11's
//! sentence, `Next` passing the task over with no reason, and each purpose
//! judged on what that phase would spawn with. Judging again at spawn, and
//! filling an absent choice, are in `tests/ceiling_at_spawn.rs` (task 072).
//! How the runner reads the stored key is here too; that the loop and the
//! starter send what it reads is asserted in `crates/runner/tests/queue.rs`.
//!
//! Bob's runner, "Mac mini", on the shared team (`testing::shared`), running
//! Bob's own tasks, so neither eligibility nor consent is what refuses.

use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::board::{
    ClaimTarget, FinishRun, FreeCapacity, LeasePurpose, NextStep, StartRun, TranscriptEnd,
};
use rimaia_core::consent::ceiling::{strategy_ceiling, StrategyCeiling, STRATEGY_CEILING};
use rimaia_core::db::{BoardColumn, ExitClass, RunKind, RunState, RunStatus};
use rimaia_core::events::TaskId;
use rimaia_core::review_loop::config as review_config;
use rimaia_core::runner::events::TokenUsage;
use rimaia_core::runner::outcome::{RunOutcome, SpawnedAs};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderId};
use rimaia_core::runner::RunTrigger;
use rimaia_core::scheduler::selection::{self, RunnerView};
use rimaia_core::tasks::{self, Patch, TaskPatch};
use rimaia_core::testing::shared::{Member, SharedTeam};
use rimaia_core::testing::TestContext;
use rimaia_core::ErrorCode;
use serde_json::json;

#[tokio::test]
async fn a_named_claim_above_the_runners_ceiling_is_refused_in_a_sentence() {
    let team = SharedTeam::new().await;
    let opus = team.task(&team.bob, "Asks for opus", Some(&team.bob)).await;
    choose(&team.bob, &opus, Some("opus"), None).await;
    let high = team.task(&team.bob, "Asks for high", Some(&team.bob)).await;
    choose(&team.bob, &high, None, Some("high")).await;

    assert_eq!(
        refusal(&team, run_now(&opus, models(&["sonnet", "haiku"]))).await,
        "this task asks for the model \"opus\", which Mac mini's strategy ceiling does not \
         allow. Change the task's model, or run it on another runner."
    );
    assert_eq!(
        refusal(&team, run_now(&high, max_effort("medium"))).await,
        "this task asks for the effort \"high\", above Mac mini's ceiling of \"medium\". \
         Nothing is lowered for it: change the task's effort, or raise the ceiling."
    );
    for task in [&opus, &high] {
        assert_eq!(
            run_state(&team, task).await,
            RunState::Idle,
            "a refused claim moves nothing"
        );
    }

    // Within the ceiling, the same claims are granted.
    claim(&team, run_now(&opus, models(&["opus"]))).await;
    claim(&team, run_now(&high, max_effort("high"))).await;
}

#[tokio::test]
async fn plan_now_is_judged_on_the_planners_budget_not_the_cards_model() {
    let team = SharedTeam::new().await;
    // Claude's catalogue gives the planner haiku at low effort.
    let task = team.task(&team.bob, "Planned", Some(&team.bob)).await;
    choose(&team.bob, &task, Some("opus"), Some("max")).await;

    assert_eq!(
        refusal(&team, plan_now(&task, models(&["opus"]))).await,
        "this task asks for the model \"haiku\", which Mac mini's strategy ceiling does not \
         allow. Change the task's model, or run it on another runner."
    );

    let planned = claim(
        &team,
        plan_now(
            &task,
            StrategyCeiling {
                models: Some(vec!["haiku".to_string()]),
                max_effort: Some("low".to_string()),
            },
        ),
    )
    .await;
    assert_eq!(planned.purpose, LeasePurpose::Strategy);
}

#[tokio::test]
async fn next_passes_over_a_task_above_the_ceiling_without_a_skip_reason() {
    let team = SharedTeam::new().await;
    let above = team.task(&team.bob, "Asks for opus", Some(&team.bob)).await;
    choose(&team.bob, &above, Some("opus"), None).await;
    let below = team.task(&team.bob, "Names nothing", Some(&team.bob)).await;
    let ceiling = models(&["sonnet"]);

    // The plan a card shows leaves it out rather than giving it a reason:
    // another runner can take it with nobody acting.
    let view = RunnerView::new(
        team.bob.runner_id.clone(),
        ProviderId::ClaudeCode,
        [team.repository.id.clone()],
    )
    .with_ceiling(ceiling.clone());
    let plan = selection::plan(&team.bob.ctx, &view)
        .await
        .expect("draw the plan");
    assert_eq!(
        plan.iter()
            .map(|entry| (entry.task_id.clone(), entry.skip, entry.queue_position))
            .collect::<Vec<_>>(),
        vec![(below.clone(), None, Some(1))]
    );

    let taken = claim(&team, next(&team, ceiling)).await;
    assert_eq!(taken.lease.task_id, below, "the task below it");
    assert_eq!(run_state(&team, &above).await, RunState::Idle);

    // With no ceiling the same runner takes it.
    let taken = claim(&team, next(&team, StrategyCeiling::default())).await;
    assert_eq!(taken.lease.task_id, above);
}

#[tokio::test]
async fn a_continue_into_a_review_is_judged_on_the_review_model_then_the_tasks() {
    // (review_model, the task's model, what the finish answers under a
    // ceiling of sonnet alone)
    let cases = [
        (
            Some("opus"),
            None,
            NextStep::Released { resume_after: None },
        ),
        (
            None,
            Some("opus"),
            NextStep::Released { resume_after: None },
        ),
        (
            Some("sonnet"),
            Some("opus"),
            NextStep::Continue {
                kind: RunKind::Review,
            },
        ),
    ];
    for (review_model, task_model, expected) in cases {
        let team = SharedTeam::new().await;
        let mut config = json!({ "enabled": "on_cost_acknowledged" });
        if let Some(model) = review_model {
            config["review_model"] = json!(model);
        }
        review_config::set_review_settings(&team.alice.ctx, &ClaudeProvider, "", config)
            .await
            .expect("the owner turns the loop on");
        let task = team.task(&team.bob, "Reviewed", Some(&team.bob)).await;
        choose(&team.bob, &task, task_model, None).await;

        // The implementation is claimed with no ceiling; the finish carries
        // the one the next phase is judged against.
        let board = team.board(&team.bob);
        let claimed = claim(&team, run_now(&task, StrategyCeiling::default())).await;
        let run_id = format!("run-{task}");
        board
            .start_run(&claimed.lease, implementation(&run_id))
            .await
            .expect("open the implementation");
        let receipt = board
            .finish_run(&claimed.lease, &run_id, succeeded(models(&["sonnet"])))
            .await
            .expect("finish the implementation");

        let case = format!("review_model {review_model:?}, task model {task_model:?}");
        assert_eq!(receipt.next, expected, "{case}");
        if matches!(expected, NextStep::Released { .. }) {
            let detail = tasks::get_task(&team.bob.ctx, &task)
                .await
                .expect("read the task");
            assert_eq!(detail.task.column, BoardColumn::InReview, "{case}");
            assert_eq!(detail.task.run_state, RunState::Idle, "{case}");
        }
    }
}

#[tokio::test]
async fn a_stored_ceiling_that_does_not_parse_reads_as_no_ceiling() {
    let harness = TestContext::new().await;
    let machine = harness.machine();
    let read = || async { strategy_ceiling(machine).await.expect("read the ceiling") };

    assert_eq!(read().await, StrategyCeiling::default(), "absent is none");

    machine
        .store
        .set_setting(
            STRATEGY_CEILING,
            r#"{"models":["sonnet"],"maxEffort":"medium"}"#,
        )
        .await
        .expect("store a ceiling");
    assert_eq!(
        read().await,
        StrategyCeiling {
            models: Some(vec!["sonnet".to_string()]),
            max_effort: Some("medium".to_string()),
        }
    );

    // A hand-edited `runner.db`, or one a later build wrote: tolerated, and
    // read as no ceiling rather than stopping every claim (ADR-0003).
    machine
        .store
        .set_setting(STRATEGY_CEILING, "sonnet, please")
        .await
        .expect("store a value that is not JSON");
    assert_eq!(read().await, StrategyCeiling::default());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn models(ids: &[&str]) -> StrategyCeiling {
    StrategyCeiling {
        models: Some(ids.iter().map(|id| (*id).to_string()).collect()),
        max_effort: None,
    }
}

fn max_effort(effort: &str) -> StrategyCeiling {
    StrategyCeiling {
        models: None,
        max_effort: Some(effort.to_string()),
    }
}

/// Sets the task's model and effort through the board's own service.
async fn choose(by: &Member, task: &str, model: Option<&str>, effort: Option<&str>) {
    let set = |value: Option<&str>| value.map_or(Patch::Unset, |v| Patch::Set(v.to_string()));
    tasks::update_task(
        &by.ctx,
        task,
        TaskPatch {
            model: set(model),
            effort: set(effort),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("choose the task's strategy");
}

fn run_now(task: &str, ceiling: StrategyCeiling) -> ClaimTarget {
    ClaimTarget::Run {
        task_id: task.to_string(),
        trigger: RunTrigger::Queued,
        continue_session: false,
        ceiling,
    }
}

fn plan_now(task: &str, ceiling: StrategyCeiling) -> ClaimTarget {
    ClaimTarget::Plan {
        task_id: task.to_string(),
        ceiling,
    }
}

fn next(team: &SharedTeam, ceiling: StrategyCeiling) -> ClaimTarget {
    let repository = team.repository.id.clone();
    ClaimTarget::Next {
        capacity: FreeCapacity {
            total: 1,
            per_repository: [(repository.clone(), 1)].into_iter().collect(),
        },
        repositories: vec![repository],
        wait: Duration::ZERO,
        ceiling,
    }
}

/// The sentence Bob's runner is refused `target` with.
async fn refusal(team: &SharedTeam, target: ClaimTarget) -> String {
    let error = team
        .board(&team.bob)
        .claim(target)
        .await
        .expect_err("the claim is refused");
    assert_eq!(error.code(), ErrorCode::Invalid, "{error}");
    error.to_string()
}

async fn claim(team: &SharedTeam, target: ClaimTarget) -> rimaia_core::board::Claim {
    team.board(&team.bob)
        .claim(target)
        .await
        .unwrap_or_else(|error| panic!("Bob's runner may take it: {error}"))
        .expect("nobody else holds it")
}

async fn run_state(team: &SharedTeam, task: &TaskId) -> RunState {
    tasks::get_task(&team.bob.ctx, task)
        .await
        .expect("read the task")
        .task
        .run_state
}

fn implementation(run_id: &str) -> StartRun {
    StartRun {
        run_id: run_id.to_string(),
        kind: RunKind::Implementation,
        session_id: "session-1".to_string(),
        prompt: "do the work".to_string(),
        base_ref: Some("main".to_string()),
        base_sha: None,
    }
}

fn succeeded(ceiling: StrategyCeiling) -> FinishRun {
    FinishRun {
        outcome: RunOutcome {
            exit_class: ExitClass::Success,
            status: RunStatus::Succeeded,
            error_message: None,
            num_turns: Some(4),
            cost_usd: Some(0.25),
            duration_ms: Some(1_000),
            pr_url: None,
            usage_limit_resets_at: None,
            resume_after: None,
            spawned_as: SpawnedAs::default(),
            usage: TokenUsage::default(),
        },
        head_sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        bundle: None,
        window_closes_at: None,
        transcript: TranscriptEnd::Complete { length: 0 },
        ceiling,
    }
}
