//! Run now, Retry now and Cancel on a server (task 052, ADR-0031 point 7).
//!
//! A person presses a button through a board command, over HTTP with a
//! desktop token; the server holds the request in its `RelayedRequests`; the
//! runner collects it through the runner protocol with its own token: a Run
//! now on its next `claim(Next)`, a Cancel on its heartbeats. Every case here
//! drives both doors over the real router, on the injected clock, and sleeps
//! nowhere.

mod common;

use std::sync::Arc;

use chrono::Duration;
use pretty_assertions::assert_eq;
use rimaia_core::api::tail::SOLO_TAIL_RELAY_CAPACITY;
use rimaia_core::api::{BoardHost, StreamControl, TailRelay, PROTOCOL_VERSION};
use rimaia_core::board::{
    authorize_start, lease, runners as board_runners, ClaimTarget, OwnerPresence, RelayedRequests,
};
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::identity::secret::Secret;
use rimaia_core::identity::{pairing, runners, tokens};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderProfile};
use rimaia_core::runner::RunTrigger;
use rimaia_core::tasks::{self, Patch, TaskPatch};
use rimaia_core::testing::api as testing_api;
use rimaia_core::testing::shared::{Member, SharedTeam};
use rimaia_core::testing::teams::TwoTeams;
use rimaia_core::AppPaths;
use serde_json::{json, Value};

use common::{state_over, Answer, TestServer};

/// The two doors' calls, over one server.
struct Doors {
    server: TestServer,
    relayed: Arc<RelayedRequests>,
}

impl Doors {
    /// A board command, as a person's connected desktop sends it.
    async fn command(&self, desktop: &Secret, name: &str, args: Value) -> Answer {
        self.server.command(name, desktop.expose(), &args).await
    }

    /// A runner method, as `runner`'s own token sends it.
    async fn runner(&self, runner: &Secret, method: &str, body: Value) -> Answer {
        Answer::of(
            self.server
                .post(
                    &format!("runner/{method}"),
                    Some(runner.expose()),
                    Some(PROTOCOL_VERSION),
                    body.to_string(),
                )
                .await,
        )
        .await
    }

    /// `runner`'s `claim(Next)` with no free capacity: what a busy runner's
    /// loop sends, which only a relayed request can answer.
    async fn next_when_full(&self, runner: &Secret, repository: &str) -> Value {
        self.next(runner, repository, 0).await
    }

    async fn next(&self, runner: &Secret, repository: &str, free: usize) -> Value {
        let answer = self
            .runner(
                runner,
                "claim",
                json!({
                    "target": { "next": {
                        "capacity": { "total": free, "perRepository": { repository: free } },
                        "repositories": [repository],
                        "wait": 0,
                    } },
                    "provider": "claude-code",
                }),
            )
            .await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        answer.body
    }

    async fn heartbeat(&self, runner: &Secret, held: &[&Value]) -> Value {
        let answer = self
            .runner(runner, "heartbeat", json!({ "held": held }))
            .await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        answer.body
    }
}

fn claimed_task(claim: &Value) -> Option<&str> {
    claim["lease"]["taskId"].as_str()
}

fn run_now(task_id: &str, runner_id: &str) -> Value {
    json!({ "taskId": task_id, "runnerId": runner_id })
}

