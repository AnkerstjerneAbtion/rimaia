//! A board that holds two teams, for the tests that prove neither can see the
//! other (task 039, ADR-0029 point 5).
//!
//! # Server-shaped, on purpose
//!
//! There is no `solo_identity` row. Both teams are made by
//! [`identity::create_personal_team`], the team-creation service a sign-up
//! uses, never by hand-written `INSERT`s into `users`, `teams` or
//! `team_memberships`: the fixture builds teams the way the product does, and
//! task 051 builds its server-shaped boards on it.
//!
//! # Team B is marked
//!
//! Every title, plan, repository name, path and text setting of team B carries
//! [`SENTINEL`], so a leak shows up in the same scan whether it is an id or a
//! word. Numeric and enum settings cannot carry it, so they differ between the
//! teams instead, and the tests that read them assert the exact value.
//!
//! # Nothing reaches a real binary or a real keychain
//!
//! [`TwoTeams::runner`] spawns [`FakeCli`], which replays recorded streams,
//! and its credential store is a [`MemoryStore`]. [`TwoTeams::doctor`] is
//! `testing::doctor::temp_environment()`, probing the same stand-in.
//!
//! Lives here rather than in a test binary so task 046's
//! `crates/server/tests/commands.rs` can reuse it (D32 point 5).

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use tempfile::TempDir;
use tokio::sync::broadcast::Receiver;

use crate::board::{BoardPort, InProcessBoard};
use crate::clock::Clock;
use crate::context::{ServiceContext, TeamScope};
use crate::credentials::CredentialAccess;
use crate::db::settings::{self, placement, Placement, ALL_KEYS};
use crate::db::{BoardColumn, MutationSource, Repository, RunKind};
use crate::doctor;
use crate::events::{ChangeEvent, RunId, RunnerId, TaskId, TeamId, UserId};
use crate::identity::create_personal_team;
use crate::machine::MachineContext;
use crate::mcp::{LocalTools, RunHandles};
use crate::paths::AppPaths;
use crate::repo::{self, NewRepository};
use crate::review::{FindingSeverity, FindingStatus};
use crate::runner::strategy::PlannerAccess;
use crate::runner::RunnerConfig;
use crate::scheduler::InFlight;
use crate::strategy::settings::repository_default_key;
use crate::tasks::{self, NewTask, NewTaskLink};
use crate::testing::credentials::MemoryStore;
use crate::testing::machine::MemoryMachine;
use crate::testing::runs::{seed_finding, SeededFinding};
use crate::testing::{test_epoch, test_pool, FakeCli, TempRepo, TestClock};

/// What every text of team B contains, so a scan for it finds a leak.
pub const SENTINEL: &str = "team-b-sentinel";

/// One team's share of the board: its owner, its repository, and something in
/// every table a board service reads.
pub struct TeamBoard {
    pub team_id: TeamId,
    /// The owner [`create_personal_team`] made. Team A's owner is not a member
    /// of team B, and no membership is written by hand.
    pub owner_id: UserId,
    /// A runner of the owner's, which the team's runs name.
    pub runner_id: RunnerId,
    pub repository: Repository,
    pub not_ready: TaskId,
    pub ready: TaskId,
    /// Carries the runs, the bundle and the findings, a worktree path that is
    /// missing on disk, and the dependency edge (it depends on [`done`]).
    ///
    /// [`done`]: Self::done
    pub in_review: TaskId,
    pub done: TaskId,
    pub archived: TaskId,
    /// A link on [`ready`](Self::ready).
    pub link: String,
    pub implementation_run: RunId,
    pub review_run: RunId,
    pub fix_run: RunId,
    /// On the review run: one open, one fixed by the fix run.
    pub findings: Vec<String>,
    /// Each team setting as this team stored it, keyed by key.
    pub settings: Vec<(String, String)>,
    /// What the owner pays a month, as stored in `user_settings`.
    pub subscription_monthly_usd: f64,
    /// Every run's `cost_usd`, in the order the runs were written.
    pub run_costs: [f64; 3],
    _source: TempRepo,
    _worktrees: TempDir,
}

