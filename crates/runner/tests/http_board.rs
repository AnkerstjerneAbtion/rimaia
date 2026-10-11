//! `HttpBoard`'s own behaviour (task 052): what it sends, how it reads what
//! comes back, and what it refuses. The contract suite proves it answers as
//! the in-process adapter does (`board_port_http.rs`); these pin the parts no
//! in-process adapter has, against a stub router that records every request
//! and answers what a case tells it to, and against the real server where the
//! answer is the board's.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use pretty_assertions::assert_eq;
use rimaia_core::api::PROTOCOL_VERSION;
use rimaia_core::board::{
    BoardMethod, BoardPort, ClaimTarget, FinishRun, Heartbeat, LeaseRef, PreviewOf, RunContext,
    StartRun, TranscriptChunk, TranscriptEnd,
};
use rimaia_core::db::{ExitClass, RunKind, RunStatus};
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::pairing;
use rimaia_core::identity::secret::Secret;
use rimaia_core::runner::events::{RunTail, TokenUsage};
use rimaia_core::runner::outcome::{RunOutcome, SpawnedAs};
use rimaia_core::runner::provider::ProviderId;
use rimaia_core::runner::RunTrigger;
use rimaia_core::strategy::catalogue::catalogue_for_provider;
use rimaia_core::tasks::strategy::StrategyPlan;
use rimaia_core::testing::api as testing_api;
use rimaia_core::testing::identity::FakeIdentityProvider;
use rimaia_core::testing::teams::TwoTeams;
use rimaia_core::{Error, ErrorCode};
use rimaia_runner::board::http::TAIL_BUFFER;
use rimaia_runner::board::HttpBoard;
use rimaia_server::ServerState;
use serde_json::{json, Value};
use tokio::sync::Notify;

const TOKEN: &str = "rmr_a-runner-token-nobody-may-see";

// ---------------------------------------------------------------------------
// The stub
// ---------------------------------------------------------------------------

/// One request the stub received.
#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    query: Option<String>,
    authorization: Option<String>,
    protocol: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

#[derive(Default)]
struct StubState {
    received: Mutex<Vec<Recorded>>,
    /// What every request is answered: `null` with `200` until a case says
    /// otherwise.
    reply: Mutex<Option<(StatusCode, String)>>,
    arrived: Notify,
}

/// A router that answers every path, records it, and replies with whatever
/// the case set, on `127.0.0.1:0`.
struct Stub {
    origin: String,
    state: Arc<StubState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn record(
    State(state): State<Arc<StubState>>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    state.received.lock().expect("the record").push(Recorded {
        path: uri.path().to_string(),
        query: uri.query().map(str::to_string),
        authorization: header("authorization"),
        protocol: header("rimaia-protocol"),
        content_type: header("content-type"),
        body: body.to_vec(),
    });
    state.arrived.notify_one();
    let (status, body) = state
        .reply
        .lock()
        .expect("the reply")
        .clone()
        .unwrap_or((StatusCode::OK, "null".to_string()));
    (status, [("content-type", "application/json")], body).into_response()
}

impl Stub {
    async fn spawn() -> Self {
        let state = Arc::new(StubState::default());
        let router = Router::new().fallback(record).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port");
        let origin = format!("http://{}", listener.local_addr().expect("an address"));
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("the stub serves");
        });
        Self {
            origin,
            state,
            task,
        }
    }

    fn board(&self, provider: ProviderId) -> HttpBoard {
        HttpBoard::new(&self.origin, Secret::new(TOKEN), provider).expect("a loopback origin")
    }

    fn reply(&self, status: StatusCode, body: impl Into<String>) {
        *self.state.reply.lock().expect("the reply") = Some((status, body.into()));
    }

    fn received(&self) -> Vec<Recorded> {
        self.state.received.lock().expect("the record").clone()
    }

    /// Waits until `count` requests have arrived, bounded so a request that
    /// never comes fails rather than hangs. Not a sleep: it wakes on each
    /// arrival.
    async fn wait_for(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while self.received().len() < count {
                self.state.arrived.notified().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{count} requests never arrived"));
    }
}

