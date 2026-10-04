//! The analytics page, from the outside (task 024, ADR-0022, seam-contract D18).
//!
//! Every figure is computed from `runs` at read time, so every test here builds
//! real rows through `start_run`/`finish_run` — the only writer — and then
//! checks the page against the database rather than against the values it fed
//! in. The three assertions that matter most are the ones about what *must not*
//! change a total: pruning a transcript, and a NULL cost.

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::analytics::{self, Period};
use rimaia_core::db::settings;
use rimaia_core::db::{
    BoardColumn, ExitClass, MutationSource, RunKind, RunState, RunStatus, StrategyMode,
};
use rimaia_core::mcp::requests::AnalyticsRequest;
use rimaia_core::mcp::responses::{AnalyticsView, RunOutcomesView};
use rimaia_core::mcp::RimaiaServer;
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::TokenUsage;
use rimaia_core::runner::outcome::{finish_run, start_run, NewRun, RunOutcome, SpawnedAs};
use rimaia_core::runs::bundle::RunCapture;
use rimaia_core::runs::{self, PruneCriterion};
use rimaia_core::tasks::{self, NewTask, TaskPatch};
use rimaia_core::testing::{self, TempRepo, TestContext};
use rimaia_core::{AppPaths, Clock};
use rmcp::handler::server::wrapper::{Json, Parameters};
use tempfile::TempDir;

struct Fixture {
    harness: TestContext,
    #[allow(dead_code)]
    repository: TempRepo,
    #[allow(dead_code)]
    data: TempDir,
    paths: AppPaths,
    repository_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let repository = TempRepo::init();
        let data = tempfile::Builder::new()
            .prefix("rimaia-analytics-")
            .tempdir()
            .expect("a temporary app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");

        let registered = repo::register(
            &harness.context,
            &paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register the test repository");

        Self {
            harness,
            repository,
            data,
            paths,
            repository_id: registered.id,
        }
    }

    async fn task(&self, title: &str, column: BoardColumn) -> String {
        let task = tasks::create_task(
            &self.harness.context,
            NewTask {
                repository_id: self.repository_id.clone(),
                title: title.to_string(),
                plan: Some("a plan".to_string()),
                extra_instructions: None,
                column: Some(column),
                links: vec![],
            },
        )
        .await
        .expect("create a task");
        task.id
    }

    /// One finished attempt, with a cost and a model — or without, which is the
    /// case D18 exists for.
    async fn attempt(
        &self,
        task_id: &str,
        status: RunStatus,
        cost_usd: Option<f64>,
        model: Option<&str>,
    ) -> String {
        self.claim(task_id).await;
        let run = start_run(
            &self.harness.context,
            &self.paths,
            NewRun {
                task_id: task_id.to_string(),
                kind: RunKind::Implementation,
                session_id: "session".to_string(),
                prompt: "a prompt".to_string(),
                base_ref: None,
                base_sha: None,
            },
        )
        .await
        .expect("open a run row");

        // The transcript the prune test deletes. `start_run` records the path;
        // nothing creates the file, because the runner streams into it.
        std::fs::create_dir_all(
            std::path::Path::new(&run.log_path)
                .parent()
                .expect("a transcript lives in a directory"),
        )
        .expect("the transcript directory");
        std::fs::write(&run.log_path, "{}\n").expect("a transcript to prune");

        finish_run(
            &self.harness.context,
            &run.id,
            &RunOutcome {
                exit_class: match status {
                    RunStatus::Succeeded => ExitClass::Success,
                    RunStatus::Cancelled => ExitClass::Cancelled,
                    RunStatus::Interrupted => ExitClass::Interrupted,
                    _ => ExitClass::Fatal,
                },
                status,
                error_message: None,
                num_turns: Some(4),
                cost_usd,
                duration_ms: None,
                pr_url: None,
                usage_limit_resets_at: None,
                resume_after: None,
                spawned_as: SpawnedAs {
                    model: model.map(str::to_string),
                    effort: None,
                    run_environment: None,
                },
                usage: TokenUsage::default(),
            },
            &RunCapture::default(),
        )
        .await
        .expect("close the run row");

        run.id
    }

