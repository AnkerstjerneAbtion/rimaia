//! Assignment, revisions, acceptance and trust, as services (ADR-0032, task
//! 045).
//!
//! A shared team of two (`testing::shared`): Alice owns it, Bob is a member,
//! and each has a runner. "Alice's runner is refused" is asked the way a
//! runner asks, through the board port's claim, so the sentence asserted is the
//! one a person reads. The clock is the test clock throughout.

use pretty_assertions::assert_eq;
use rimaia_core::board::{Claim, ClaimTarget, LeaseRef};
use rimaia_core::consent::{self, pieces::ContentKind};
use rimaia_core::db::settings;
use rimaia_core::db::{BoardColumn, ExitClass, RunKind, RunStatus};
use rimaia_core::events::TaskId;
use rimaia_core::identity::create_personal_team;
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::review;
use rimaia_core::review_loop::config as review_config;
use rimaia_core::runner::events::TokenUsage;
use rimaia_core::runner::outcome::{RunOutcome, SpawnedAs};
use rimaia_core::runner::provider::ClaudeProvider;
use rimaia_core::runner::RunTrigger;
use rimaia_core::tasks::{self, NewTask, Patch, TaskPatch};
use rimaia_core::testing::shared::{Member, SharedTeam};
use rimaia_core::testing::TempRepo;
use rimaia_core::{board, ErrorCode};
use serde_json::json;

// ---------------------------------------------------------------------------
// Revisions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn editing_a_teammates_plan_makes_it_unrunnable_for_them_until_they_accept() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", Some(&team.alice)).await;

    edit_plan(&team.bob, &task, "1. Alpha, Bob's way").await;

    assert_eq!(
        refusal(&team, &team.alice, &task).await,
        "the plan was changed by @bob, and you have not accepted that revision. Accept it, or \
         trust @bob's changes."
    );

    consent::accept(
        &team.alice.ctx,
        &team.team_id,
        Some(&task),
        ContentKind::Plan,
        "2",
    )
    .await
    .expect("accept the revision Alice read");
    claim(&team, &team.alice, &task).await;
}

#[tokio::test]
async fn saving_the_same_plan_again_is_not_a_new_revision() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", Some(&team.alice)).await;

    edit_plan(&team.bob, &task, "1. Alpha").await;

    let row = row(&team, &task).await;
    assert_eq!(row.plan_revision, 1);
    assert_eq!(
        row.plan_updated_by.as_deref(),
        Some(team.alice.user_id.as_str())
    );
    claim(&team, &team.alice, &task).await;
}

#[tokio::test]
async fn changing_a_tasks_review_instructions_is_a_new_revision() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", Some(&team.alice)).await;
    let instructions = || {
        sqlx::query_as::<_, (i64, Option<String>)>(
            "SELECT review_instructions_revision, review_instructions_updated_by
               FROM tasks WHERE id = ?1",
        )
        .bind(&task)
        .fetch_one(&team.alice.ctx.pool)
    };
    assert_eq!(instructions().await.expect("read"), (1, None));

    for _ in 0..2 {
        review_config::set_task_review(
            &team.bob.ctx,
            &ClaudeProvider,
            &task,
            Some("Check the migrations.".to_string()),
            json!(null),
        )
        .await
        .expect("set the task's review instructions");
        assert_eq!(
            instructions().await.expect("read"),
            (2, Some(team.bob.user_id.clone())),
            "the first save is a revision, and saving the same text again is not"
        );
    }

    review_config::set_task_review(&team.alice.ctx, &ClaudeProvider, &task, None, json!(null))
        .await
        .expect("clear them");
    assert_eq!(
        instructions().await.expect("read"),
        (3, Some(team.alice.user_id.clone()))
    );
}