fn lease() -> LeaseRef {
    LeaseRef::new("task-1", 3, "team-1")
}

fn tail() -> RunTail {
    RunTail {
        run_id: "run-1".to_string(),
        elapsed_ms: 1_000,
        turns: 1,
        current_tool: None,
        last_assistant_text: None,
    }
}

fn finish() -> FinishRun {
    FinishRun {
        outcome: RunOutcome {
            exit_class: ExitClass::Success,
            status: RunStatus::Succeeded,
            error_message: None,
            num_turns: Some(1),
            cost_usd: None,
            duration_ms: None,
            pr_url: None,
            usage_limit_resets_at: None,
            resume_after: None,
            spawned_as: SpawnedAs::default(),
            usage: TokenUsage::default(),
        },
        head_sha: None,
        bundle: None,
        window_closes_at: None,
        transcript: TranscriptEnd::Complete { length: 0 },
        ceiling: Default::default(),
    }
}

/// One call of `method`, whatever it answers. Exhaustive, so a method added
/// to the port has to be added here.
async fn call(board: &HttpBoard, method: BoardMethod) {
    let lease = lease();
    let _ = match method {
        BoardMethod::Preview => board.preview("task-1", PreviewOf::Run).await.map(drop),
        BoardMethod::Claim => board
            .claim(ClaimTarget::Run {
                task_id: "task-1".to_string(),
                trigger: RunTrigger::Queued,
                continue_session: false,
                ceiling: Default::default(),
            })
            .await
            .map(drop),
        BoardMethod::Heartbeat => board
            .heartbeat(std::slice::from_ref(&lease))
            .await
            .map(drop),
        BoardMethod::RunContext => board.run_context(&lease).await.map(drop),
        BoardMethod::RecordBranch => board.record_branch(&lease, "rimaia/x").await,
        BoardMethod::StartRun => {
            board
                .start_run(
                    &lease,
                    StartRun {
                        run_id: "run-1".to_string(),
                        kind: RunKind::Implementation,
                        session_id: "session".to_string(),
                        prompt: "prompt".to_string(),
                        base_ref: None,
                        base_sha: None,
                    },
                )
                .await
        }
        BoardMethod::AppendTranscript => board
            .append_transcript(
                &lease,
                TranscriptChunk {
                    run_id: "run-1".to_string(),
                    offset: 7,
                    bytes: b"{}\n".to_vec(),
                },
            )
            .await
            .map(drop),
        BoardMethod::PublishTail => {
            board.publish_tail(&lease, tail());
            Ok(())
        }
        BoardMethod::FinishRun => board.finish_run(&lease, "run-1", finish()).await.map(drop),
        BoardMethod::Release => board.release(&lease).await,
        BoardMethod::RecordStrategy => {
            board
                .record_strategy(&lease, StrategyPlan::failed("nothing"))
                .await
        }
        BoardMethod::RecordReviewFindings => {
            board.record_review_findings(&lease, "run-1", vec![]).await
        }
    };
}

