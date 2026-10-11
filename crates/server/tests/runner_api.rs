//! The runner protocol over HTTP (task 052, seam-contract D31 point 10).
//!
//! `/api/v1/runner/<method>` against the real router, the real
//! `SessionsAndTokens` and a two-team board: who may call it, which version,
//! what a body may claim, and what it refuses. The behaviour behind each route
//! is the board service's, which the contract suite runs over these routes
//! through `rimaia-runner`'s `HttpBoard`; this file holds what only the
//! transport decides.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::api::tail::SOLO_TAIL_RELAY_CAPACITY;
use rimaia_core::api::{protocol, BoardHost, Effect, StreamControl, TailRelay, PROTOCOL_VERSION};
use rimaia_core::board::{BoardMethod, RelayedRequests};
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::identity::secret::{self, Secret};
use rimaia_core::identity::{pairing, runners, tokens};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderProfile};
use rimaia_core::testing::api as testing_api;
use rimaia_core::testing::shared::SharedTeam;
use rimaia_core::testing::teams::TwoTeams;
use rimaia_core::{AppPaths, Clock, ServiceContext};
use serde_json::{json, Value};

use common::{state_over, Answer, TestServer};

/// A server over a two-team board, authenticated by real tokens.
struct Fixture {
    teams: TwoTeams,
    server: TestServer,
    /// The server's tail relay, which `get_run_tail` answers from.
    tails: TailRelay,
}

impl Fixture {
    async fn new() -> Self {
        let teams = TwoTeams::new().await;
        let (host, relayed) = testing_api::relayed_host(&teams).await;
        let tails = host.tails.clone();
        let auth = SessionsAndTokens {
            pool: teams.a.pool.clone(),
            clock: Arc::new(teams.clock.clone()),
        };
        let server = TestServer::spawn(state_over(host, relayed, auth)).await;
        Self {
            teams,
            server,
            tails,
        }
    }

    /// Team A's paired runner's token.
    fn token(&self) -> String {
        self.teams
            .team_a
            .credentials
            .runner_token
            .expose()
            .to_string()
    }

    /// `method` as team A's runner, at this build's version.
    async fn call(&self, method: &str, body: &Value) -> Answer {
        self.call_as(&self.token(), method, body).await
    }

    async fn call_as(&self, token: &str, method: &str, body: &Value) -> Answer {
        Answer::of(
            self.server
                .post(
                    &format!("runner/{method}"),
                    Some(token),
                    Some(PROTOCOL_VERSION),
                    body.to_string(),
                )
                .await,
        )
        .await
    }

    /// Team A's `ready` task, claimed by team A's runner: its lease.
    async fn claim(&self) -> Value {
        let answer = self
            .call("claim", &run_target(&self.teams.team_a.ready))
            .await;
        assert_eq!(answer.status, 200, "{}", answer.body);
        answer.body["lease"].clone()
    }

    /// Opens `run_id` under `lease`, as a runner does before its first tail.
    async fn start_run(&self, lease: &Value, run_id: &str) {
        let answer = self
            .call(
                "start_run",
                &json!({ "lease": lease, "run": {
                    "runId": run_id, "kind": "implementation", "sessionId": "s",
                    "prompt": "p", "baseRef": null, "baseSha": null,
                } }),
            )
            .await;
        assert_eq!(answer.status, 200, "{}", answer.body);
    }

    async fn last_seen(&self) -> Option<DateTime<Utc>> {
        runners::list(&self.teams.a)
            .await
            .expect("the owner's runners")
            .into_iter()
            .find(|runner| runner.id == self.teams.team_a.credentials.paired_runner_id)
            .expect("the paired runner")
            .last_seen_at
    }
}

fn run_target(task_id: &str) -> Value {
    json!({
        "target": { "run": { "taskId": task_id, "trigger": "queued", "continueSession": false } },
        "provider": "claude-code",
    })
}

fn lease(task_id: &str, generation: i64, team_id: &str) -> Value {
    json!({ "taskId": task_id, "generation": generation, "teamId": team_id })
}

