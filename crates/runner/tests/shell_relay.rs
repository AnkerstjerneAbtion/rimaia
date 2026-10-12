//! Solo's Run now and Cancel, through the board command and the shell's relay
//! (task 052).
//!
//! The desktop window dispatches `start_task_run` and `cancel_task_run` as
//! board commands now, through `Door::Shell`, and the host's relay is
//! `ShellRelay`: today's starter, run on this machine, answering with today's
//! sentences. These hold that path to what the commands did before it, with
//! the real starter, a real repository and a stand-in CLI.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pretty_assertions::assert_eq;
use rimaia_core::api::tail::SOLO_TAIL_RELAY_CAPACITY;
use rimaia_core::api::{self, BoardHost, Caller, StreamControl, TailRelay};
use rimaia_core::board::lease;
use rimaia_core::db::{BoardColumn, RunState};
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::provider::ProviderProfile;
use rimaia_core::runner::{claim_manual_start, ManualStart, RunnerConfig};
use rimaia_core::runs;
use rimaia_core::scheduler::InFlight;
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::AppPaths;
use rimaia_runner::relay::ShellRelay;
use serde_json::json;
use tempfile::TempDir;

/// The solo board, one opted-in repository with a `ready` task, and the host
/// the shell builds over them.
struct Solo {
    harness: TestContext,
    host: BoardHost,
    paths: AppPaths,
    config: RunnerConfig,
    in_flight: InFlight,
    task_id: String,
    _repository: TempRepo,
    _data: TempDir,
}

impl Solo {
    async fn new(program: PathBuf) -> Self {
        let harness = TestContext::new().await;
        let data = TempDir::new().expect("a data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app directories");
        let repository = TempRepo::init();
        let registered = repo::register(
            &harness.context,
            harness.machine(),
            &paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register the repository");
        repo::set_allow_unattended_runs(&harness.context, harness.machine(), &registered.id, true)
            .await
            .expect("this machine's opt-in");
        let task_id = tasks::create_task(
            &harness.context,
            NewTask {
                repository_id: registered.id,
                title: "Started by hand".to_string(),
                plan: Some("1. Do it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("a ready task")
        .id;

        let config = RunnerConfig {
            program,
            ..RunnerConfig::default()
        };
        let in_flight = InFlight::new();
        let host = BoardHost {
            context: harness.context.clone(),
            provider: ProviderProfile::of(config.provider.as_ref()),
            paths: paths.clone(),
            rate_limits: Arc::new(RateLimiter::new(harness.context.clock.clone())),
            tails: TailRelay::new(SOLO_TAIL_RELAY_CAPACITY),
            streams: StreamControl::new(),
            relay: Arc::new(ShellRelay {
                board: harness.board(&paths, &config),
                machine: harness.machine().clone(),
                paths: paths.clone(),
                config: config.clone(),
                in_flight: in_flight.clone(),
            }),
        };
        Self {
            harness,
            host,
            paths,
            config,
            in_flight,
            task_id,
            _repository: repository,
            _data: data,
        }
    }

    /// `name` dispatched as the solo window dispatches it: through the shell's
    /// door, with this machine.
    async fn dispatch(&self, name: &str) -> rimaia_core::Result<serde_json::Value> {
        api::dispatch_with_machine(
            &self.host,
            Caller::solo(&self.harness.solo),
            self.harness.machine(),
            name,
            json!({ "taskId": self.task_id }),
        )
        .await
    }
}

#[tokio::test]
async fn a_solo_start_reaches_the_starter_through_the_shell_relay() {
    // No CLI where the config points: the starter's own refusal, before any
    // run state is written (task 008).
    let missing = PathBuf::from("/nonexistent/rimaia-test/claude");
    let solo = Solo::new(missing).await;

    let through_the_relay = solo
        .dispatch("start_task_run")
        .await
        .expect_err("the CLI is missing");
    let straight_to_the_starter = claim_manual_start(
        solo.harness
            .starter(rimaia_core::board::OwnerPresence::AtRunner),
        solo.harness.board(&solo.paths, &solo.config).as_ref(),
        solo.harness.machine(),
        &solo.paths,
        &solo.config,
        &solo.in_flight,
        ManualStart {
            task_id: solo.task_id.clone(),
            continue_session: false,
        },
    )
    .await
    .err()
    .expect("the CLI is missing");

    assert_eq!(
        through_the_relay.to_string(),
        straight_to_the_starter.to_string()
    );
    assert_eq!(through_the_relay.code(), straight_to_the_starter.code());
    let task = tasks::get_task(&solo.harness.context, &solo.task_id)
        .await
        .expect("read the task");
    assert_eq!(task.task.run_state, RunState::Idle, "nothing was written");
}

/// A start held in its CLI probe by a stand-in that does not answer, stopped
/// by Cancel: the slot's cancel reaches it before its claim, so nothing is
/// claimed. FakeCli is a POSIX shell script.
#[cfg(unix)]
#[tokio::test]
async fn a_solo_cancel_stops_a_start_still_in_preflight() {
    let cli = rimaia_core::testing::FakeCli::new();
    // The stand-in holds `--version` only once `auth` has been asked, which
    // is the startup doctor's line; ask it as the doctor does.
    std::process::Command::new(cli.program())
        .args(["auth", "status", "--json"])
        .output()
        .expect("the stand-in answers auth");
    cli.hold_version_probe();
    let solo = Arc::new(Solo::new(cli.program()).await);

    let starting = tokio::spawn({
        let solo = Arc::clone(&solo);
        async move { solo.dispatch("start_task_run").await }
    });
    tokio::time::timeout(Duration::from_secs(30), async {
        while !cli.version_probe_is_held() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the start never reached its CLI probe");

    solo.dispatch("cancel_task_run")
        .await
        .expect("Cancel is answered while the start is held");
    cli.release_version_probe();

    let answer = tokio::time::timeout(Duration::from_secs(30), starting)
        .await
        .expect("the start ends")
        .expect("the start does not panic")
        .expect_err("a cancelled start is refused");
    assert_eq!(
        answer.to_string(),
        "this start was cancelled before its run began"
    );
    let ctx = &solo.harness.context;
    let task = tasks::get_task(ctx, &solo.task_id)
        .await
        .expect("read the task");
    assert_eq!(task.task.run_state, RunState::Idle, "nothing was claimed");
    assert_eq!(
        lease::state_of(ctx, &solo.task_id)
            .await
            .expect("read the lease")
            .lease,
        None
    );
    assert_eq!(
        runs::list_runs_for_task(ctx, &solo.task_id)
            .await
            .expect("read the runs"),
        vec![]
    );
    assert!(solo.in_flight.is_empty(), "the slot was given back");
}