// ---------------------------------------------------------------------------
// What it sends
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_runner_request_carries_the_token_and_the_protocol_version() {
    let stub = Stub::spawn().await;
    let board = stub.board(ProviderId::ClaudeCode);

    for (sent, method) in BoardMethod::ALL.into_iter().enumerate() {
        call(&board, method).await;
        stub.wait_for(sent + 1).await;
    }

    let received = stub.received();
    assert_eq!(
        received
            .iter()
            .map(|request| request.path.clone())
            .collect::<Vec<_>>(),
        BoardMethod::ALL
            .map(|method| format!("/api/v1/runner/{}", method.as_str()))
            .to_vec(),
    );
    for request in &received {
        assert_eq!(
            request.authorization.as_deref(),
            Some(format!("Bearer {TOKEN}").as_str()),
            "{}",
            request.path
        );
        assert_eq!(
            request.protocol.as_deref(),
            Some(PROTOCOL_VERSION),
            "{}",
            request.path
        );
    }

    // The one body that is not JSON: the bytes, with ids alone in the query.
    let transcript = received
        .iter()
        .find(|request| request.path.ends_with("/append_transcript"))
        .expect("the transcript request");
    assert_eq!(transcript.body, b"{}\n".to_vec());
    assert_eq!(
        transcript.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(
        transcript.query.as_deref(),
        Some("taskId=task-1&teamId=team-1&generation=3&runId=run-1&offset=7")
    );
    for request in received.iter().filter(|request| request.body != b"{}\n") {
        assert_eq!(
            request.content_type.as_deref(),
            Some("application/json"),
            "{}",
            request.path
        );
    }
}

#[tokio::test]
async fn the_adapter_sends_its_provider_on_preview_claim_and_run_context() {
    let stub = Stub::spawn().await;
    let board = stub.board(ProviderId::Ledger);

    for (sent, method) in BoardMethod::ALL.into_iter().enumerate() {
        call(&board, method).await;
        stub.wait_for(sent + 1).await;
    }

    let providers: Vec<(String, Option<Value>)> = stub
        .received()
        .into_iter()
        .filter(|request| !request.path.ends_with("/append_transcript"))
        .map(|request| {
            let body: Value = serde_json::from_slice(&request.body).expect("a JSON body");
            (
                request
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                body.get("provider").cloned(),
            )
        })
        .filter(|(_, provider)| provider.is_some())
        .collect();
    assert_eq!(
        providers,
        ["preview", "claim", "run_context"]
            .map(|method| (method.to_string(), Some(json!("ledger"))))
            .to_vec()
    );
}

// ---------------------------------------------------------------------------
// How it reads an answer
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_adapter_rebuilds_every_error_code_from_the_body_not_the_status() {
    let stub = Stub::spawn().await;
    let board = stub.board(ProviderId::ClaudeCode);
    // Each code with a status that is not its own, 200 included.
    let cases = [
        (
            "invalid",
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Invalid,
        ),
        ("not_found", StatusCode::OK, ErrorCode::NotFound),
        ("database", StatusCode::NOT_FOUND, ErrorCode::Database),
        ("io", StatusCode::BAD_REQUEST, ErrorCode::Io),
        ("internal", StatusCode::CONFLICT, ErrorCode::Internal),
        ("conflict", StatusCode::UNAUTHORIZED, ErrorCode::Conflict),
        (
            "unauthenticated",
            StatusCode::UPGRADE_REQUIRED,
            ErrorCode::Unauthenticated,
        ),
        (
            "upgrade_required",
            StatusCode::BAD_GATEWAY,
            ErrorCode::UpgradeRequired,
        ),
    ];
    assert_eq!(cases.len(), ErrorCode::ALL.len(), "every code has a case");

    for (code, status, expected) in cases {
        stub.reply(
            status,
            json!({ "code": code, "message": format!("the board says {code}") }).to_string(),
        );

        let error = board.heartbeat(&[]).await.expect_err(code);

        assert_eq!(error.code(), expected, "{code} with {status}");
        if !matches!(expected, ErrorCode::Database | ErrorCode::Unauthenticated) {
            assert_eq!(error.to_string(), format!("the board says {code}"));
        }
    }
}

#[tokio::test]
async fn a_reply_that_is_not_an_error_body_is_internal_and_names_the_status() {
    let stub = Stub::spawn().await;
    let board = stub.board(ProviderId::ClaudeCode);
    stub.reply(
        StatusCode::BAD_GATEWAY,
        "<html><body>502 Bad Gateway</body></html>",
    );

    let error = board.heartbeat(&[]).await.expect_err("a proxy's page");

    assert_eq!(error.code(), ErrorCode::Internal);
    assert_eq!(
        error.to_string(),
        format!(
            "the board at {} answered 502 Bad Gateway without an error this runner could read",
            stub.origin
        )
    );
    assert_no_token(&error);
}

#[tokio::test]
async fn an_unreachable_server_is_io_and_names_the_origin() {
    // A port nothing listens on any more.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("an address"));
    drop(listener);
    let board = HttpBoard::new(&origin, Secret::new(TOKEN), ProviderId::ClaudeCode)
        .expect("a loopback origin");

    let error = board.heartbeat(&[]).await.expect_err("nobody answers");

    assert_eq!(error.code(), ErrorCode::Io);
    assert!(
        error
            .to_string()
            .starts_with(&format!("could not reach the board at {origin}: ")),
        "{error}"
    );
    assert_no_token(&error);
}

#[tokio::test]
async fn the_runner_token_is_never_in_an_error_or_debug_output() {
    let stub = Stub::spawn().await;
    let board = stub.board(ProviderId::ClaudeCode);
    stub.reply(StatusCode::INTERNAL_SERVER_ERROR, "not an error body");

    let printed = format!("{board:?}");
    let error = board.heartbeat(&[]).await.expect_err("an unreadable reply");

    assert!(!printed.contains(TOKEN), "{printed}");
    assert!(printed.contains(&stub.origin), "{printed}");
    assert_no_token(&error);
    assert!(!format!("{error:?}").contains(TOKEN));
}

fn assert_no_token(error: &Error) {
    assert!(!error.to_string().contains(TOKEN), "{error}");
}

#[tokio::test]
async fn the_adapter_ignores_a_reply_field_it_does_not_know() {
    let stub = Stub::spawn().await;
    let board = stub.board(ProviderId::ClaudeCode);
    stub.reply(
        StatusCode::OK,
        json!({ "fenced": [], "cancel": [], "cleanup": [], "fromANewerBoard": { "x": 1 } })
            .to_string(),
    );

    let heartbeat = board.heartbeat(&[]).await.expect("an answer it can read");

    assert_eq!(heartbeat, Heartbeat::default());
}

// ---------------------------------------------------------------------------
// What it refuses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_adapter_refuses_plain_http_to_a_host_that_is_not_loopback() {
    for refused in [
        "http://rimaia.example",
        "http://10.0.0.7:8080",
        "http://localhost.example:8080",
        "ftp://rimaia.example",
    ] {
        let error =
            HttpBoard::new(refused, Secret::new(TOKEN), ProviderId::ClaudeCode).expect_err(refused);
        assert_eq!(error.code(), ErrorCode::Invalid, "{refused}");
        assert_no_token(&error);
    }

    for accepted in [
        "http://127.0.0.1:4100",
        "http://[::1]:4100",
        "http://localhost:4100",
        "https://rimaia.example",
    ] {
        HttpBoard::new(accepted, Secret::new(TOKEN), ProviderId::ClaudeCode)
            .unwrap_or_else(|error| panic!("{accepted}: {error}"));
    }
}