#[tokio::test]
async fn requesting_changes_is_a_new_plan_revision_by_the_reviewer() {
    for verdict in ["request_changes", "reject"] {
        let team = SharedTeam::new().await;
        let task = task_in(
            &team,
            &team.alice,
            "Alpha",
            BoardColumn::InReview,
            Some(&team.alice),
        )
        .await;

        match verdict {
            "request_changes" => review::request_changes(&team.bob.ctx, &task, "Add a test.")
                .await
                .map(drop),
            _ => review::reject(&team.bob.ctx, None, &task, "Start over.")
                .await
                .map(drop),
        }
        .expect("the reviewer decides");

        let after = row(&team, &task).await;
        assert_eq!(after.plan_revision, 2, "{verdict}");
        assert_eq!(
            after.plan_updated_by.as_deref(),
            Some(team.bob.user_id.as_str())
        );
        assert_eq!(
            refusal(&team, &team.alice, &task).await,
            "the plan was changed by @bob, and you have not accepted that revision. Accept it, \
             or trust @bob's changes.",
            "{verdict}"
        );
        consent::accept(
            &team.alice.ctx,
            &team.team_id,
            Some(&task),
            ContentKind::Plan,
            "2",
        )
        .await
        .expect("accept the reviewer's note");
        claim(&team, &team.alice, &task).await;
    }

    // 034's table refuses a verdict on a card that is not in review, and a
    // refused move bumps nothing, as it appends nothing.
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", Some(&team.alice)).await;
    review::request_changes(&team.bob.ctx, &task, "Add a test.")
        .await
        .expect_err("a ready task is not in review");
    let after = row(&team, &task).await;
    assert_eq!(after.plan_revision, 1);
    assert_eq!(
        after.plan_updated_by.as_deref(),
        Some(team.alice.user_id.as_str())
    );
}

#[tokio::test]
async fn base_instructions_edits_need_acceptance_from_members_who_do_not_trust_the_editor() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", Some(&team.alice)).await;

    settings::set_base_instructions(&team.bob.ctx, "Use tabs.")
        .await
        .expect("Bob edits the team's base instructions");

    assert_eq!(
        refusal(&team, &team.alice, &task).await,
        "the team's base instructions was changed by @bob, and you have not accepted that \
         revision. Accept it, or trust @bob's changes."
    );

    consent::set_trust(&team.alice.ctx, &team.team_id, &team.bob.user_id, true)
        .await
        .expect("Alice trusts Bob");
    claim(&team, &team.alice, &task).await;
}

// ---------------------------------------------------------------------------
// Acceptance and trust
// ---------------------------------------------------------------------------

#[tokio::test]
async fn accepting_a_stale_revision_is_invalid_naming_the_current_one() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", Some(&team.alice)).await;
    edit_plan(&team.bob, &task, "1. Alpha, again").await;
    edit_plan(&team.bob, &task, "1. Alpha, once more").await;

    let error = consent::accept(
        &team.alice.ctx,
        &team.team_id,
        Some(&task),
        ContentKind::Plan,
        "2",
    )
    .await
    .expect_err("revision 2 was read before Bob's second edit");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        "revision 2 is not current: the plan is at revision 3, changed by @bob. Read it before \
         accepting it."
    );
    assert_eq!(
        refusal(&team, &team.alice, &task).await,
        "the plan was changed by @bob, and you have not accepted that revision. Accept it, or \
         trust @bob's changes.",
        "nothing was recorded"
    );
}

#[tokio::test]
async fn accepting_base_instructions_names_the_team() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", Some(&team.alice)).await;
    settings::set_base_instructions(&team.bob.ctx, "Use tabs.")
        .await
        .expect("Bob edits the team's base instructions");

    let error = consent::accept(
        &team.alice.ctx,
        &team.team_id,
        Some(&task),
        ContentKind::BaseInstructions,
        "1",
    )
    .await
    .expect_err("base instructions belong to the team, not to a task");
    assert_eq!(error.code(), ErrorCode::Invalid);

    for _ in 0..2 {
        consent::accept(
            &team.alice.ctx,
            &team.team_id,
            None,
            ContentKind::BaseInstructions,
            "1",
        )
        .await
        .expect("accepting names the team, and twice is idempotent");
    }
    claim(&team, &team.alice, &task).await;

    // A task in another team is not this team's to accept for: Alice's own
    // personal team, which her shared-team context does not reach, reads as
    // a task that does not exist.
    let elsewhere = personal_task(&team, &team.alice).await;
    let error = consent::accept(
        &team.alice.ctx,
        &team.team_id,
        Some(&elsewhere.0),
        ContentKind::Plan,
        "1",
    )
    .await
    .expect_err("a task in another team");
    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(
        error.to_string(),
        format!("no task with id {}", elsewhere.0)
    );
}