    /// One review or fix row, closed the way task 021's arms will close it.
    ///
    /// `finish_run` refuses these kinds until 021, so the row is closed through
    /// `testing::runs::close_run` and its cost written beside it, which is the
    /// one capture column this page reads.
    async fn loop_run(
        &self,
        task_id: &str,
        kind: RunKind,
        status: RunStatus,
        minutes: i64,
        cost_usd: f64,
    ) -> String {
        let run = start_run(
            &self.harness.context,
            &self.paths,
            NewRun {
                task_id: task_id.to_string(),
                kind,
                session_id: "loop-session".to_string(),
                prompt: "a prompt".to_string(),
                base_ref: None,
                base_sha: None,
            },
        )
        .await
        .expect("open a run row");
        self.harness.clock.advance(Duration::minutes(minutes));
        let class = match status {
            RunStatus::Succeeded => ExitClass::Success,
            _ => ExitClass::Fatal,
        };
        testing::runs::close_run(&self.harness.context, &run.id, status, class, None).await;
        sqlx::query("UPDATE runs SET cost_usd = ?1 WHERE id = ?2")
            .bind(cost_usd)
            .bind(&run.id)
            .execute(&self.harness.context.pool)
            .await
            .expect("record the loop run's cost");
        run.id
    }

    /// Walks the card into `running`, which is what `finish_run` closes out.
    ///
    /// `Idle -> Queued -> Running` is the real path every run takes; skipping it
    /// makes `apply_to_task` refuse an illegal transition, which is the state
    /// machine doing its job rather than something to work around.
    async fn claim(&self, task_id: &str) {
        for state in [RunState::Queued, RunState::Running] {
            tasks::set_run_state(&self.harness.context, task_id, state)
                .await
                .expect("walk the card into running");
        }
    }

    async fn page(&self) -> analytics::Analytics {
        analytics::analytics(&self.harness.context.pool, Period::default())
            .await
            .expect("the page is a read")
    }

    /// The same sum, straight out of SQLite. Task 024's acceptance criterion
    /// asks for exactly this comparison rather than a comparison against the
    /// numbers the test fed in — an aggregate that agreed with the fixture but
    /// not with the table would be the bug it is written to catch.
    async fn sql_spend(&self) -> f64 {
        sqlx::query_scalar::<_, Option<f64>>("SELECT SUM(cost_usd) FROM runs")
            .fetch_one(&self.harness.context.pool)
            .await
            .expect("sum the costs")
            .unwrap_or(0.0)
    }
}

#[tokio::test]
async fn spend_for_a_period_matches_a_direct_sum_over_the_same_rows() {
    let fixture = Fixture::new().await;
    let task = fixture.task("Wire the board", BoardColumn::Done).await;
    fixture
        .attempt(&task, RunStatus::Failed, Some(0.25), Some("sonnet"))
        .await;
    fixture
        .attempt(&task, RunStatus::Succeeded, Some(1.5), Some("opus"))
        .await;

    let page = fixture.page().await;

    assert_eq!(page.spend_usd, fixture.sql_spend().await);
    assert_eq!(page.spend_usd, 1.75);
    assert_eq!(page.outcomes.succeeded, 1);
    assert_eq!(page.outcomes.failed, 1);
    assert_eq!(page.outcomes.total(), 2);
    assert_eq!(page.outcomes.failure_rate(), Some(0.5));
}

#[tokio::test]
async fn a_run_that_recorded_no_cost_is_counted_as_unrecorded_rather_than_as_zero() {
    // Seam-contract D18's whole point. A NULL and a 0.0 describe different
    // worlds — nothing recorded, and a run that genuinely cost nothing — and
    // averaging them together is a claim about history that is false.
    let fixture = Fixture::new().await;
    let task = fixture
        .task("From before the columns", BoardColumn::Done)
        .await;
    fixture
        .attempt(&task, RunStatus::Succeeded, None, None)
        .await;
    fixture
        .attempt(&task, RunStatus::Succeeded, Some(2.0), Some("sonnet"))
        .await;

    let page = fixture.page().await;

    assert_eq!(
        page.spend_usd, 2.0,
        "the sum is over the rows that have one"
    );
    assert_eq!(
        page.runs_without_cost, 1,
        "and the page has to be able to say the period is partly unrecorded",
    );
    assert_eq!(page.runs_without_model, 1);
    // The unrecorded run is not a model in the mix, because a bucket in the
    // chart would read as a model somebody chose.
    assert_eq!(page.models.len(), 1);
    assert_eq!(page.models[0].model, "sonnet");
    assert_eq!(page.models[0].runs, 1);
}

