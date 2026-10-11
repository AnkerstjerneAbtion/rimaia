//! The runner protocol: `POST /api/v1/runner/<method>`, one route per
//! `BoardMethod` (seam-contract D31 point 10, ADR-0034 point 2, task 052).
//!
//! The board port over a network. `rimaia-runner`'s `HttpBoard` sends each
//! method here, and the handler makes **one call** into the same
//! `board::service` function `InProcessBoard` calls, so the two adapters
//! cannot come to disagree about a rule: the contract suite runs over both.
//!
//! Every handler, in order:
//!
//! 1. takes a [`RunnerCaller`] as its first extractor: a runner's `rmr_`
//!    token, and nothing else. The runner's identity comes from the token, so
//!    a `LeaseRef` copied from another runner's lease is `Conflict` at the
//!    fence (D31 point 3);
//! 2. checks `Rimaia-Protocol` as a write, reads included: a runner that
//!    could read but not report would compose and spawn a run whose result it
//!    could not deliver (ADR-0037 point 4);
//! 3. records that the runner was heard from (`board::runners::mark_seen`);
//! 4. reads the body, from bytes, so a malformed or oversized one is
//!    `invalid` in the error shape and never axum's plain text;
//! 5. verifies what the body claims about the caller: a `LeaseRef`'s team is
//!    in the runner's owner's teams (the service's own scope check), and the
//!    provider `preview`, `claim` and `run_context` name is the one the runner
//!    was paired as (`board::runners::paired_provider`);
//! 6. calls the service.
//!
//! These routes are not in D32's registry: `BoardMethod::ALL` is theirs, and
//! the one exhaustive `match` in [`route`] means a method added there does not
//! compile until it has a route here. They sit outside the board routes' CORS
//! layer, because a runner is not a browser.

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{post, MethodRouter};
use axum::{Json, Router};
use rimaia_core::api::{protocol, Effect};
use rimaia_core::board::service::{self, Runner};
use rimaia_core::board::wire::{
    ClaimBody, FinishRunBody, HeartbeatBody, LeaseBody, PreviewBody, PublishTailBody,
    RecordBranchBody, RecordReviewFindingsBody, RecordStrategyBody, RunContextBody, StartRunBody,
    TranscriptQuery,
};
use rimaia_core::board::{
    relay, runners, BoardMethod, Claim, FinishReceipt, Heartbeat, LeaseTerm, RunContext,
    TranscriptAck,
};
use rimaia_core::runner::provider::{ProviderId, ProviderProfile};
use rimaia_core::{Error, ServiceContext};
use serde::de::DeserializeOwned;
use tower_http::trace::TraceLayer;

use crate::caller::RunnerCaller;
use crate::{http_span, ApiError, ProtocolHeader, ServerState};

/// The most bytes a runner route reads of a body: axum's default
/// `DefaultBodyLimit`, which every runner route keeps, `finish_run` included
/// (seam-contract D34). Task 056 raises it on `append_transcript` alone.
pub const BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// One route per `BoardMethod`, answering an unknown method `404 not_found`
/// and any verb but `POST` `400 invalid`, both in the error shape.
pub(crate) fn routes() -> Router<ServerState> {
    let mut router = Router::new();
    for method in BoardMethod::ALL {
        router = router.route(&format!("/{}", method.as_str()), route(method));
    }
    router
        .route_layer(TraceLayer::new_for_http().make_span_with(http_span::<axum::body::Body>))
        .fallback(|| async { ApiError(Error::not_found("there is no such runner method")) })
        .method_not_allowed_fallback(|| async {
            ApiError(Error::invalid("a runner method is sent with POST"))
        })
}

/// The handler of `method`. Exhaustive, so a method added to the port has no
/// route until it is given one here.
fn route(method: BoardMethod) -> MethodRouter<ServerState> {
    match method {
        BoardMethod::Preview => post(preview),
        BoardMethod::Claim => post(claim),
        BoardMethod::Heartbeat => post(heartbeat),
        BoardMethod::RunContext => post(run_context),
        BoardMethod::RecordBranch => post(record_branch),
        BoardMethod::StartRun => post(start_run),
        BoardMethod::AppendTranscript => post(append_transcript),
        BoardMethod::PublishTail => post(publish_tail),
        BoardMethod::FinishRun => post(finish_run),
        BoardMethod::Release => post(release),
        BoardMethod::RecordStrategy => post(record_strategy),
        BoardMethod::RecordReviewFindings => post(record_review_findings),
    }
}

/// A runner's request after its token and its protocol version were
/// accepted: its context and its id.
struct Remote {
    /// Every team the runner's owner belongs to, read on this request
    /// (ADR-0030 point 2), acting as the owner, sourced as `System` (D32
    /// point 7). A method under a lease narrows it to the lease's team in the
    /// service, after checking the team is in it.
    ctx: ServiceContext,
    runner_id: String,
    term: LeaseTerm,
}

impl Remote {
    /// Steps 2 and 3 of the module's list.
    async fn begin(
        caller: &RunnerCaller,
        ProtocolHeader(header): ProtocolHeader,
        state: &ServerState,
    ) -> Result<Self, Error> {
        protocol::check(header.as_deref(), Effect::Write)?;
        let ctx = state.host.context.for_caller(caller.caller())?;
        let runner_id = caller.runner_id().to_string();
        runners::mark_seen(&ctx, &runner_id).await?;
        Ok(Self {
            ctx,
            runner_id,
            term: state.lease_term,
        })
    }