/// A two-team board: team A's owner presses through their desktop token, on
/// their paired runner `A` and a second machine `B` of theirs.
struct Fixture {
    teams: TwoTeams,
    doors: Doors,
    desktop: Secret,
    a: Secret,
    a_id: String,
    b: Secret,
    b_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let teams = TwoTeams::new().await;
        let second = pairing::pair_own_runner(&teams.a, "Mac mini", "claude-code")
            .await
            .expect("a second machine");
        let (host, relayed) = testing_api::relayed_host(&teams).await;
        let auth = SessionsAndTokens {
            pool: teams.a.pool.clone(),
            clock: Arc::new(teams.clock.clone()),
        };
        let server = TestServer::spawn(state_over(host, relayed.clone(), auth)).await;
        let credentials = &teams.team_a.credentials;
        Self {
            desktop: credentials.desktop_token.clone(),
            a: credentials.runner_token.clone(),
            a_id: credentials.paired_runner_id.clone(),
            b: second.token,
            b_id: second.runner_id,
            doors: Doors { server, relayed },
            teams,
        }
    }

    fn repository(&self) -> &str {
        &self.teams.team_a.repository.id
    }

    async fn press(&self, task_id: &str, runner_id: &str) -> Answer {
        self.doors
            .command(&self.desktop, "start_task_run", run_now(task_id, runner_id))
            .await
    }

    /// Team A's `ready` task, claimed by `A` by name: its lease.
    async fn claim_ready(&self) -> Value {
        let answer = self
            .doors
            .runner(
                &self.a,
                "claim",
                json!({
                    "target": { "run": { "taskId": self.teams.team_a.ready,
                                         "trigger": "queued", "continueSession": false } },
                    "provider": "claude-code",
                }),
            )
            .await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        answer.body["lease"].clone()
    }

    /// Team A's `ready` task, run once by `A` and left waiting to retry, under
    /// session `session`: pinned to `A`.
    async fn waiting_to_retry(&self, session: &str) {
        let lease = self.claim_ready().await;
        let started = self
            .doors
            .runner(
                &self.a,
                "start_run",
                json!({ "lease": lease, "run": {
                    "runId": "first-attempt", "kind": "implementation", "sessionId": session,
                    "prompt": "do it", "baseRef": null, "baseSha": null,
                } }),
            )
            .await;
        assert_eq!(started.status, 200, "{}", started.body);
        let finished = self
            .doors
            .runner(
                &self.a,
                "finish_run",
                json!({ "lease": lease, "runId": "first-attempt",
                        "finish": finish(json!({ "exitClass": "transient", "status": "failed",
                                                 "errorMessage": "the API was overloaded" })) }),
            )
            .await;
        assert_eq!(finished.status, 200, "{}", finished.body);
        let detail = tasks::get_task(&self.teams.a, &self.teams.team_a.ready)
            .await
            .expect("read the task");
        assert_eq!(
            serde_json::to_value(detail.task.run_state).expect("a run state"),
            json!("waiting_retry"),
            "the fixture's run is waiting to retry"
        );
    }
}

/// A finish, its outcome overlaid with `outcome`'s fields.
fn finish(outcome: Value) -> Value {
    let mut base = json!({
        "exitClass": "success", "status": "succeeded", "errorMessage": null,
        "numTurns": 1, "costUsd": null, "durationMs": null, "prUrl": null,
        "usageLimitResetsAt": null, "resumeAfter": null,
        "spawnedAs": { "model": null, "effort": null, "runEnvironment": null },
        "usage": {},
    });
    for (key, value) in outcome.as_object().expect("an object") {
        base[key] = value.clone();
    }
    json!({
        "outcome": base, "headSha": null, "bundle": null, "windowClosesAt": null,
        "transcript": { "complete": { "length": 0 } },
    })
}

// ---------------------------------------------------------------------------
// Run now
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_runner_is_online_for_one_lease_lifetime_after_its_last_request() {
    let f = Fixture::new().await;
    let ready = &f.teams.team_a.ready;

    f.teams.clock.advance(Duration::minutes(2));
    f.doors.heartbeat(&f.a, &[]).await;

    f.teams.clock.advance(Duration::minutes(2));
    let accepted = f.press(ready, &f.a_id).await;
    assert_eq!(accepted.status, 200, "{}", accepted.body);

    f.teams.clock.advance(Duration::seconds(61));
    let refused = f.press(ready, &f.a_id).await;
    assert_eq!((refused.status, refused.code()), (400, Some("invalid")));
    assert_eq!(refused.body["message"], json!("this runner has not been in touch for more than three minutes, so it cannot start a run now"));
}

#[tokio::test]
async fn a_run_now_request_is_collected_by_that_runners_next_claim_and_by_no_other() {
    let f = Fixture::new().await;
    let ready = f.teams.team_a.ready.clone();
    assert_eq!(f.press(&ready, &f.a_id).await.status, 200);

    let by_b = f.doors.next_when_full(&f.b, f.repository()).await;
    let by_a = f.doors.next_when_full(&f.a, f.repository()).await;

    assert_eq!(by_b, Value::Null, "B's claim is not handed A's request");
    assert_eq!(claimed_task(&by_a), Some(ready.as_str()));
}

#[tokio::test]
async fn a_relayed_run_now_is_claimed_unattended() {
    let f = Fixture::new().await;
    assert_eq!(f.press(&f.teams.team_a.ready, &f.a_id).await.status, 200);

    let claim = f.doors.next_when_full(&f.a, f.repository()).await;

    assert_eq!(claim["trigger"], json!("queued"));
}