#[tokio::test]
async fn revoking_trust_stops_the_next_claim() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", Some(&team.alice)).await;
    consent::set_trust(&team.alice.ctx, &team.team_id, &team.bob.user_id, true)
        .await
        .expect("trust Bob");
    edit_plan(&team.bob, &task, "1. Alpha, Bob's way").await;
    team.board(&team.alice)
        .preview(&task)
        .await
        .expect("Bob's changes count while Alice trusts him");

    let listed = consent::set_trust(&team.alice.ctx, &team.team_id, &team.bob.user_id, false)
        .await
        .expect("revoke the trust");
    assert_eq!(listed, Vec::<String>::new());

    assert_eq!(
        refusal(&team, &team.alice, &task).await,
        "the plan was changed by @bob, and you have not accepted that revision. Accept it, or \
         trust @bob's changes."
    );
}

// ---------------------------------------------------------------------------
// Point 6's mark
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_plan_bob_wrote_while_his_runner_ran_alices_plan_needs_his_own_acceptance() {
    let team = SharedTeam::new().await;
    let alices = team.task(&team.alice, "Alice's", Some(&team.bob)).await;
    let bobs = team.task(&team.bob, "Bob's", Some(&team.bob)).await;
    consent::set_trust(&team.bob.ctx, &team.team_id, &team.alice.user_id, true)
        .await
        .expect("Bob trusts Alice, so his runner takes her plan");

    let running = claim(&team, &team.bob, &alices).await;
    edit_plan(&team.bob, &bobs, "1. Whatever the agent decided").await;
    team.board(&team.bob)
        .release(&running.lease)
        .await
        .expect("the run ends");

    assert!(row(&team, &bobs).await.plan_written_during_run);
    assert_eq!(
        refusal(&team, &team.bob, &bobs).await,
        "the plan was written with @bob's credentials during a run on someone else's task. Only \
         accepting that revision lets it run."
    );

    consent::accept(
        &team.bob.ctx,
        &team.team_id,
        Some(&bobs),
        ContentKind::Plan,
        "2",
    )
    .await
    .expect("Bob reads it and accepts it");
    claim(&team, &team.bob, &bobs).await;
}