    /// The runner's paired provider, held against the one the body names.
    async fn provider(&self, claimed: Option<ProviderId>) -> Result<ProviderProfile, Error> {
        let paired = runners::paired_provider(&self.ctx, &self.runner_id, claimed).await?;
        Ok(ProviderProfile::for_id(paired))
    }

    fn runner<'a>(&'a self, provider: &'a ProviderProfile) -> Runner<'a> {
        Runner {
            id: &self.runner_id,
            provider,
            term: self.term,
        }
    }
}

/// A JSON body, read from bytes.
fn body<T: DeserializeOwned>(body: Result<Bytes, BytesRejection>) -> Result<T, Error> {
    let body = body.map_err(rejected)?;
    serde_json::from_slice(&body)
        .map_err(|error| Error::invalid(format!("the request body could not be read: {error}")))
}

/// axum's rejection of a body, in the error shape. One over the limit names
/// the limit, so a runner's log says why rather than a bare 413.
fn rejected(rejection: BytesRejection) -> Error {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        Error::invalid(format!(
            "the request body is larger than the {BODY_LIMIT_BYTES} bytes a runner route accepts"
        ))
    } else {
        Error::invalid(rejection.body_text())
    }
}

type Answer<T> = Result<Json<T>, ApiError>;

async fn preview(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<RunContext> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let PreviewBody {
        task_id,
        of,
        provider,
    } = body(request)?;
    let provider = remote.provider(Some(provider)).await?;
    let runner = remote.runner(&provider);
    Ok(Json(
        service::preview(&remote.ctx, runner, &task_id, of).await?,
    ))
}

/// `Next` goes through the relay, so this runner's held Run now comes first
/// (task 052); every other target is the service's claim.
async fn claim(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<Option<Claim>> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let ClaimBody { target, provider } = body(request)?;
    let provider = remote.provider(Some(provider)).await?;
    let runner = remote.runner(&provider);
    Ok(Json(
        relay::claim_next(&remote.ctx, runner, &state.relayed, target).await?,
    ))
}

async fn heartbeat(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<Heartbeat> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let HeartbeatBody { held } = body(request)?;
    let provider = remote.provider(None).await?;
    let runner = remote.runner(&provider);
    Ok(Json(
        service::heartbeat(&remote.ctx, runner, &held, state.relayed.cancels()).await?,
    ))
}

async fn run_context(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<RunContext> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let RunContextBody { lease, provider } = body(request)?;
    let provider = remote.provider(Some(provider)).await?;
    let runner = remote.runner(&provider);
    Ok(Json(
        service::run_context(&remote.ctx, runner, &lease).await?,
    ))
}

async fn record_branch(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<()> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let RecordBranchBody { lease, branch } = body(request)?;
    service::record_branch(&remote.ctx, &remote.runner_id, &lease, &branch).await?;
    Ok(Json(()))
}

async fn start_run(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<()> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let StartRunBody { lease, run } = body(request)?;
    service::start_run(
        &remote.ctx,
        &state.host.paths,
        &remote.runner_id,
        &lease,
        run,
    )
    .await?;
    Ok(Json(()))
}

/// The one route whose body is not JSON (ADR-0034 point 2): the raw bytes,
/// with the lease, the run and the offset in the query string. Until task 056
/// it acknowledges through the service exactly as the in-process adapter
/// does, and keeps nothing.
async fn append_transcript(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    query: Result<Query<TranscriptQuery>, QueryRejection>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<TranscriptAck> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let Query(query) = query.map_err(|rejection| Error::invalid(rejection.body_text()))?;
    let bytes = request.map_err(rejected)?;
    Ok(Json(
        service::append_transcript(
            &remote.ctx,
            &remote.runner_id,
            &query.lease(),
            query.chunk(bytes.to_vec()),
        )
        .await?,
    ))
}

/// `null` whether the tail was relayed or dropped: a tail cannot fail (D14).
async fn publish_tail(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<()> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let PublishTailBody { lease, tail } = body(request)?;
    service::publish_remote_tail(&remote.ctx, &remote.runner_id, &lease, tail).await;
    Ok(Json(()))
}

async fn finish_run(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<FinishReceipt> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let FinishRunBody {
        lease,
        run_id,
        finish,
    } = body(request)?;
    let provider = remote.provider(None).await?;
    let runner = remote.runner(&provider);
    Ok(Json(
        service::finish_run(&remote.ctx, runner, &lease, &run_id, finish).await?,
    ))
}

async fn release(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<()> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let LeaseBody { lease } = body(request)?;
    service::release(&remote.ctx, &remote.runner_id, &lease).await?;
    Ok(Json(()))
}

async fn record_strategy(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<()> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let RecordStrategyBody { lease, plan } = body(request)?;
    service::record_strategy(&remote.ctx, &remote.runner_id, &lease, plan).await?;
    Ok(Json(()))
}

async fn record_review_findings(
    caller: RunnerCaller,
    header: ProtocolHeader,
    State(state): State<ServerState>,
    request: Result<Bytes, BytesRejection>,
) -> Answer<()> {
    let remote = Remote::begin(&caller, header, &state).await?;
    let RecordReviewFindingsBody {
        lease,
        run_id,
        findings,
    } = body(request)?;
    service::record_review_findings(&remote.ctx, &remote.runner_id, &lease, &run_id, findings)
        .await?;
    Ok(Json(()))
}