#[tokio::test]
async fn a_runner_claiming_its_own_task_by_hand_is_an_interactive_run() {
    let f = Fixture::new().await;

    let answer = f
        .doors
        .runner(
            &f.a,
            "claim",
            json!({
                "target": { "run": { "taskId": f.teams.team_a.ready, "trigger": "manual",
                                     "continueSession": false } },
                "provider": "claude-code",
            }),
        )
        .await;

    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.body["trigger"], json!("manual"));
}

#[tokio::test]
async fn a_collected_run_now_ignores_the_runners_capacity() {
    let f = Fixture::new().await;

    let nothing_held = f.doors.next_when_full(&f.a, f.repository()).await;
    assert_eq!(
        nothing_held,
        Value::Null,
        "a full runner is never handed a queue pick, though a task is ready"
    );

    assert_eq!(f.press(&f.teams.team_a.ready, &f.a_id).await.status, 200);
    let held = f.doors.next_when_full(&f.a, f.repository()).await;
    assert_eq!(claimed_task(&held), Some(f.teams.team_a.ready.as_str()));
}

#[tokio::test]
async fn pressing_run_now_twice_records_one_request() {
    let f = Fixture::new().await;
    let ready = f.teams.team_a.ready.clone();

    assert_eq!(f.press(&ready, &f.a_id).await.status, 200);
    assert_eq!(f.press(&ready, &f.a_id).await.status, 200);

    assert_eq!(f.doors.relayed.held(), vec![(ready, f.a_id.clone())]);
}

#[tokio::test]
async fn a_run_now_nobody_collected_is_dropped_after_one_lease_lifetime() {
    let f = Fixture::new().await;
    assert_eq!(f.press(&f.teams.team_a.ready, &f.a_id).await.status, 200);

    f.teams.clock.advance(Duration::seconds(3 * 60 + 1));
    let claim = f.doors.next_when_full(&f.a, f.repository()).await;

    assert_eq!(claim, Value::Null);
    assert_eq!(f.doors.relayed.held(), vec![]);
}

#[tokio::test]
async fn retry_now_through_a_relay_resumes_the_session() {
    let f = Fixture::new().await;
    f.waiting_to_retry("the-first-session").await;

    let pressed = f
        .doors
        .command(
            &f.desktop,
            "retry_task_now",
            run_now(&f.teams.team_a.ready, &f.a_id),
        )
        .await;
    assert_eq!(pressed.status, 200, "{}", pressed.body);
    let claim = f.doors.next_when_full(&f.a, f.repository()).await;

    assert_eq!(claimed_task(&claim), Some(f.teams.team_a.ready.as_str()));
    assert_eq!(
        claim["resume"],
        json!({ "kind": "implementation", "sessionId": "the-first-session" })
    );
}

#[tokio::test]
async fn retry_without_a_runner_goes_to_the_pinned_runner() {
    let f = Fixture::new().await;
    f.waiting_to_retry("a-session").await;

    let pressed = f
        .doors
        .command(
            &f.desktop,
            "retry_task_now",
            json!({ "taskId": f.teams.team_a.ready }),
        )
        .await;
    assert_eq!(pressed.status, 200, "{}", pressed.body);
    assert_eq!(
        f.doors.relayed.held(),
        vec![(f.teams.team_a.ready.clone(), f.a_id.clone())]
    );

    // Without a pin there is no runner to default to.
    let unpinned = f
        .doors
        .command(
            &f.desktop,
            "retry_task_now",
            json!({ "taskId": f.teams.team_a.not_ready }),
        )
        .await;
    assert_eq!((unpinned.status, unpinned.code()), (400, Some("invalid")));
    assert_eq!(
        unpinned.body["message"],
        json!("name the runner to start this task on")
    );
}

// ---------------------------------------------------------------------------
// The sentences
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_now_that_names_no_runner_is_refused_over_http() {
    let f = Fixture::new().await;

    let refused = f
        .doors
        .command(
            &f.desktop,
            "start_task_run",
            json!({ "taskId": f.teams.team_a.ready }),
        )
        .await;

    assert_eq!((refused.status, refused.code()), (400, Some("invalid")));
    assert_eq!(
        refused.body["message"],
        json!("name the runner to start this task on")
    );
    assert_eq!(f.doors.relayed.held(), vec![]);
}