impl TeamBoard {
    /// Every id that names a row of this team, for a scan of an answer.
    pub fn ids(&self) -> Vec<String> {
        let mut ids = vec![
            self.team_id.clone(),
            self.owner_id.clone(),
            self.runner_id.clone(),
            self.repository.id.clone(),
            self.not_ready.clone(),
            self.ready.clone(),
            self.in_review.clone(),
            self.done.clone(),
            self.archived.clone(),
            self.link.clone(),
            self.implementation_run.clone(),
            self.review_run.clone(),
            self.fix_run.clone(),
        ];
        ids.extend(self.findings.iter().cloned());
        ids
    }

    /// The value this team stored for a team key.
    pub fn setting(&self, key: &str) -> &str {
        self.settings
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("the fixture sets every team key, not {key}"))
    }
}

/// Two teams on one board, and three contexts onto it.
pub struct TwoTeams {
    /// Scoped to team A, acting as A's owner.
    pub a: ServiceContext,
    /// Scoped to team B, acting as B's owner.
    pub b: ServiceContext,
    /// Scoped to both teams, acting as **A's owner**: the context the edges of
    /// tasks 046 and 047 would build for a user in two teams. 039's services
    /// check the scope, not membership.
    pub both: ServiceContext,
    /// Subscribed before anything else could publish, on the one channel all
    /// three contexts share.
    pub changes: Receiver<ChangeEvent>,
    pub clock: TestClock,
    pub team_a: TeamBoard,
    pub team_b: TeamBoard,
    /// The stand-in `claude` every spawn reaches.
    pub cli: FakeCli,
    /// The shared handle table the MCP server and the runner mint from.
    pub handles: RunHandles,
    /// Points at [`cli`](Self::cli), with an in-memory credential store.
    pub runner: RunnerConfig,
    pub in_flight: InFlight,
    pub paths: AppPaths,
    /// The one machine both teams' work runs on, over a `MemoryMachine`,
    /// announcing on the shared channel under team A (task 041).
    pub machine: MachineContext,
    _data: TempDir,
    doctor: doctor::Environment,
    _doctor_root: TempDir,
}

impl TwoTeams {
    pub async fn new() -> Self {
        let clock = TestClock::new(test_epoch());
        let pool = test_pool().await;

        let (personal_a, personal_b) = {
            let mut tx = pool.begin().await.expect("a transaction");
            let a = create_personal_team(&mut tx, &clock, "ada")
                .await
                .expect("team A");
            let b = create_personal_team(&mut tx, &clock, "grace")
                .await
                .expect("team B");
            tx.commit().await.expect("commit the two teams");
            (a, b)
        };

        let a = ServiceContext::new(
            pool,
            Arc::new(clock.clone()),
            MutationSource::Ui,
            TeamScope::one(personal_a.team_id.clone()),
            personal_a.user_id.clone(),
        );
        let changes = a.subscribe();
        let b = ServiceContext {
            scope: TeamScope::one(personal_b.team_id.clone()),
            actor: personal_b.user_id.clone(),
            ..a.clone()
        };
        let both = a.with_scope(
            TeamScope::of([personal_a.team_id.clone(), personal_b.team_id.clone()])
                .expect("two teams"),
        );

        let data = tempfile::Builder::new()
            .prefix("rimaia-two-teams-")
            .tempdir()
            .expect("a data directory");
        let paths = AppPaths::new(PathBuf::from(data.path()));
        paths.create_all().expect("the app directories");

        let team_a = arrange(&a, &clock, &paths, Marking::Plain).await;
        let team_b = arrange(&b, &clock, &paths, Marking::Sentinel).await;

        let cli = FakeCli::new();
        let handles = RunHandles::default();
        let runner = RunnerConfig {
            program: cli.program(),
            run_handles: handles.clone(),
            credentials: CredentialAccess::new(MemoryStore::new()),
            ..RunnerConfig::default()
        };
        // A real, writable directory for the doctor to report on, probing the
        // stand-in rather than whatever `claude` is on this machine's PATH.
        let (doctor_root, mut doctor) = crate::testing::doctor::temp_environment();
        doctor.programs.agent = runner.program.clone();
        doctor.run_handles = handles.clone();
        let machine = MachineContext {
            store: Arc::new(MemoryMachine::new()),
            clock: Arc::new(clock.clone()),
            changes: a.changes.clone(),
            event_team: personal_a.team_id.clone(),
        };

        Self {
            a,
            b,
            both,
            changes,
            clock,
            team_a,
            team_b,
            cli,
            handles,
            runner,
            in_flight: InFlight::new(),
            paths,
            machine,
            _data: data,
            doctor,
            _doctor_root: doctor_root,
        }
    }