// ---------------------------------------------------------------------------
// The routes and the caller
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_board_method_has_exactly_one_runner_route() {
    let f = Fixture::new().await;

    let mut failures = Vec::new();
    for method in BoardMethod::ALL {
        let answer = Answer::of(
            f.server
                .post(&format!("runner/{}", method.as_str()), None, None, "{}")
                .await,
        )
        .await;
        let got = (
            answer.status,
            answer.code(),
            answer.www_authenticate.as_deref(),
        );
        if got != (401, Some("unauthenticated"), Some("Bearer")) {
            failures.push(format!("{}: {got:?}", method.as_str()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    // No general run-state setter on the port (D31 point 4), and so no route.
    let unknown = Answer::of(
        f.server
            .post("runner/set_run_state", Some(&f.token()), None, "{}")
            .await,
    )
    .await;
    assert_eq!((unknown.status, unknown.code()), (404, Some("not_found")));
    assert_eq!(unknown.content_type.as_deref(), Some("application/json"));
}

#[tokio::test]
async fn a_runner_route_accepts_only_a_runner_token() {
    let f = Fixture::new().await;
    let lease = f.claim().await;
    let credentials = &f.teams.team_a.credentials;
    let session = credentials.session_secret.expose();
    let cookie = format!("rimaia_session={session}");
    let csrf = secret::csrf_for(session);

    for (method, body) in [
        ("heartbeat", json!({ "held": [] })),
        ("release", json!({ "lease": lease })),
    ] {
        let url = format!("{}/api/v1/runner/{method}", f.server.base_url());
        let send = |request: reqwest::RequestBuilder| {
            let body = body.to_string();
            async move {
                Answer::of(
                    request
                        .header("rimaia-protocol", PROTOCOL_VERSION)
                        .body(body)
                        .send()
                        .await
                        .expect("the request reaches the server"),
                )
                .await
            }
        };
        let client = f.server.client();
        let refused = [
            (
                "a desktop token",
                send(
                    client
                        .post(&url)
                        .bearer_auth(credentials.desktop_token.expose()),
                )
                .await,
            ),
            (
                "a personal token",
                send(
                    client
                        .post(&url)
                        .bearer_auth(credentials.personal_token.expose()),
                )
                .await,
            ),
            (
                "a session cookie with its CSRF header",
                send(
                    client
                        .post(&url)
                        .header("cookie", &cookie)
                        .header("x-rimaia-csrf", &csrf),
                )
                .await,
            ),
            (
                "a cookie beside the runner's own token",
                send(
                    client
                        .post(&url)
                        .header("cookie", &cookie)
                        .bearer_auth(f.token()),
                )
                .await,
            ),
        ];
        for (what, answer) in refused {
            assert_eq!(
                (answer.status, answer.code()),
                (401, Some("unauthenticated")),
                "{method} with {what}: {}",
                answer.body
            );
        }

        let served = send(client.post(&url).bearer_auth(f.token())).await;
        assert_eq!(served.status, 200, "{method}: {}", served.body);
    }
}

#[tokio::test]
async fn a_revoked_runner_token_is_refused_on_its_next_request() {
    let f = Fixture::new().await;
    let held = json!({ "held": [] });
    assert_eq!(f.call("heartbeat", &held).await.status, 200);

    tokens::revoke(&f.teams.a, &f.teams.team_a.credentials.runner_token_id)
        .await
        .expect("the owner revokes the token");

    let refused = f.call("heartbeat", &held).await;
    assert_eq!(
        (refused.status, refused.code()),
        (401, Some("unauthenticated"))
    );
}

#[tokio::test]
async fn an_unpaired_runners_token_is_refused() {
    let f = Fixture::new().await;
    let held = json!({ "held": [] });
    assert_eq!(f.call("heartbeat", &held).await.status, 200);

    runners::unpair_own(&f.teams.a, &f.teams.team_a.credentials.paired_runner_id)
        .await
        .expect("the owner unpairs the runner");

    let refused = f.call("heartbeat", &held).await;
    assert_eq!(
        (refused.status, refused.code()),
        (401, Some("unauthenticated"))
    );
}

#[tokio::test]
async fn a_runner_sees_only_its_owners_teams() {
    let f = Fixture::new().await;
    let theirs = &f.teams.team_b;

    let preview = f
        .call(
            "preview",
            &json!({ "taskId": theirs.ready, "of": "run", "provider": "claude-code" }),
        )
        .await;
    assert_eq!((preview.status, preview.code()), (404, Some("not_found")));

    let release = f
        .call(
            "release",
            &json!({ "lease": lease(&theirs.ready, 1, &theirs.team_id) }),
        )
        .await;
    assert_eq!((release.status, release.code()), (404, Some("not_found")));
}

/// One method's cross-team request: its path and query, and its body.
struct Request {
    path: String,
    body: Vec<u8>,
}

/// What a method names of team B, sent as team A's runner.
struct RunnerCase {
    method: BoardMethod,
    request: fn(&TwoTeams) -> Request,
    /// Whether the answer is `not_found`, rather than an answer that reads as
    /// a never-issued id's (a lost claim, a fenced heartbeat, a dropped tail).
    not_found: bool,
}

fn json_request(method: &str, body: Value) -> Request {
    Request {
        path: format!("runner/{method}"),
        body: body.to_string().into_bytes(),
    }
}

/// Team B's `ready` task under a lease of team B's.
fn their_lease(t: &TwoTeams) -> Value {
    lease(&t.team_b.ready, 1, &t.team_b.team_id)
}

/// One per `BoardMethod`. `every_runner_route_has_a_cross_team_case` holds it
/// equal to `BoardMethod::ALL` (ADR-0029 point 5's registry test, for the
/// runner protocol).
static RUNNER_CASES: &[RunnerCase] = &[
    RunnerCase {
        method: BoardMethod::Preview,
        request: |t| {
            json_request(
                "preview",
                json!({ "taskId": t.team_b.ready, "of": "run", "provider": "claude-code" }),
            )
        },
        not_found: true,
    },
    RunnerCase {
        method: BoardMethod::Claim,
        request: |t| json_request("claim", run_target(&t.team_b.ready)),
        not_found: false,
    },
    RunnerCase {
        method: BoardMethod::Heartbeat,
        request: |t| json_request("heartbeat", json!({ "held": [their_lease(t)] })),
        not_found: false,
    },
    RunnerCase {
        method: BoardMethod::RunContext,
        request: |t| {
            json_request(
                "run_context",
                json!({ "lease": their_lease(t), "provider": "claude-code" }),
            )
        },
        not_found: true,
    },
    RunnerCase {
        method: BoardMethod::RecordBranch,
        request: |t| {
            json_request(
                "record_branch",
                json!({ "lease": their_lease(t), "branch": "rimaia/elsewhere" }),
            )
        },
        not_found: true,
    },
    RunnerCase {
        method: BoardMethod::StartRun,
        request: |t| {
            json_request(
                "start_run",
                json!({ "lease": their_lease(t), "run": {
                    "runId": "a-run", "kind": "implementation", "sessionId": "s",
                    "prompt": "p", "baseRef": null, "baseSha": null,
                } }),
            )
        },
        not_found: true,
    },
    RunnerCase {
        method: BoardMethod::AppendTranscript,
        request: |t| Request {
            path: format!(
                "runner/append_transcript?taskId={}&teamId={}&generation=1&runId={}&offset=0",
                t.team_b.ready, t.team_b.team_id, t.team_b.implementation_run
            ),
            body: b"{}\n".to_vec(),
        },
        not_found: true,
    },
    RunnerCase {
        method: BoardMethod::PublishTail,
        request: |t| {
            json_request(
                "publish_tail",
                json!({ "lease": their_lease(t), "tail": {
                    "runId": t.team_b.implementation_run, "elapsedMs": 1, "turns": 1,
                    "currentTool": null, "lastAssistantText": null,
                } }),
            )
        },
        not_found: false,
    },
    RunnerCase {
        method: BoardMethod::FinishRun,
        request: |t| {
            json_request(
                "finish_run",
                json!({ "lease": their_lease(t), "runId": t.team_b.implementation_run,
                        "finish": finish_body() }),
            )
        },
        not_found: true,
    },
    RunnerCase {
        method: BoardMethod::Release,
        request: |t| json_request("release", json!({ "lease": their_lease(t) })),
        not_found: true,
    },
    RunnerCase {
        method: BoardMethod::RecordStrategy,
        request: |t| {
            json_request(
                "record_strategy",
                json!({ "lease": their_lease(t), "plan": { "status": "failed" } }),
            )
        },
        not_found: true,
    },
    RunnerCase {
        method: BoardMethod::RecordReviewFindings,
        request: |t| {
            json_request(
                "record_review_findings",
                json!({ "lease": their_lease(t), "runId": t.team_b.review_run, "findings": [] }),
            )
        },
        not_found: true,
    },
];

fn finish_body() -> Value {
    json!({
        "outcome": {
            "exitClass": "success", "status": "succeeded", "errorMessage": null,
            "numTurns": 1, "costUsd": null, "durationMs": null, "prUrl": null,
            "usageLimitResetsAt": null, "resumeAfter": null,
            "spawnedAs": { "model": null, "effort": null, "runEnvironment": null },
            "usage": {},
        },
        "headSha": null,
        "bundle": null,
        "windowClosesAt": null,
        "transcript": { "complete": { "length": 0 } },
    })
}

#[test]
fn every_runner_route_has_a_cross_team_case() {
    let cases: BTreeSet<&str> = RUNNER_CASES
        .iter()
        .map(|case| case.method.as_str())
        .collect();
    let methods: BTreeSet<&str> = BoardMethod::ALL.iter().map(|m| m.as_str()).collect();

    assert_eq!(cases, methods, "a runner method with no cross-team case");
    assert_eq!(RUNNER_CASES.len(), cases.len(), "a method with two cases");
}

#[tokio::test]
async fn every_runner_route_answers_another_teams_ids_as_never_issued_ones() {
    let mut f = Fixture::new().await;
    let mut tails = f.teams.a.subscribe_tail();

    let mut failures = Vec::new();
    for case in RUNNER_CASES {
        let name = case.method.as_str();
        let before = f.teams.snapshot_b().await;
        let request = (case.request)(&f.teams);
        let foreign = f.send(&request).await;

        // The same request, every id of team B's swapped for a fresh one.
        let mut text = format!(
            "{}\n{}",
            request.path,
            String::from_utf8_lossy(&request.body)
        );
        let mut swapped = Vec::new();
        for id in f.teams.team_b.ids() {
            if text.contains(&id) {
                let fresh = TwoTeams::never_issued();
                text = text.replace(&id, &fresh);
                swapped.push((id, fresh));
            }
        }
        let (path, body) = text.split_once('\n').expect("the two halves");
        let missing = f
            .send(&Request {
                path: path.to_string(),
                body: body.as_bytes().to_vec(),
            })
            .await;
        let restored = testing_api::restore_ids(&missing.body.to_string(), &swapped);

        if (foreign.status, foreign.body.to_string()) != (missing.status, restored.clone()) {
            failures.push(format!(
                "{name}: B's ids answer {} {}, a never-issued id {} {restored}",
                foreign.status, foreign.body, missing.status
            ));
        }
        if case.not_found && foreign.code() != Some("not_found") {
            failures.push(format!("{name}: {} {}", foreign.status, foreign.body));
        }
        let sent = String::from_utf8_lossy(&request.body).to_string() + &request.path;
        for leaked in testing_api::leaks(&f.teams.team_b, &json!(sent), &foreign.body.to_string()) {
            failures.push(format!("{name}: the answer carries B's {leaked}"));
        }
        if f.teams.snapshot_b().await != before {
            failures.push(format!("{name}: team B's rows changed"));
        }
        if let Ok(tail) = tails.try_recv() {
            failures.push(format!("{name}: a tail reached the channel: {tail:?}"));
        }
        f.teams.drain_changes();
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

impl Fixture {
    async fn send(&self, request: &Request) -> Answer {
        Answer::of(
            self.server
                .post(
                    &request.path,
                    Some(&self.token()),
                    Some(PROTOCOL_VERSION),
                    request.body.clone(),
                )
                .await,
        )
        .await
    }
}

#[tokio::test]
async fn a_body_provider_that_differs_from_the_paired_one_is_invalid() {
    let f = Fixture::new().await;
    let lease = f.claim().await;
    let task = &f.teams.team_a.not_ready;
    let refusal = "this runner was paired as claude-code, and its request names ledger; a runner \
                   whose provider changed is paired again";

    for (method, body) in [
        (
            "preview",
            json!({ "taskId": task, "of": "run", "provider": "ledger" }),
        ),
        (
            "claim",
            json!({ "target": { "run": { "taskId": task, "trigger": "queued",
                                         "continueSession": false } },
                    "provider": "ledger" }),
        ),
        (
            "run_context",
            json!({ "lease": lease, "provider": "ledger" }),
        ),
    ] {
        let answer = f.call(method, &body).await;
        assert_eq!(
            (answer.status, answer.code()),
            (400, Some("invalid")),
            "{method}: {}",
            answer.body
        );
        assert_eq!(answer.body["message"], json!(refusal), "{method}");
    }
}

#[tokio::test]
async fn a_runner_request_marks_the_runner_seen_at_most_once_per_heartbeat_interval() {
    let mut f = Fixture::new().await;
    let held = json!({ "held": [] });
    f.teams.clock.advance(Duration::minutes(1));
    let first = f.teams.clock.now();
    f.teams.drain_changes();

    assert_eq!(f.call("heartbeat", &held).await.status, 200);
    assert_eq!(f.last_seen().await, Some(first));

    f.teams.clock.advance(Duration::seconds(10));
    assert_eq!(f.call("heartbeat", &held).await.status, 200);
    assert_eq!(f.last_seen().await, Some(first), "written once in 30 s");

    f.teams.clock.advance(Duration::seconds(21));
    assert_eq!(f.call("heartbeat", &held).await.status, 200);
    assert_eq!(
        f.last_seen().await,
        Some(first + Duration::seconds(31)),
        "written again past the interval"
    );

    assert_eq!(
        f.teams.drain_changes(),
        vec![],
        "presence publishes nothing"
    );
}

#[tokio::test]
async fn a_runner_route_ignores_the_rimaia_team_header() {
    let f = Fixture::new().await;
    // A runner whose owner is in two teams: team A's guest, in team A and in
    // their own.
    let guest = ServiceContext {
        actor: f.teams.team_a.guest_id.clone(),
        ..f.teams.a.clone()
    };
    let paired = pairing::pair_own_runner(&guest, "guest's laptop", "claude-code")
        .await
        .expect("the guest pairs a runner");

    let body = json!({ "taskId": f.teams.team_a.ready, "of": "run", "provider": "claude-code" });
    let url = format!("{}/api/v1/runner/preview", f.server.base_url());

    let send = |team: Option<&str>| {
        let mut request = f
            .server
            .client()
            .post(&url)
            .bearer_auth(paired.token.expose())
            .header("rimaia-protocol", PROTOCOL_VERSION)
            .body(body.to_string());
        if let Some(team) = team {
            request = request.header("rimaia-team", team);
        }
        async move { Answer::of(request.send().await.expect("the request")).await }
    };
    // Whatever the board answers the guest's runner about team A's task, a
    // header naming their personal team would turn it into `not_found` if it
    // narrowed the caller, as it does on a board route.
    let plain = send(None).await;
    assert_ne!(plain.code(), Some("not_found"), "{}", plain.body);

    for team in [
        f.teams.team_a.team_id.as_str(),
        f.teams.team_a.guest_personal_team.as_str(),
    ] {
        let named = send(Some(team)).await;
        assert_eq!((named.status, &named.body), (plain.status, &plain.body));
    }
}

#[tokio::test]
async fn a_tail_under_a_stale_generation_is_dropped_over_http() {
    let f = Fixture::new().await;
    let lease = f.claim().await;
    f.start_run(&lease, "a-run").await;
    let mut tails = f.teams.a.subscribe_tail();
    let tail = json!({ "runId": "a-run", "elapsedMs": 1, "turns": 1,
                       "currentTool": null, "lastAssistantText": null });
    let mut stale = lease.clone();
    stale["generation"] = json!(lease["generation"].as_i64().expect("a generation") - 1);

    let answer = f
        .call("publish_tail", &json!({ "lease": stale, "tail": tail }))
        .await;

    assert_eq!((answer.status, &answer.body), (200, &Value::Null));
    assert!(
        tails.try_recv().is_err(),
        "a stale tail reached the channel"
    );

    // The current lease's tail does reach it, so the drop was the fence's.
    let answer = f
        .call("publish_tail", &json!({ "lease": lease, "tail": tail }))
        .await;
    assert_eq!(answer.status, 200);
    assert!(tails.try_recv().is_ok(), "the current lease's tail");
}

#[tokio::test]
async fn a_tail_naming_a_run_its_lease_is_not_open_for_is_dropped_over_http() {
    let f = Fixture::new().await;
    let lease = f.claim().await;
    f.start_run(&lease, "a-run").await;
    let mut subscriber = f.teams.a.subscribe_tail();
    let tail = |run_id: &str| {
        json!({ "runId": run_id, "elapsedMs": 1, "turns": 1,
                "currentTool": null, "lastAssistantText": "forged" })
    };
    // Another run in the runner's own team, and a run in a team its owner is
    // not in, each with a cached catch-up tail from the fixture.
    let others = [
        f.teams.team_a.implementation_run.clone(),
        f.teams.team_b.implementation_run.clone(),
    ];
    let cached: Vec<_> = others.iter().map(|run| f.tails.latest(run)).collect();
    assert!(cached.iter().all(Option::is_some), "the fixture's tails");

    for other in &others {
        let answer = f
            .call(
                "publish_tail",
                &json!({ "lease": lease, "tail": tail(other) }),
            )
            .await;
        assert_eq!(
            (answer.status, &answer.body),
            (200, &Value::Null),
            "{other}"
        );
        assert!(
            subscriber.try_recv().is_err(),
            "a tail for {other} reached the channel"
        );
    }

    // The lease's own run's tail does reach it, so the drop was the run
    // check's. The relay hears the channel in order, so once it holds this
    // one it would hold anything published before it.
    let answer = f
        .call(
            "publish_tail",
            &json!({ "lease": lease, "tail": tail("a-run") }),
        )
        .await;
    assert_eq!(answer.status, 200);
    assert!(subscriber.try_recv().is_ok(), "the lease's own run's tail");
    while f.tails.latest("a-run").is_none() {
        tokio::task::yield_now().await;
    }
    let after: Vec<_> = others.iter().map(|run| f.tails.latest(run)).collect();
    assert_eq!(after, cached, "the other runs' cached tails");
}

#[tokio::test]
async fn a_runner_body_over_the_transport_limit_is_invalid_in_the_error_shape() {
    let f = Fixture::new().await;
    let body = vec![b' '; 2 * 1024 * 1024 + 1];

    let answer = Answer::of(
        f.server
            .post(
                "runner/finish_run",
                Some(&f.token()),
                Some(PROTOCOL_VERSION),
                body,
            )
            .await,
    )
    .await;

    assert_eq!((answer.status, answer.code()), (400, Some("invalid")));
    assert_eq!(
        answer.body["message"],
        json!("the request body is larger than the 2097152 bytes a runner route accepts")
    );
}

// ---------------------------------------------------------------------------
// Fencing between two members' runners
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_runner_cannot_report_on_a_lease_another_runner_holds() {
    let team = SharedTeam::new().await;
    let token = |member: &rimaia_core::testing::shared::Member| {
        let ctx = team.alice.ctx.clone();
        let user = member.user_id.clone();
        let runner = member.runner_id.clone();
        async move {
            let mut tx = ctx.pool.begin().await.expect("a transaction");
            let token =
                tokens::create_runner(&mut tx, ctx.clock.as_ref(), &user, &runner, "runner")
                    .await
                    .expect("a runner token");
            tx.commit().await.expect("commit");
            token.token
        }
    };
    let alice: Secret = token(&team.alice).await;
    let bob: Secret = token(&team.bob).await;
    let relayed = Arc::new(RelayedRequests::new());
    let host = BoardHost {
        context: team.alice.ctx.clone(),
        provider: ProviderProfile::of(&ClaudeProvider),
        paths: AppPaths::new(std::env::temp_dir()),
        rate_limits: Arc::new(RateLimiter::new(Arc::new(team.clock.clone()))),
        tails: TailRelay::new(SOLO_TAIL_RELAY_CAPACITY),
        streams: StreamControl::new(),
        relay: relayed.clone(),
    };
    let auth = SessionsAndTokens {
        pool: team.alice.ctx.pool.clone(),
        clock: Arc::new(team.clock.clone()),
    };
    let server = TestServer::spawn(state_over(host, relayed, auth)).await;
    let call = |token: &Secret, method: &str, body: Value| {
        let token = token.expose().to_string();
        let method = method.to_string();
        let server = &server;
        async move {
            Answer::of(
                server
                    .post(
                        &format!("runner/{method}"),
                        Some(&token),
                        Some(PROTOCOL_VERSION),
                        body.to_string(),
                    )
                    .await,
            )
            .await
        }
    };

    // Bob's runner holds Bob's task, with a run open under it.
    let task = team.task(&team.bob, "Bob's plan", Some(&team.bob)).await;
    let claim = call(&bob, "claim", run_target(&task)).await;
    assert_eq!(claim.status, 200, "{}", claim.body);
    let bobs_lease = claim.body["lease"].clone();
    let started = call(
        &bob,
        "start_run",
        json!({ "lease": bobs_lease, "run": {
            "runId": "bobs-run", "kind": "implementation", "sessionId": "s", "prompt": "p",
            "baseRef": null, "baseSha": null,
        } }),
    )
    .await;
    assert_eq!(started.status, 200, "{}", started.body);
    let before = rows(&team.alice.ctx).await;

    // Alice's runner sends Bob's lease, exactly as Bob's runner holds it.
    for (method, body) in [
        (
            "finish_run",
            json!({ "lease": bobs_lease, "runId": "bobs-run", "finish": finish_body() }),
        ),
        ("release", json!({ "lease": bobs_lease })),
    ] {
        let answer = call(&alice, method, body).await;
        assert_eq!(
            (answer.status, answer.code()),
            (409, Some("conflict")),
            "{method}: {}",
            answer.body
        );
    }
    assert_eq!(rows(&team.alice.ctx).await, before, "nothing was written");
}

/// Every row of `tasks`, `runs` and `runner_leases`, as text.
async fn rows(ctx: &ServiceContext) -> Vec<String> {
    rimaia_core::testing::db::rows_of(&ctx.pool, &["tasks", "runs", "runner_leases"]).await
}

// ---------------------------------------------------------------------------
// The protocol version
// ---------------------------------------------------------------------------

/// What `protocol::check` answers a write from `header`: 046's message.
fn refusal(header: Option<&str>) -> String {
    protocol::check(header, Effect::Write)
        .expect_err("refused")
        .to_string()
}

/// `method` at `header`, as team A's runner.
async fn at_version(f: &Fixture, method: &str, body: &Value, header: Option<&str>) -> Answer {
    Answer::of(
        f.server
            .post(
                &format!("runner/{method}"),
                Some(&f.token()),
                header,
                body.to_string(),
            )
            .await,
    )
    .await
}

async fn refused_at(header: Option<&str>) {
    let f = Fixture::new().await;
    let lease = f.claim().await;
    for (method, body) in [
        ("heartbeat", json!({ "held": [] })),
        ("release", json!({ "lease": lease })),
    ] {
        let answer = at_version(&f, method, &body, header).await;
        assert_eq!(
            (answer.status, answer.code()),
            (426, Some("upgrade_required")),
            "{method} at {header:?}"
        );
        assert_eq!(answer.body["message"], json!(refusal(header)), "{method}");

        let served = at_version(&f, method, &body, Some("1.0")).await;
        assert_eq!(served.status, 200, "{method} at 1.0: {}", served.body);
    }
}

#[tokio::test]
async fn a_runner_request_without_the_protocol_header_is_refused() {
    refused_at(None).await;
}

#[tokio::test]
async fn a_runner_ahead_of_the_server_is_refused() {
    refused_at(Some("1.1")).await;
}

#[tokio::test]
async fn a_runner_on_another_major_version_is_refused() {
    refused_at(Some("2.0")).await;
}

#[tokio::test]
async fn a_stale_runner_cannot_read_a_run_context_either() {
    let f = Fixture::new().await;
    let lease = f.claim().await;

    for (method, body) in [
        (
            "run_context",
            json!({ "lease": lease, "provider": "claude-code" }),
        ),
        (
            "preview",
            json!({ "taskId": f.teams.team_a.not_ready, "of": "run", "provider": "claude-code" }),
        ),
    ] {
        let answer = at_version(&f, method, &body, Some("1.1")).await;
        assert_eq!(
            (answer.status, answer.code()),
            (426, Some("upgrade_required")),
            "{method}"
        );
    }
}