#[tokio::test]
async fn a_run_now_for_a_runner_not_heard_from_is_refused_as_offline() {
    let f = Fixture::new().await;
    f.teams.clock.advance(Duration::seconds(3 * 60 + 1));

    let refused = f.press(&f.teams.team_a.ready, &f.a_id).await;

    assert_eq!((refused.status, refused.code()), (400, Some("invalid")));
    assert_eq!(refused.body["message"], json!("this runner has not been in touch for more than three minutes, so it cannot start a run now"));
    assert_eq!(f.doors.relayed.held(), vec![]);
}

#[tokio::test]
async fn start_task_run_for_a_runner_outside_every_shared_team_is_not_found() {
    let f = Fixture::new().await;
    let theirs = &f.teams.team_b.credentials.paired_runner_id;

    let refused = f.press(&f.teams.team_a.ready, theirs).await;

    assert_eq!((refused.status, refused.code()), (404, Some("not_found")));
    assert_eq!(
        refused.body["message"],
        json!(format!("no runner with id {theirs}"))
    );
}

#[tokio::test]
async fn the_http_start_refusals_are_the_in_process_sentences() {
    let f = Fixture::new().await;
    let ready = f.teams.team_a.ready.clone();

    // A task pinned to B, pressed for A: 043's pin sentence, as the
    // in-process claim of a runner that is not B answers it.
    lease::pin_for_test(&f.teams.a, &ready, Some(&f.b_id))
        .await
        .expect("pin the task to B");
    let pinned = f.press(&ready, &f.a_id).await;
    let in_process = f
        .teams
        .board(&f.teams.a)
        .claim(ClaimTarget::Run {
            task_id: ready.clone(),
            trigger: RunTrigger::Queued,
            continue_session: false,
            ceiling: Default::default(),
        })
        .await
        .expect_err("a task pinned elsewhere");
    assert_eq!(pinned.body["message"], json!(in_process.to_string()));
    lease::pin_for_test(&f.teams.a, &ready, None)
        .await
        .expect("unpin it");

    // An unpaired runner: 067's sentence.
    runners::unpair_own(&f.teams.a, &f.b_id)
        .await
        .expect("unpair B");
    let unpaired = f.press(&ready, &f.b_id).await;
    let in_process = authorize_start(&f.teams.a, &f.b_id, OwnerPresence::AtRunner)
        .await
        .expect_err("an unpaired runner");
    assert_eq!(unpaired.body["message"], json!(in_process.to_string()));

    // A runner someone else owns: 067's sentence.
    let team = SharedServer::new().await;
    let task = team.task(&team.team.bob, "Bob's plan").await;
    let theirs = team.press(&team.alice_desktop, &task, &team.team.bob).await;
    let in_process = authorize_start(
        &team.team.alice.ctx,
        &team.team.bob.runner_id,
        OwnerPresence::AtRunner,
    )
    .await
    .expect_err("Bob's runner is not Alice's to start");
    assert_eq!(theirs.body["message"], json!(in_process.to_string()));
}

// ---------------------------------------------------------------------------
// On a shared team: consent, and who may stop a run
// ---------------------------------------------------------------------------

/// `testing::shared`'s team behind a server: each person's desktop and runner
/// tokens, each runner heard from.
struct SharedServer {
    team: SharedTeam,
    doors: Doors,
    alice_desktop: Secret,
    bob_desktop: Secret,
    alice_runner: Secret,
    bob_runner: Secret,
}