#[tokio::test]
async fn cost_per_completed_task_counts_the_attempts_that_failed() {
    // Task 024's Notes name this as the one number that will be wrong if
    // nobody thinks about it. Four failures and one success cost all five runs.
    let fixture = Fixture::new().await;
    let task = fixture.task("Hard one", BoardColumn::Done).await;
    for _ in 0..4 {
        fixture
            .attempt(&task, RunStatus::Failed, Some(1.0), Some("sonnet"))
            .await;
    }
    fixture
        .attempt(&task, RunStatus::Succeeded, Some(1.0), Some("sonnet"))
        .await;

    let page = fixture.page().await;

    assert_eq!(page.tasks_attempted, 1);
    assert_eq!(page.tasks_completed, 1);
    assert_eq!(
        page.cost_per_completed_task_usd,
        Some(5.0),
        "the flattering version would say 1.00, which hides the four failures",
    );
}

#[tokio::test]
async fn nothing_completed_has_no_cost_per_task_rather_than_a_zero_one() {
    let fixture = Fixture::new().await;
    let task = fixture.task("Still going", BoardColumn::Ready).await;
    fixture
        .attempt(&task, RunStatus::Failed, Some(0.5), Some("sonnet"))
        .await;

    let page = fixture.page().await;

    assert_eq!(page.tasks_attempted, 1);
    assert_eq!(page.tasks_completed, 0);
    assert_eq!(page.cost_per_completed_task_usd, None);
}

#[tokio::test]
async fn pruning_run_logs_changes_nothing_on_the_page() {
    // ADR-0022 part 2, asserted from the reader's side: task 015 deletes the
    // JSONL file and leaves the row, so every total here has to survive it
    // byte for byte.
    let fixture = Fixture::new().await;
    let task = fixture.task("Pruned", BoardColumn::Done).await;
    fixture
        .attempt(&task, RunStatus::Succeeded, Some(3.25), Some("opus"))
        .await;

    let before = fixture.page().await;

    let pruned = runs::prune_logs(
        &fixture.harness.context,
        &fixture.paths,
        PruneCriterion::Task(task.clone()),
    )
    .await
    .expect("prune the transcripts");
    assert_eq!(
        pruned.runs_pruned, 1,
        "the test needs a prune that happened"
    );

    assert_eq!(fixture.page().await, before);
}

#[tokio::test]
async fn deleting_a_task_removes_its_runs_from_every_figure() {
    // The cascade is a person saying it never happened, so the page has to
    // agree — a total that outlived the row it came from would be an aggregate
    // in disguise.
    let fixture = Fixture::new().await;
    let kept = fixture.task("Kept", BoardColumn::Done).await;
    let deleted = fixture.task("Deleted", BoardColumn::Done).await;
    fixture
        .attempt(&kept, RunStatus::Succeeded, Some(1.0), Some("sonnet"))
        .await;
    fixture
        .attempt(&deleted, RunStatus::Succeeded, Some(9.0), Some("opus"))
        .await;

    assert_eq!(fixture.page().await.spend_usd, 10.0);

    tasks::delete_task(&fixture.harness.context, &deleted)
        .await
        .expect("delete the task");

    let page = fixture.page().await;
    assert_eq!(page.spend_usd, 1.0);
    assert_eq!(page.spend_usd, fixture.sql_spend().await);
    assert_eq!(page.outcomes.total(), 1);
    assert_eq!(page.tasks_attempted, 1);
}

#[tokio::test]
async fn a_period_scopes_every_figure_and_two_adjacent_ones_never_share_a_run() {
    let fixture = Fixture::new().await;
    let task = fixture.task("Over two days", BoardColumn::Done).await;

    let first = fixture
        .attempt(&task, RunStatus::Succeeded, Some(1.0), Some("sonnet"))
        .await;
    fixture.harness.clock.advance(Duration::days(2));
    let second = fixture
        .attempt(&task, RunStatus::Succeeded, Some(4.0), Some("opus"))
        .await;
    assert_ne!(first, second);

    let boundary = fixture.harness.clock.now() - Duration::days(1);
    let earlier = analytics::analytics(
        &fixture.harness.context.pool,
        Period {
            from: None,
            to: Some(boundary),
        },
    )
    .await
    .expect("the earlier period");
    let later = analytics::analytics(
        &fixture.harness.context.pool,
        Period {
            from: Some(boundary),
            to: None,
        },
    )
    .await
    .expect("the later period");

    assert_eq!(earlier.spend_usd, 1.0);
    assert_eq!(later.spend_usd, 4.0);
    assert_eq!(
        earlier.outcomes.total() + later.outcomes.total(),
        2,
        "an inclusive `from` and an exclusive `to` mean no run is in both",
    );
}

