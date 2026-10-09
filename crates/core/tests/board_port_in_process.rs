//! The board port's contract suite, against the in-process adapter (ADR-0027
//! point 5, seam-contract D31 point 13, task 036).
//!
//! Two runners over one board, both built by `TestContext::board` over one
//! test context. Task 052 invokes the same macro over HTTP; nothing in a case
//! knows which adapter it is talking to.

use std::sync::Arc;

use chrono::TimeDelta;
use pretty_assertions::assert_eq;
use rimaia_core::board::{
    BoardMethod, BoardPort, Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat,
    InProcessBoard, LeasePurpose, LeaseRef, NextStep, RunContext, StartRun, TeamLimits,
    TranscriptAck, TranscriptChunk, TranscriptEnd,
};
use rimaia_core::db::{BoardColumn, ExitClass, RunKind, RunStatus};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::TokenUsage;
use rimaia_core::runner::outcome::{RunOutcome, SpawnedAs};
use rimaia_core::runner::{RunTrigger, RunnerConfig};
use rimaia_core::runs::bundle::ReviewBundle;
use rimaia_core::scheduler::ResumePoint;
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::board_contract::{Harness, Which};
use rimaia_core::testing::db::insert_runner;
use rimaia_core::testing::{TempRepo, TestClock, TestContext};
use rimaia_core::{AppPaths, Clock, ServiceContext};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tempfile::TempDir;

struct InProcess {
    harness: TestContext,
    /// Held for its `Drop`: the boards' transcript paths point inside it.
    _data: TempDir,
    a: Arc<dyn BoardPort>,
    b: Arc<dyn BoardPort>,
}

impl Harness for InProcess {
    async fn start() -> Self {
        let harness = TestContext::new().await;
        let data = tempfile::Builder::new()
            .prefix("rimaia-board-contract-")
            .tempdir()
            .expect("a data directory");
        let paths = AppPaths::new(data.path());
        let config = RunnerConfig::default();
        // Runner A is the solo runner. Runner B is a second machine of the
        // same user, which `runs.runner_id` needs a row for (D31 point 13).
        let a = harness.board(&paths, &config);
        let second_runner = {
            let mut conn = harness.context.pool.acquire().await.expect("a connection");
            insert_runner(&mut conn, &harness.clock, &harness.solo.user_id, "Runner B").await
        };
        let b: Arc<dyn BoardPort> = Arc::new(InProcessBoard::new(
            harness.context.clone(),
            paths.clone(),
            config.provider.clone(),
            second_runner,
        ));

        Self {
            harness,
            _data: data,
            a,
            b,
        }
    }

    fn runner(&self, which: Which) -> Arc<dyn BoardPort> {
        match which {
            Which::A => Arc::clone(&self.a),
            Which::B => Arc::clone(&self.b),
        }
    }

    fn board(&self) -> &ServiceContext {
        &self.harness.context
    }

    fn clock(&self) -> &TestClock {
        &self.harness.clock
    }
}

rimaia_core::board_contract!(InProcess);

// ---------------------------------------------------------------------------
// The wire shape
// ---------------------------------------------------------------------------

fn round_trips<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("serialize");
    let back: T = serde_json::from_str(&json).expect("deserialize what was serialized");
    assert_eq!(&back, value, "{json}");
}