impl SharedServer {
    async fn new() -> Self {
        let team = SharedTeam::new().await;
        let ctx = team.alice.ctx.clone();
        let desktop = |member: &Member| {
            let ctx = ctx.clone();
            let user = member.user_id.clone();
            async move {
                tokens::create_desktop(&ctx.pool, ctx.clock.as_ref(), &user, "desktop")
                    .await
                    .expect("a desktop token")
                    .token
            }
        };
        let runner = |member: &Member| {
            let ctx = ctx.clone();
            let user = member.user_id.clone();
            let runner_id = member.runner_id.clone();
            async move {
                let mut tx = ctx.pool.begin().await.expect("a transaction");
                let token =
                    tokens::create_runner(&mut tx, ctx.clock.as_ref(), &user, &runner_id, "runner")
                        .await
                        .expect("a runner token");
                tx.commit().await.expect("commit the token");
                board_runners::mark_seen(&ctx, &runner_id)
                    .await
                    .expect("the runner was heard from");
                token.token
            }
        };
        let alice_desktop = desktop(&team.alice).await;
        let bob_desktop = desktop(&team.bob).await;
        let alice_runner = runner(&team.alice).await;
        let bob_runner = runner(&team.bob).await;

        let relayed = Arc::new(RelayedRequests::new());
        let host = BoardHost {
            context: ctx.clone(),
            provider: ProviderProfile::of(&ClaudeProvider),
            paths: AppPaths::new(std::env::temp_dir()),
            rate_limits: Arc::new(RateLimiter::new(ctx.clock.clone())),
            tails: TailRelay::new(SOLO_TAIL_RELAY_CAPACITY),
            streams: StreamControl::new(),
            relay: relayed.clone(),
        };
        let auth = SessionsAndTokens {
            pool: ctx.pool.clone(),
            clock: ctx.clock.clone(),
        };
        let server = TestServer::spawn(state_over(host, relayed.clone(), auth)).await;
        Self {
            team,
            doors: Doors { server, relayed },
            alice_desktop,
            bob_desktop,
            alice_runner,
            bob_runner,
        }
    }

    fn repository(&self) -> &str {
        &self.team.repository.id
    }

    /// A `ready` task `by` writes and assigns to themself.
    async fn task(&self, by: &Member, title: &str) -> String {
        self.team.task(by, title, Some(by)).await
    }

    async fn press(&self, desktop: &Secret, task_id: &str, on: &Member) -> Answer {
        self.doors
            .command(desktop, "start_task_run", run_now(task_id, &on.runner_id))
            .await
    }

    /// `task_id`, claimed by name by `runner`: its lease.
    async fn claim(&self, runner: &Secret, task_id: &str) -> Value {
        let answer = self
            .doors
            .runner(
                runner,
                "claim",
                json!({
                    "target": { "run": { "taskId": task_id, "trigger": "queued",
                                         "continueSession": false } },
                    "provider": "claude-code",
                }),
            )
            .await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        answer.body["lease"].clone()
    }

    /// Alice rewrites `task_id`'s plan, which Bob has not read.
    async fn alice_edits(&self, task_id: &str) {
        tasks::update_task(
            &self.team.alice.ctx,
            task_id,
            TaskPatch {
                plan: Patch::Set("1. Do it Alice's way".to_string()),
                ..TaskPatch::default()
            },
        )
        .await
        .expect("Alice edits the plan");
    }
}

#[tokio::test]
async fn run_now_for_a_runner_the_task_is_not_eligible_for_is_refused_with_the_claims_sentence() {
    let s = SharedServer::new().await;
    // Alice writes it and gives it to Bob, who has not read it.
    let task = s
        .team
        .task(&s.team.alice, "Alice's plan", Some(&s.team.bob))
        .await;

    let refused = s.press(&s.bob_desktop, &task, &s.team.bob).await;
    let in_process = s
        .team
        .board(&s.team.bob)
        .claim(ClaimTarget::Run {
            task_id: task.clone(),
            trigger: RunTrigger::Queued,
            continue_session: false,
            ceiling: Default::default(),
        })
        .await
        .expect_err("Run now on an unconsented task refuses with the reason");

    assert_eq!((refused.status, refused.code()), (400, Some("invalid")));
    assert_eq!(refused.body["message"], json!(in_process.to_string()));
    assert_eq!(
        s.doors.next_when_full(&s.bob_runner, s.repository()).await,
        Value::Null,
        "nothing was held for Bob's runner"
    );
}

#[tokio::test]
async fn a_relayed_run_now_that_is_no_longer_eligible_is_dropped_and_selection_continues() {
    let s = SharedServer::new().await;
    let pressed = s.task(&s.team.bob, "Pressed").await;
    let below = s.task(&s.team.bob, "Below it").await;
    let answer = s.press(&s.bob_desktop, &pressed, &s.team.bob).await;
    assert_eq!(answer.status, 200, "{}", answer.body);

    // Alice edits the pressed plan after the press, before the claim.
    s.alice_edits(&pressed).await;
    let claim = s.doors.next(&s.bob_runner, s.repository(), 1).await;

    assert_eq!(claimed_task(&claim), Some(below.as_str()));
    assert_eq!(s.doors.relayed.held(), vec![], "the request was dropped");
}