#[tokio::test]
async fn the_subscription_comparison_is_absent_until_the_user_gives_a_figure() {
    let fixture = Fixture::new().await;

    assert_eq!(fixture.page().await.subscription_monthly_usd, None);

    settings::set_subscription_monthly_usd(&fixture.harness.context, Some(20.0))
        .await
        .expect("store the user's own figure");
    assert_eq!(fixture.page().await.subscription_monthly_usd, Some(20.0));

    // Cleared, not zeroed: a zero would be a claim that the subscription is
    // free, and the page renders the comparison off `Some`.
    settings::set_subscription_monthly_usd(&fixture.harness.context, None)
        .await
        .expect("clear it");
    assert_eq!(fixture.page().await.subscription_monthly_usd, None);

    let refusal = settings::set_subscription_monthly_usd(&fixture.harness.context, Some(-1.0))
        .await
        .expect_err("a negative monthly cost is not one");
    assert!(refusal.to_string().contains("zero or more"), "{refusal}");
}

#[tokio::test]
async fn a_hand_edited_subscription_row_reads_as_absent_rather_than_failing_the_page() {
    let fixture = Fixture::new().await;

    settings::set(
        &fixture.harness.context,
        settings::SUBSCRIPTION_MONTHLY_USD,
        "twenty dollars",
    )
    .await
    .expect("store a typo");

    assert_eq!(fixture.page().await.subscription_monthly_usd, None);
}