#[tokio::test]
async fn a_plan_written_into_a_personal_team_during_a_shared_team_run_is_marked() {
    let team = SharedTeam::new().await;
    let alices = team.task(&team.alice, "Alice's", Some(&team.bob)).await;
    consent::set_trust(&team.bob.ctx, &team.team_id, &team.alice.user_id, true)
        .await
        .expect("Bob trusts Alice");
    let (personal, _repository) = personal_task(&team, &team.bob).await;
    assert!(!row(&team, &personal).await.plan_written_during_run);

    let _running = claim(&team, &team.bob, &alices).await;
    let task = tasks::update_task(
        &team.bob.personal,
        &personal,
        TaskPatch {
            plan: Patch::Set("1. Into Bob's own board".to_string()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("Bob writes into his personal team");

    assert_eq!(task.plan_revision, 2);
    assert!(
        row(&team, &personal).await.plan_written_during_run,
        "the mark reads across every team the actor's runners hold leases in"
    );
}

#[tokio::test]
async fn planning_while_your_runner_works_on_your_own_task_is_not_marked() {
    let team = SharedTeam::new().await;
    let first = team.task(&team.bob, "First", Some(&team.bob)).await;
    let second = team.task(&team.bob, "Second", Some(&team.bob)).await;

    let _running = claim(&team, &team.bob, &first).await;
    edit_plan(&team.bob, &second, "1. Second, refined").await;

    let after = row(&team, &second).await;
    assert_eq!(after.plan_revision, 2);
    assert!(!after.plan_written_during_run);
}

// ---------------------------------------------------------------------------
// The base commit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_dependency_commit_from_another_owners_runner_needs_trust_or_acceptance() {
    let team = SharedTeam::new().await;
    let dependency = team.task(&team.alice, "Schema", Some(&team.alice)).await;
    let head = "0123456789abcdef0123456789abcdef01234567";
    succeed(&team, &team.alice, &dependency, head).await;
    let task = team.task(&team.bob, "Endpoint", Some(&team.bob)).await;
    tasks::set_task_dependencies(&team.bob.ctx, &task, std::slice::from_ref(&dependency))
        .await
        .expect("the endpoint builds on the schema");

    let refused = format!(
        "commit {head} from \"Schema\" was changed by @alice, and you have not accepted that \
         revision. Accept it, or trust @alice's changes."
    );
    assert_eq!(refusal(&team, &team.bob, &task).await, refused);

    consent::set_trust(&team.bob.ctx, &team.team_id, &team.alice.user_id, true)
        .await
        .expect("trust Alice");
    team.board(&team.bob)
        .preview(&task)
        .await
        .expect("trusting the commit's owner consents");
    consent::set_trust(&team.bob.ctx, &team.team_id, &team.alice.user_id, false)
        .await
        .expect("and stop");
    assert_eq!(refusal(&team, &team.bob, &task).await, refused);

    consent::accept(
        &team.bob.ctx,
        &team.team_id,
        Some(&dependency),
        ContentKind::BaseCommit,
        head,
    )
    .await
    .expect("accept the commit, named by the dependency that made it");
    claim(&team, &team.bob, &task).await;
}

// ---------------------------------------------------------------------------
// Assignment and the team ceiling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_member_cannot_assign_a_task_to_someone_outside_the_team() {
    let team = SharedTeam::new().await;
    let task = team.task(&team.alice, "Alpha", None).await;
    let carol = {
        let mut conn = team.alice.ctx.pool.acquire().await.expect("a connection");
        create_personal_team(&mut conn, &team.clock, "carol")
            .await
            .expect("carol signs up")
            .user_id
    };

    let outsider = tasks::assign_task(&team.bob.ctx, &task, Some(&carol))
        .await
        .expect_err("carol is not a member");
    let nobody = tasks::assign_task(&team.bob.ctx, &task, Some("never-issued"))
        .await
        .expect_err("nobody has that id");
    assert_eq!(outsider.code(), ErrorCode::NotFound);
    assert_eq!(
        outsider.to_string().replace(&carol, "never-issued"),
        nobody.to_string(),
        "an outsider reads exactly as a user who does not exist"
    );
    assert_eq!(row(&team, &task).await.assignee_id, None);

    let assigned = tasks::assign_task(&team.bob.ctx, &task, Some(&team.alice.user_id))
        .await
        .expect("assign a member");
    assert_eq!(
        assigned.assignee_id.as_deref(),
        Some(team.alice.user_id.as_str())
    );
    assert_eq!(
        assigned.assigned_by.as_deref(),
        Some(team.bob.user_id.as_str())
    );
    let pooled = tasks::assign_task(&team.bob.ctx, &task, None)
        .await
        .expect("back to the pool");
    assert_eq!(pooled.assignee_id, None);
}

#[tokio::test]
async fn a_member_cannot_set_the_team_ceiling() {
    let team = SharedTeam::new().await;

    let error = repo::set_repository_unattended_ceiling(&team.bob.ctx, &team.repository.id, false)
        .await
        .expect_err("Bob is a member, not an owner");
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        "only an owner of this team can change whether it allows unattended runs."
    );

    repo::set_repository_unattended_ceiling(&team.alice.ctx, &team.repository.id, false)
        .await
        .expect("Alice owns the team");

    // A personal team has no ceiling, for its owner too.
    let (_, personal) = personal_task(&team, &team.alice).await;
    let error = repo::set_repository_unattended_ceiling(&team.alice.personal, &personal, true)
        .await
        .expect_err("a personal team has no ceiling");
    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        "a personal team has no ceiling: this machine's own consent decides."
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// [`SharedTeam::task`], in `column`.
async fn task_in(
    team: &SharedTeam,
    by: &Member,
    title: &str,
    column: BoardColumn,
    assignee: Option<&Member>,
) -> TaskId {
    let task = tasks::create_task(
        &by.ctx,
        NewTask {
            repository_id: team.repository.id.clone(),
            title: title.to_string(),
            plan: Some(format!("1. {title}")),
            extra_instructions: None,
            column: Some(column),
            links: vec![],
        },
    )
    .await
    .expect("create a task");
    tasks::assign_task(&by.ctx, &task.id, assignee.map(|m| m.user_id.as_str()))
        .await
        .expect("assign it");
    task.id
}

/// The columns this file asserts on, read straight off the row.
#[derive(Debug, sqlx::FromRow)]
struct Row {
    plan_revision: i64,
    plan_updated_by: Option<String>,
    plan_written_during_run: bool,
    assignee_id: Option<String>,
}

async fn row(team: &SharedTeam, task: &str) -> Row {
    sqlx::query_as(
        "SELECT plan_revision, plan_updated_by, plan_written_during_run, assignee_id
           FROM tasks WHERE id = ?1",
    )
    .bind(task)
    .fetch_one(&team.alice.ctx.pool)
    .await
    .expect("read the task's row")
}

async fn edit_plan(by: &Member, task: &str, plan: &str) {
    tasks::update_task(
        &by.ctx,
        task,
        TaskPatch {
            plan: Patch::Set(plan.to_string()),
            ..TaskPatch::default()
        },
    )
    .await
    .expect("edit the plan");
}

fn run_now(task: &str) -> ClaimTarget {
    ClaimTarget::Run {
        task_id: task.to_string(),
        trigger: RunTrigger::Queued,
        continue_session: false,
        ceiling: Default::default(),
    }
}

/// The sentence `member`'s runner is refused `task` with.
async fn refusal(team: &SharedTeam, member: &Member, task: &str) -> String {
    let error = team
        .board(member)
        .claim(run_now(task))
        .await
        .expect_err("the claim is refused");
    assert_eq!(error.code(), ErrorCode::Invalid, "{error}");
    error.to_string()
}

/// `member`'s runner claims `task`.
async fn claim(team: &SharedTeam, member: &Member, task: &str) -> Claim {
    team.board(member)
        .claim(run_now(task))
        .await
        .unwrap_or_else(|error| panic!("{}'s runner may take it: {error}", member.login))
        .expect("nobody else holds it")
}

/// A task with a plan in `member`'s personal team, and the repository it is
/// in, registered from a clone of its own.
async fn personal_task(team: &SharedTeam, member: &Member) -> (TaskId, String) {
    let source = TempRepo::init();
    let repository = repo::register(
        &member.personal,
        &team.machine,
        &team.paths.worktrees_dir(),
        NewRepository {
            path: source.path().to_string_lossy().into_owned(),
            name: Some(format!("{}-own", member.login)),
            worktree_root: None,
        },
    )
    .await
    .expect("register a repository in the personal team");
    // The clone only has to exist while it registers: nothing here runs.
    drop(source);
    let task = tasks::create_task(
        &member.personal,
        NewTask {
            repository_id: repository.id.clone(),
            title: "Own".to_string(),
            plan: Some("1. Mine".to_string()),
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect("create a personal task");
    (task.id, repository.id)
}

/// `member`'s runner runs `task`'s implementation to success at `head`.
async fn succeed(team: &SharedTeam, member: &Member, task: &str, head: &str) {
    let port = team.board(member);
    let claimed = claim(team, member, task).await;
    let lease: &LeaseRef = &claimed.lease;
    port.start_run(
        lease,
        board::StartRun {
            run_id: format!("run-{task}"),
            kind: RunKind::Implementation,
            session_id: "session-1".to_string(),
            prompt: "do the work".to_string(),
            base_ref: Some("main".to_string()),
            base_sha: None,
        },
    )
    .await
    .expect("open the run");
    port.finish_run(
        lease,
        &format!("run-{task}"),
        board::FinishRun {
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
            head_sha: Some(head.to_string()),
            bundle: None,
            window_closes_at: None,
            transcript: board::TranscriptEnd::Complete { length: 0 },
            ceiling: Default::default(),
        },
    )
    .await
    .expect("finish the run");
}