#[tokio::test]
async fn every_board_dto_round_trips_through_json() {
    // D31 point 6: every type crosses a network from task 052, so a value that
    // does not come back as itself is a bug found here rather than by a runner
    // on another machine. One value of each type in `board::types`, the ones
    // carrying core types read off a real board so every nested field is a
    // real one.
    let harness = TestContext::new().await;
    let data = TempDir::new().expect("a data directory");
    let paths = AppPaths::new(data.path());
    let board = harness.board(&paths, &RunnerConfig::default());
    let repository = TempRepo::init();
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
    .expect("register a repository");
    let task = tasks::create_task(
        &harness.context,
        NewTask {
            repository_id: registered.id,
            title: "Round trip".to_string(),
            plan: Some("1. Go and come back".to_string()),
            extra_instructions: None,
            column: Some(BoardColumn::Ready),
            links: vec![],
        },
    )
    .await
    .expect("create a task");

    let context: RunContext = board.preview(&task.id).await.expect("preview");
    let target = ClaimTarget::Run {
        task_id: task.id.clone(),
        trigger: RunTrigger::Manual,
        continue_session: false,
    };
    let claim: Claim = board
        .claim(target.clone())
        .await
        .expect("claim")
        .expect("unclaimed");
    let start = StartRun {
        run_id: "run-1".to_string(),
        kind: RunKind::Implementation,
        session_id: "session-1".to_string(),
        prompt: "do the work".to_string(),
        base_ref: Some("main".to_string()),
        base_sha: None,
    };
    board
        .start_run(&claim.lease, start.clone())
        .await
        .expect("start");
    let outcome = RunOutcome {
        exit_class: ExitClass::Success,
        status: RunStatus::Succeeded,
        error_message: None,
        num_turns: Some(3),
        cost_usd: Some(0.125),
        duration_ms: Some(900),
        pr_url: Some("https://github.com/acme/widgets/pull/7".to_string()),
        usage_limit_resets_at: None,
        resume_after: None,
        spawned_as: SpawnedAs {
            model: Some("claude-sonnet-5".to_string()),
            effort: None,
            run_environment: Some("inherit".to_string()),
        },
        usage: TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(20),
            cache_read_tokens: None,
            cache_creation_tokens: Some(5),
        },
    };
    let finish = FinishRun {
        outcome,
        head_sha: None,
        bundle: None::<ReviewBundle>,
        window_closes_at: Some(harness.clock.now() + TimeDelta::hours(4)),
        transcript: TranscriptEnd::Complete { length: 42 },
    };
    let receipt: FinishReceipt = board
        .finish_run(&claim.lease, "run-1", finish.clone())
        .await
        .expect("finish");

    round_trips(&LeaseRef::solo(
        task.id.clone(),
        harness.solo.team_id.clone(),
    ));
    round_trips(&LeasePurpose::Strategy);
    round_trips(&target);
    round_trips(&ClaimTarget::Plan { task_id: task.id });
    round_trips(&claim);
    round_trips(&Claim {
        resume: Some(ResumePoint {
            kind: RunKind::Implementation,
            session_id: "session-1".to_string(),
        }),
        ..claim.clone()
    });
    round_trips(&context);
    round_trips(&TeamLimits {
        max_turns: 300,
        disallowed_tools: Some(vec!["Bash(git push --force:*)".to_string()]),
    });
    round_trips(&Heartbeat {
        fenced: vec![LeaseRef::solo("fenced", harness.solo.team_id.clone())],
        cancel: vec!["cancelled".to_string()],
    });
    round_trips(&start);
    round_trips(&TranscriptChunk {
        run_id: "run-1".to_string(),
        offset: 7,
        bytes: b"{}\n".to_vec(),
    });
    round_trips(&TranscriptAck { stored_through: 10 });
    round_trips(&finish);
    round_trips(&TranscriptEnd::KeptOnRunner);
    round_trips(&receipt);
    round_trips(&NextStep::Released {
        resume_after: Some(harness.clock.now() + TimeDelta::minutes(5)),
    });
    for method in BoardMethod::ALL {
        round_trips(&method);
    }
}

#[test]
fn every_board_method_is_named_for_its_trait_method() {
    assert_eq!(
        BoardMethod::ALL.map(BoardMethod::as_str),
        [
            "preview",
            "claim",
            "heartbeat",
            "run_context",
            "record_branch",
            "start_run",
            "append_transcript",
            "publish_tail",
            "finish_run",
            "release",
            "record_strategy",
            "record_review_findings",
        ],
    );
}