#[tokio::test]
async fn the_strategy_mix_counts_runs_by_the_mode_their_task_carries() {
    let fixture = Fixture::new().await;
    let planned = fixture.task("Planned", BoardColumn::Done).await;
    let plain = fixture.task("Plain", BoardColumn::Done).await;
    tasks::update_task(
        &fixture.harness.context,
        &planned,
        TaskPatch {
            strategy_mode: Some(StrategyMode::Planned),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("set the mode");

    fixture
        .attempt(&planned, RunStatus::Succeeded, Some(2.0), Some("opus"))
        .await;
    fixture
        .attempt(&plain, RunStatus::Succeeded, Some(1.0), Some("sonnet"))
        .await;

    let page = fixture.page().await;
    let planned_runs = page
        .strategies
        .iter()
        .find(|entry| entry.mode == StrategyMode::Planned)
        .expect("the planned mode is in the mix");

    assert_eq!(planned_runs.runs, 1);
    assert_eq!(planned_runs.spend_usd, 2.0);
    assert_eq!(page.strategies.len(), 2);
}

#[tokio::test]
async fn the_longest_run_and_the_median_are_measured_rather_than_averaged() {
    let fixture = Fixture::new().await;
    let task = fixture.task("Timed", BoardColumn::Done).await;

    // `finish_run` stamps `ended_at` from the injected clock, so a run's
    // duration is exactly what the test advanced it by — CLAUDE.md's "fake the
    // clock, never sleep".
    for minutes in [1_i64, 5, 30] {
        fixture.claim(&task).await;
        let run = start_run(
            &fixture.harness.context,
            &fixture.paths,
            NewRun {
                task_id: task.clone(),
                kind: RunKind::Implementation,
                session_id: "session".to_string(),
                prompt: "a prompt".to_string(),
                base_ref: None,
                base_sha: None,
            },
        )
        .await
        .expect("open a run row");
        fixture.harness.clock.advance(Duration::minutes(minutes));
        finish_run(
            &fixture.harness.context,
            &run.id,
            &RunOutcome {
                exit_class: ExitClass::Success,
                status: RunStatus::Succeeded,
                error_message: None,
                num_turns: Some(1),
                cost_usd: Some(0.5),
                duration_ms: None,
                pr_url: None,
                usage_limit_resets_at: None,
                resume_after: None,
                spawned_as: SpawnedAs::default(),
                usage: TokenUsage::default(),
            },
            &RunCapture::default(),
        )
        .await
        .expect("close the run row");
    }

    let page = fixture.page().await;

    assert_eq!(page.median_duration_seconds, Some(5 * 60));
    assert_eq!(
        page.longest_run.as_ref().map(|run| run.seconds),
        Some(30 * 60)
    );
    assert_eq!(
        page.longest_run.as_ref().map(|run| run.title.as_str()),
        Some("Timed"),
        "the longest run is named, not only measured",
    );
    // 1 + 5 + 30 minutes, summed — parallel runs each contribute their own.
    assert_eq!(page.unattended_hours, 36.0 / 60.0);
}

#[tokio::test]
async fn implementation_and_review_loop_spend_sum_to_total_spend() {
    // D29 point 7: a task's review loop is part of what it cost, so spend
    // counts every kind, but a review's `succeeded` says the reviewer ran, not
    // that the work was good, so the outcomes, the failure rate, the median
    // and the strategy mix count implementation runs only.
    let fixture = Fixture::new().await;
    let looped = fixture.task("Looped", BoardColumn::InReview).await;
    let failed = fixture.task("Failed", BoardColumn::Ready).await;
    fixture
        .attempt(&looped, RunStatus::Succeeded, Some(2.0), Some("opus"))
        .await;
    fixture
        .attempt(&failed, RunStatus::Failed, Some(1.0), Some("opus"))
        .await;
    let before = fixture.page().await;

    fixture
        .loop_run(&looped, RunKind::Review, RunStatus::Succeeded, 3, 0.5)
        .await;
    fixture
        .loop_run(&looped, RunKind::Fix, RunStatus::Failed, 40, 0.25)
        .await;
    let after = fixture.page().await;

    assert_eq!(after.outcomes, before.outcomes);
    assert_eq!(after.outcomes.failure_rate(), Some(0.5));
    assert_eq!(
        after.median_duration_seconds,
        before.median_duration_seconds
    );
    assert_eq!(after.strategies, before.strategies);
    assert_eq!(after.review_loop_outcomes.succeeded, 1);
    assert_eq!(after.review_loop_outcomes.failed, 1);
    assert_eq!(after.review_loop_outcomes.total(), 2);
    assert_eq!(after.implementation_spend_usd, 3.0);
    assert_eq!(after.review_loop_spend_usd, 0.75);
    assert_eq!(
        after.spend_usd,
        after.implementation_spend_usd + after.review_loop_spend_usd
    );
    assert_eq!(after.spend_usd, fixture.sql_spend().await);
    assert_eq!(
        after.longest_run.as_ref().map(|run| run.seconds),
        Some(40 * 60),
        "the longest run counts every kind",
    );

    // ADR-0021: the agent's door reports what the window's door reports.
    let server = RimaiaServer::new(
        fixture.harness.context.with_source(MutationSource::Mcp),
        testing::doctor::environment(),
        testing::doctor::planner_access(),
    );
    let request: AnalyticsRequest =
        serde_json::from_value(serde_json::json!({})).expect("an empty period");
    let Json(view) = server
        .get_analytics(Parameters(request))
        .await
        .expect("analytics over MCP");
    assert_eq!(view, AnalyticsView::from(&after));
    assert_eq!(view.review_loop_spend_usd, 0.75);
    assert_eq!(
        view.review_loop_outcomes,
        RunOutcomesView {
            runs_total: 2,
            runs_succeeded: 1,
            runs_failed: 1,
            runs_cancelled: 0,
            runs_interrupted: 0,
            runs_running: 0,
            failure_rate: Some(0.5),
        },
    );
    assert_eq!(
        view.runs_total, 2,
        "the flat counts are implementation runs"
    );
}

#[tokio::test]
async fn a_page_with_no_runs_at_all_is_a_page_of_zeroes_rather_than_an_error() {
    let fixture = Fixture::new().await;

    let page = fixture.page().await;

    assert_eq!(page.spend_usd, 0.0);
    assert_eq!(page.outcomes.total(), 0);
    assert_eq!(page.outcomes.failure_rate(), None);
    assert_eq!(page.median_duration_seconds, None);
    assert_eq!(page.longest_run, None);
    assert!(page.models.is_empty());
    assert_eq!(page.cost_per_completed_task_usd, None);
}

/// A period bound parsed the way the board sends one, so the shape crossing the
/// boundary is the shape the tests use.
#[allow(dead_code)]
fn instant(text: &str) -> DateTime<Utc> {
    text.parse().expect("an RFC 3339 instant")
}