#[tokio::test]
async fn a_team_owner_can_stop_a_members_run() {
    let s = SharedServer::new().await;
    let task = s.task(&s.team.bob, "Bob's plan").await;
    let lease = s.claim(&s.bob_runner, &task).await;

    let stopped = s
        .doors
        .command(
            &s.alice_desktop,
            "cancel_task_run",
            json!({ "taskId": task }),
        )
        .await;

    assert_eq!(stopped.status, 200, "{}", stopped.body);
    let heartbeat = s.doors.heartbeat(&s.bob_runner, &[&lease]).await;
    assert_eq!(heartbeat["cancel"], json!([task]));
}

#[tokio::test]
async fn a_member_who_owns_neither_the_runner_nor_the_team_cannot_stop_its_run() {
    let s = SharedServer::new().await;
    let task = s.task(&s.team.alice, "Alice's plan").await;
    let lease = s.claim(&s.alice_runner, &task).await;

    let refused = s
        .doors
        .command(&s.bob_desktop, "cancel_task_run", json!({ "taskId": task }))
        .await;

    assert_eq!((refused.status, refused.code()), (400, Some("invalid")));
    assert_eq!(refused.body["message"], json!("only the owner of the runner holding this task, or an owner of its team, can stop its run"));
    let heartbeat = s.doors.heartbeat(&s.alice_runner, &[&lease]).await;
    assert_eq!(
        heartbeat["cancel"],
        json!([]),
        "nothing was asked of Alice's runner"
    );
}

// ---------------------------------------------------------------------------
// Cancel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cancel_is_listed_on_the_holding_runners_heartbeats_until_its_lease_ends() {
    let f = Fixture::new().await;
    let ready = f.teams.team_a.ready.clone();
    let lease = f.claim_ready().await;

    let stopped = f
        .doors
        .command(&f.desktop, "cancel_task_run", json!({ "taskId": ready }))
        .await;
    assert_eq!(stopped.status, 200, "{}", stopped.body);

    for _ in 0..2 {
        let heartbeat = f.doors.heartbeat(&f.a, &[&lease]).await;
        assert_eq!(
            heartbeat["cancel"],
            json!([ready]),
            "listed again, until it ends"
        );
    }
    let by_b = f.doors.heartbeat(&f.b, &[&lease]).await;
    assert_eq!(by_b["cancel"], json!([]), "never on another runner's");

    let started = f
        .doors
        .runner(
            &f.a,
            "start_run",
            json!({ "lease": lease, "run": {
                "runId": "the-run", "kind": "implementation", "sessionId": "s",
                "prompt": "p", "baseRef": null, "baseSha": null,
            } }),
        )
        .await;
    assert_eq!(started.status, 200, "{}", started.body);
    let finished = f
        .doors
        .runner(
            &f.a,
            "finish_run",
            json!({ "lease": lease, "runId": "the-run", "finish": finish(json!({})) }),
        )
        .await;
    assert_eq!(finished.status, 200, "{}", finished.body);

    let after = f.doors.heartbeat(&f.a, &[&lease]).await;
    assert_eq!(after["cancel"], json!([]), "not after the lease ended");
}

#[tokio::test]
async fn a_cancel_for_an_ended_lease_is_dropped() {
    let f = Fixture::new().await;
    let lease = f.claim_ready().await;
    let stopped = f
        .doors
        .command(
            &f.desktop,
            "cancel_task_run",
            json!({ "taskId": f.teams.team_a.ready }),
        )
        .await;
    assert_eq!(stopped.status, 200);
    assert_eq!(f.doors.relayed.cancels().pending().len(), 1);

    let released = f
        .doors
        .runner(&f.a, "release", json!({ "lease": lease }))
        .await;
    assert_eq!(released.status, 200, "{}", released.body);
    f.doors.heartbeat(&f.a, &[]).await;

    assert_eq!(f.doors.relayed.cancels().pending(), vec![]);
}

#[tokio::test]
async fn cancel_with_nothing_running_is_a_no_op() {
    let f = Fixture::new().await;

    let answer = f
        .doors
        .command(
            &f.desktop,
            "cancel_task_run",
            json!({ "taskId": f.teams.team_a.ready }),
        )
        .await;

    assert_eq!((answer.status, &answer.body), (200, &Value::Null));
    assert_eq!(f.doors.relayed.cancels().pending(), vec![]);
}