// ---------------------------------------------------------------------------
// Tails
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tail_published_with_a_full_buffer_is_dropped_and_publish_never_blocks() {
    let stub = Stub::spawn().await;
    let board = stub.board(ProviderId::ClaudeCode);

    // No await between them, so the drain has taken none yet: the queue
    // fills, and every tail past it is dropped on the spot.
    for _ in 0..TAIL_BUFFER + 36 {
        board.publish_tail(&lease(), tail());
    }

    stub.wait_for(TAIL_BUFFER).await;
    for _ in 0..1_000 {
        tokio::task::yield_now().await;
    }
    assert_eq!(stub.received().len(), TAIL_BUFFER);
}

// ---------------------------------------------------------------------------
// Against the real server
// ---------------------------------------------------------------------------

/// The real router over a two-team board, authenticated by the real
/// `SessionsAndTokens`.
async fn serve(teams: &TwoTeams) -> (String, tokio::task::JoinHandle<()>) {
    let (host, relayed) = testing_api::relayed_host(teams).await;
    let auth = SessionsAndTokens {
        pool: teams.a.pool.clone(),
        clock: Arc::new(teams.clock.clone()),
    };
    let state = ServerState::new(
        host,
        relayed,
        Arc::new(auth),
        Arc::new(FakeIdentityProvider::new()),
        "https://rimaia.test",
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let origin = format!("http://{}", listener.local_addr().expect("an address"));
    let router = rimaia_server::router(state);
    let task = tokio::spawn(async move {
        rimaia_server::serve(listener, router)
            .await
            .expect("the server serves");
    });
    (origin, task)
}

#[tokio::test]
async fn a_published_tail_reaches_a_subscriber_through_the_server() {
    let teams = TwoTeams::new().await;
    let (origin, server) = serve(&teams).await;
    let board = HttpBoard::new(
        &origin,
        teams.team_a.credentials.runner_token.clone(),
        ProviderId::ClaudeCode,
    )
    .expect("a loopback origin");
    let claim = board
        .claim(ClaimTarget::Run {
            task_id: teams.team_a.ready.clone(),
            trigger: RunTrigger::Queued,
            continue_session: false,
            ceiling: Default::default(),
        })
        .await
        .expect("claim")
        .expect("team A's ready task");
    let mut tails = teams.a.subscribe_tail();

    board.publish_tail(&claim.lease, tail());

    let heard = tokio::time::timeout(Duration::from_secs(30), tails.recv())
        .await
        .expect("the tail arrives")
        .expect("a tail on the board's channel");
    assert_eq!(heard.team_id, teams.team_a.team_id);
    assert_eq!(heard.tail, tail());
    server.abort();
}

#[tokio::test]
async fn the_catalogue_in_the_reply_is_the_providers_the_adapter_sends() {
    let teams = TwoTeams::new().await;
    let ledger = pairing::pair_own_runner(&teams.a, "a Ledger runner", "ledger")
        .await
        .expect("a runner paired as Ledger");
    let (origin, server) = serve(&teams).await;
    let board = HttpBoard::new(&origin, ledger.token, ProviderId::Ledger).expect("loopback");

    let context = board
        .preview(&teams.team_a.ready, PreviewOf::Run)
        .await
        .expect("a preview");

    assert_eq!(
        context.catalogue,
        catalogue_for_provider(&teams.a, &teams.team_a.team_id, ProviderId::Ledger)
            .await
            .expect("Ledger's catalogue for team A"),
    );
    assert_ne!(
        context.catalogue,
        catalogue_for_provider(&teams.a, &teams.team_a.team_id, ProviderId::ClaudeCode)
            .await
            .expect("Claude's catalogue for team A"),
        "the two providers' catalogues differ, so the answer names which"
    );
    server.abort();
}

#[tokio::test]
async fn a_run_context_whose_catalogue_carries_an_unknown_field_parses_in_http_board() {
    // A real context, read off a real board, then given the keys a newer
    // board might add.
    let teams = TwoTeams::new().await;
    let real: RunContext = teams
        .board(&teams.a)
        .preview(&teams.team_a.ready, PreviewOf::Run)
        .await
        .expect("a real context");
    let mut wire = serde_json::to_value(&real).expect("serialize");
    wire["catalogue"]["fromANewerBoard"] = json!(true);
    wire["catalogue"]["models"][0]["tier"] = json!("large");
    wire["catalogue"]["planner"]["budgetUsd"] = json!(1.5);

    let stub = Stub::spawn().await;
    stub.reply(StatusCode::OK, wire.to_string());
    let board = stub.board(ProviderId::ClaudeCode);

    let read = board
        .run_context(&lease())
        .await
        .expect("a newer board's context");

    assert_eq!(read.catalogue, real.catalogue);
}