    /// A fresh id, in the shape every id has, naming nothing.
    pub fn never_issued() -> String {
        crate::db::new_id()
    }

    /// The board port over `ctx`, serving team A's runner.
    pub fn board(&self, ctx: &ServiceContext) -> Arc<dyn BoardPort> {
        Arc::new(InProcessBoard::new(
            ctx.clone(),
            self.paths.clone(),
            self.runner.provider.clone(),
            self.team_a.runner_id.clone(),
        ))
    }

    /// What `plan_task_strategy` and `plan_tasks_strategy` spawn through,
    /// over `ctx`'s board.
    pub fn planner(&self, ctx: &ServiceContext) -> PlannerAccess {
        PlannerAccess {
            paths: self.paths.clone(),
            runner: self.runner.clone(),
            in_flight: self.in_flight.clone(),
            board: self.board(ctx),
        }
    }

    /// This machine's local MCP tools for a server over `ctx`: the fixture's
    /// machine, [`doctor`](Self::doctor) and [`planner`](Self::planner).
    pub fn local(&self, ctx: &ServiceContext) -> LocalTools {
        LocalTools {
            machine: self.machine.clone(),
            doctor: self.doctor(),
            planner: self.planner(ctx),
        }
    }

    /// What `run_doctor` reports on: [`temp_environment`]'s real directory,
    /// probing the stand-in and the shared handle table.
    ///
    /// [`temp_environment`]: crate::testing::doctor::temp_environment
    pub fn doctor(&self) -> doctor::Environment {
        self.doctor.clone()
    }

    /// Every row of team B's, in every table a board service reads or writes,
    /// as text: equal before and after a call exactly when the call changed
    /// nothing of B's.
    ///
    /// `tasks`, `repositories`, `runs`, `task_links`, `task_dependencies`,
    /// `review_bundles`, `review_findings`, B's `team_settings` and B's
    /// owner's `user_settings`.
    pub async fn snapshot_b(&self) -> String {
        self.snapshot(&self.team_b).await
    }

    /// [`snapshot_b`](Self::snapshot_b), for either team.
    pub async fn snapshot(&self, team: &TeamBoard) -> String {
        let owner = &team.owner_id;
        let team = &team.team_id;
        let tables: [(&str, &str, &str); 9] = [
            ("tasks", "t", "t.team_id = ?1"),
            ("repositories", "t", "t.team_id = ?1"),
            (
                "runs",
                "t",
                "t.task_id IN (SELECT id FROM tasks WHERE team_id = ?1)",
            ),
            (
                "task_links",
                "t",
                "t.task_id IN (SELECT id FROM tasks WHERE team_id = ?1)",
            ),
            (
                "task_dependencies",
                "t",
                "t.task_id IN (SELECT id FROM tasks WHERE team_id = ?1)
                 OR t.depends_on_task_id IN (SELECT id FROM tasks WHERE team_id = ?1)",
            ),
            (
                "review_bundles",
                "t",
                "t.run_id IN (SELECT r.id FROM runs r JOIN tasks k ON k.id = r.task_id
                               WHERE k.team_id = ?1)",
            ),
            (
                "review_findings",
                "t",
                "t.task_id IN (SELECT id FROM tasks WHERE team_id = ?1)",
            ),
            ("team_settings", "t", "t.team_id = ?1"),
            ("user_settings", "t", "t.user_id = ?2"),
        ];

        let mut snapshot = String::new();
        for (table, alias, predicate) in tables {
            let columns: Vec<String> =
                sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                    .fetch_all(&self.a.pool)
                    .await
                    .expect("read a table's columns");
            let row = columns
                .iter()
                .map(|column| format!("quote({alias}.{column})"))
                .collect::<Vec<_>>()
                .join(" || '|' || ");
            let rows: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT {row} FROM {table} {alias} WHERE {predicate} ORDER BY 1"
            ))
            .bind(team)
            .bind(owner)
            .fetch_all(&self.a.pool)
            .await
            .expect("snapshot a table");
            snapshot.push_str(&format!("{table}:\n{}\n", rows.join("\n")));
        }
        snapshot
    }

    /// Every column of one row, as text, read past every service: equal before
    /// and after a call exactly when the call left the row byte-identical.
    pub async fn row_text(&self, table: &str, id: &str) -> String {
        let columns: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .fetch_all(&self.a.pool)
                .await
                .expect("read a table's columns");
        let row = columns
            .iter()
            .map(|column| format!("quote({column})"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        sqlx::query_scalar(&format!("SELECT {row} FROM {table} WHERE id = ?1"))
            .bind(id)
            .fetch_one(&self.a.pool)
            .await
            .expect("read a row")
    }

    /// The `user_settings` value `user_id` stored for `key`, read past every
    /// accessor.
    pub async fn user_setting(&self, user_id: &str, key: &str) -> Option<String> {
        sqlx::query_scalar("SELECT value FROM user_settings WHERE user_id = ?1 AND key = ?2")
            .bind(user_id)
            .bind(key)
            .fetch_optional(&self.a.pool)
            .await
            .expect("read a user setting")
    }

    /// The legacy `settings` value for `key`, read past every accessor.
    pub async fn legacy_setting(&self, key: &str) -> Option<String> {
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.a.pool)
            .await
            .expect("read a legacy setting")
    }

    /// How many `runs` rows the board holds, every team's.
    pub async fn run_count(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM runs")
            .fetch_one(&self.a.pool)
            .await
            .expect("count the runs")
    }

    /// Every change event published since the last call, in order.
    pub fn drain_changes(&mut self) -> Vec<ChangeEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.changes.try_recv() {
            events.push(event);
        }
        events
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Marking {
    Plain,
    Sentinel,
}

impl Marking {
    /// `text`, marked with [`SENTINEL`] for team B.
    fn text(self, text: &str) -> String {
        match self {
            Marking::Plain => format!("team-a {text}"),
            Marking::Sentinel => format!("{SENTINEL} {text}"),
        }
    }

    /// Team A's value or team B's, for a setting that cannot carry the
    /// sentinel.
    fn pick<'a>(self, a: &'a str, b: &'a str) -> &'a str {
        match self {
            Marking::Plain => a,
            Marking::Sentinel => b,
        }
    }
}

/// Fills one team's share of the board through its own context, through the
/// services wherever one exists.
async fn arrange(
    ctx: &ServiceContext,
    clock: &TestClock,
    paths: &AppPaths,
    marking: Marking,
) -> TeamBoard {
    let team_id = ctx.scope.sole().expect("one team").clone();
    let owner_id = ctx.actor.clone();
    let runner_id = {
        let mut conn = ctx.pool.acquire().await.expect("a connection");
        crate::testing::db::insert_runner(&mut conn, clock, &owner_id, &marking.text("runner"))
            .await
    };

    let source = TempRepo::init_with_prefix(match marking {
        Marking::Plain => "rimaia-team-a-",
        Marking::Sentinel => "rimaia-team-b-sentinel-",
    });
    let worktrees = tempfile::Builder::new()
        .prefix(match marking {
            Marking::Plain => "rimaia-team-a-worktrees-",
            Marking::Sentinel => "rimaia-team-b-sentinel-worktrees-",
        })
        .tempdir()
        .expect("a worktrees directory");
    let registered = repo::register(
        ctx,
        worktrees.path(),
        NewRepository {
            path: source.path().to_string_lossy().into_owned(),
            name: Some(marking.text("repository")),
            worktree_root: None,
        },
    )
    .await
    .expect("register the team's repository");
    let repository = repo::set_allow_unattended_runs(ctx, &registered.id, true)
        .await
        .expect("opt the repository in");

    let task = |title: &str, column: BoardColumn| {
        let repository_id = repository.id.clone();
        let title = marking.text(title);
        let plan = marking.text("plan: do the work");
        async move {
            tasks::create_task(
                ctx,
                NewTask {
                    repository_id,
                    title,
                    plan: Some(plan),
                    extra_instructions: None,
                    column: Some(column),
                    links: vec![],
                },
            )
            .await
            .expect("create a task")
            .id
        }
    };
    let not_ready = task("not ready", BoardColumn::NotReady).await;
    let ready = task("ready", BoardColumn::Ready).await;
    let in_review = task("in review", BoardColumn::InReview).await;
    let done = task("done", BoardColumn::Done).await;
    let archived = task("archived", BoardColumn::Done).await;
    tasks::archive_task(ctx, None, &archived)
        .await
        .expect("archive a task");

    tasks::set_task_dependencies(ctx, &in_review, std::slice::from_ref(&done))
        .await
        .expect("a dependency edge");
    let link = tasks::add_task_link(
        ctx,
        &ready,
        NewTaskLink {
            label: marking.text("link"),
            url: format!("https://example.invalid/{}", marking.text("issue")),
        },
    )
    .await
    .expect("a link")
    .id;

    // A worktree the board records and the disk does not hold, for the
    // startup survey.
    let missing_worktree = paths
        .worktrees_dir()
        .join(marking.text("missing-worktree").replace(' ', "-"));
    sqlx::query("UPDATE tasks SET worktree_path = ?1, branch = ?2 WHERE id = ?3")
        .bind(missing_worktree.to_string_lossy().into_owned())
        .bind(marking.text("branch").replace(' ', "-"))
        .bind(&in_review)
        .execute(&ctx.pool)
        .await
        .expect("record a worktree path");

    let run_costs = match marking {
        Marking::Plain => [1.5, 0.5, 2.5],
        Marking::Sentinel => [20.0, 10.0, 30.0],
    };
    let kinds = [RunKind::Implementation, RunKind::Review, RunKind::Fix];
    let mut runs = Vec::new();
    for (index, (kind, cost)) in kinds.into_iter().zip(run_costs).enumerate() {
        let started_at = clock.now() - Duration::hours(3 - index as i64);
        runs.push(
            insert_run(
                ctx,
                paths,
                RunRow {
                    task_id: &in_review,
                    runner_id: &runner_id,
                    attempt: index as i64 + 1,
                    kind,
                    cost_usd: cost,
                    started_at,
                    marking,
                },
            )
            .await,
        );
    }
    let [implementation_run, review_run, fix_run]: [RunId; 3] =
        runs.try_into().expect("three runs");

    insert_bundle(ctx, &implementation_run, marking).await;
    let findings = vec![
        seed_finding(
            ctx,
            &in_review,
            SeededFinding {
                review_run_id: &review_run,
                ordinal: 0,
                severity: FindingSeverity::High,
                title: &marking.text("an open finding"),
                status: FindingStatus::Open,
                resolution: None,
                resolved_by_run_id: None,
            },
        )
        .await,
        seed_finding(
            ctx,
            &in_review,
            SeededFinding {
                review_run_id: &review_run,
                ordinal: 1,
                severity: FindingSeverity::Low,
                title: &marking.text("a fixed finding"),
                status: FindingStatus::Fixed,
                resolution: Some(&marking.text("fixed it")),
                resolved_by_run_id: Some(&fix_run),
            },
        )
        .await,
    ];

    let settings = team_settings(&repository.id, marking);
    for (key, value) in &settings {
        settings::set_team(ctx, &team_id, key, Some(value))
            .await
            .expect("store a team setting");
    }
    let subscription_monthly_usd = match marking {
        Marking::Plain => 20.0,
        Marking::Sentinel => 200.0,
    };
    settings::set_user(
        ctx,
        settings::SUBSCRIPTION_MONTHLY_USD,
        &subscription_monthly_usd.to_string(),
    )
    .await
    .expect("store the owner's subscription");

    TeamBoard {
        team_id,
        owner_id,
        runner_id,
        repository,
        not_ready,
        ready,
        in_review,
        done,
        archived,
        link,
        implementation_run,
        review_run,
        fix_run,
        findings,
        settings,
        subscription_monthly_usd,
        run_costs,
        _source: source,
        _worktrees: worktrees,
    }
}

/// Every team key, with this team's value. A team key added later and not
/// given a value here fails the fixture, which is the point: every key has to
/// differ between the teams.
fn team_settings(repository_id: &str, marking: Marking) -> Vec<(String, String)> {
    let per_repository = repository_default_key(repository_id);
    ALL_KEYS
        .iter()
        .copied()
        .filter(|key| placement(key) == Placement::Team)
        .chain(std::iter::once(per_repository.as_str()))
        .map(|key| {
            let value = match key {
                settings::BASE_INSTRUCTIONS => marking.text("base instructions: open a draft PR."),
                crate::strategy::catalogue::STRATEGY_CATALOGUE => format!(
                    r#"{{"models":[{{"id":"{id}","label":"{label}"}}]}}"#,
                    id = marking.text("model").replace(' ', "-"),
                    label = marking.text("model"),
                ),
                crate::strategy::settings::STRATEGY_DEFAULT => format!(
                    r#"{{"mode":"default","model":"{}"}}"#,
                    marking.text("model").replace(' ', "-")
                ),
                crate::strategy::settings::STRATEGY_APPROVAL => {
                    marking.pick("automatic", "manual").to_string()
                }
                crate::runner::process::MAX_TURNS => marking.pick("41", "57").to_string(),
                crate::runner::process::DISALLOWED_TOOLS => {
                    format!("Bash({}:*)", marking.text("tool").replace(' ', "-"))
                }
                crate::review_loop::config::REVIEW_INSTRUCTIONS => {
                    marking.text("review instructions")
                }
                crate::review_loop::config::REVIEW_CONFIG => match marking {
                    Marking::Plain => r#"{"max_review_loops":1}"#.to_string(),
                    Marking::Sentinel => {
                        format!(r#"{{"max_review_loops":3,"review_model":"{SENTINEL}-model"}}"#)
                    }
                },
                key if key == per_repository => format!(
                    r#"{{"mode":"default","effort":"{}"}}"#,
                    marking.text("effort").replace(' ', "-")
                ),
                other => panic!("the two-team fixture has no value for the team key {other}"),
            };
            (key.to_string(), value)
        })
        .collect()
}

struct RunRow<'a> {
    task_id: &'a str,
    runner_id: &'a str,
    attempt: i64,
    kind: RunKind,
    cost_usd: f64,
    started_at: DateTime<Utc>,
    marking: Marking,
}

/// A closed run with every capture column set. Its transcript is never
/// written, so the startup survey finds it missing.
async fn insert_run(ctx: &ServiceContext, paths: &AppPaths, row: RunRow<'_>) -> RunId {
    let id = crate::db::new_id();
    let log_path = crate::runner::events::transcript_path(paths, row.task_id, &id);
    let ended_at = row.started_at + Duration::minutes(10);
    let recorded = (row.kind == RunKind::Review).then_some(ended_at);
    let pr_url = (row.kind == RunKind::Implementation).then(|| {
        format!(
            "https://example.invalid/{}/pull/1",
            row.marking.text("pr").replace(' ', "-")
        )
    });
    sqlx::query(
        "INSERT INTO runs
            (id, task_id, attempt, kind, status, session_id, prompt, started_at, ended_at,
             exit_class, num_turns, cost_usd, log_path, pr_url, base_ref, model, effort,
             run_environment, input_tokens, output_tokens, cache_read_tokens,
             cache_creation_tokens, head_sha, base_sha, findings_recorded_at, runner_id)
         VALUES (?1, ?2, ?3, ?4, 'succeeded', ?1, ?5, ?6, ?7, 'success', 12, ?8, ?9, ?10,
                 'main', ?11, 'high', 'inherit', 1000, 200, 300, 400,
                 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', ?12, ?13)",
    )
    .bind(&id)
    .bind(row.task_id)
    .bind(row.attempt)
    .bind(row.kind)
    .bind(row.marking.text("prompt"))
    .bind(row.started_at)
    .bind(ended_at)
    .bind(row.cost_usd)
    .bind(log_path.to_string_lossy().into_owned())
    .bind(pr_url)
    .bind(row.marking.text("model").replace(' ', "-"))
    .bind(recorded)
    .bind(row.runner_id)
    .execute(&ctx.pool)
    .await
    .expect("insert a run");
    id
}

/// The review bundle an implementation run's finish would have written.
async fn insert_bundle(ctx: &ServiceContext, run_id: &str, marking: Marking) {
    let patch = format!("+{}\n", marking.text("patch line"));
    sqlx::query(
        "INSERT INTO review_bundles
            (run_id, files_changed, insertions, deletions, files, commits, patch, patch_bytes,
             patch_truncated, created_at)
         VALUES (?1, 1, 1, 0, '[]', '[]', ?2, ?3, 0, ?4)",
    )
    .bind(run_id)
    .bind(&patch)
    .bind(patch.len() as i64)
    .bind(ctx.clock.now())
    .execute(&ctx.pool)
    .await
    .expect("insert a review bundle");
}
