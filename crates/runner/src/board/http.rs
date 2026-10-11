//! The board port over HTTP: the second adapter (ADR-0027 point 5,
//! seam-contract D31 point 10, task 052).
//!
//! Every method is `POST <origin>/api/v1/runner/<BoardMethod::as_str()>`,
//! answered by `rimaia-server`'s runner routes, which call the same
//! `board::service` functions `InProcessBoard` calls. The contract suite runs
//! over both, so a rule either adapter decided for itself would fail it. This
//! adapter decides nothing: it encodes, sends and decodes.
//!
//! - **Who it is.** A runner's `rmr_` token, on every request as
//!   `Authorization: Bearer`, beside `Rimaia-Protocol`. The token never
//!   appears in a `Debug`, an error message, a span or a query string.
//! - **What it runs.** Its `ProviderId`, added to the `preview`, `claim` and
//!   `run_context` bodies, which the server holds against the provider the
//!   runner was paired as.
//! - **Errors** are rebuilt from the body's `code`, never from the status: the
//!   status is for proxies, and a proxy's own error page carries no `code`.
//! - **No timeout and no retry.** Task 053 gives every method its timeout,
//!   the waiting claim's included; task 056's outbox resends reports, and
//!   callers ask again for reads.

use std::fmt;
use std::io;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::{StatusCode, Url};
use rimaia_core::api::{protocol, PROTOCOL_VERSION};
use rimaia_core::board::wire::{
    ClaimBody, FinishRunBody, HeartbeatBody, LeaseBody, PreviewBody, PublishTailBody,
    RecordBranchBody, RecordReviewFindingsBody, RecordStrategyBody, RunContextBody, StartRunBody,
    TranscriptQuery,
};
use rimaia_core::board::{
    BoardFuture, BoardMethod, BoardPort, Claim, ClaimTarget, FinishReceipt, FinishRun, Heartbeat,
    LeaseRef, PreviewOf, RunContext, StartRun, TranscriptAck, TranscriptChunk,
};
use rimaia_core::identity::secret::Secret;
use rimaia_core::review::findings::NewReviewFinding;
use rimaia_core::runner::events::RunTail;
use rimaia_core::runner::provider::ProviderId;
use rimaia_core::tasks::strategy::StrategyPlan;
use rimaia_core::{Error, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// How many tails wait to be posted before a new one is dropped. A tail is a
/// live snapshot whose loss costs nothing (D14), so a slow network sheds
/// tails rather than holding the run up behind them.
pub const TAIL_BUFFER: usize = 64;

const JSON: &str = "application/json";
const BYTES: &str = "application/octet-stream";

/// A runner's port onto a board on the other end of a network.
pub struct HttpBoard {
    remote: Remote,
    provider: ProviderId,
    tails: mpsc::Sender<PublishTailBody>,
}

/// What every request needs: the client, where the board is, and who is
/// asking. Cloned into the tail drain.
#[derive(Clone)]
struct Remote {
    client: reqwest::Client,
    origin: Url,
    token: Secret,
}

impl HttpBoard {
    /// A port onto the board at `origin`, as the runner `token` was issued to,
    /// running `provider`.
    ///
    /// **Call it inside a Tokio runtime:** it spawns the task that posts
    /// published tails one at a time, which ends when the adapter is dropped.
    ///
    /// The token is never sent in clear: an `http://` origin is refused unless
    /// its host is loopback, an IP address for which `is_loopback` holds or
    /// the name `localhost`, which is how a test harness and a server on the
    /// same machine are reached.
    ///
    /// Proxies in the environment (`HTTP_PROXY`, `ALL_PROXY` and the rest) are
    /// ignored for that loopback `http://` origin: a proxy would receive the
    /// token in clear, and reqwest has no loopback bypass unless `NO_PROXY`
    /// names it. An `https://` origin keeps them, because a proxy tunnels TLS
    /// with `CONNECT` and the token stays encrypted end to end.
    pub fn new(origin: &str, token: Secret, provider: ProviderId) -> Result<Self> {
        let origin = parse_origin(origin)?;
        let mut client = reqwest::Client::builder();
        if origin.scheme() == "http" {
            client = client.no_proxy();
        }
        let client = client.build().map_err(|error| {
            Error::internal(format!(
                "could not build an HTTP client for {origin}: {error}"
            ))
        })?;
        let remote = Remote {
            client,
            origin,
            token,
        };
        let (tails, queued) = mpsc::channel(TAIL_BUFFER);
        tokio::spawn(drain_tails(remote.clone(), queued));
        Ok(Self {
            remote,
            provider,
            tails,
        })
    }

    async fn json<B: Serialize, T: DeserializeOwned>(
        &self,
        method: BoardMethod,
        body: &B,
    ) -> Result<T> {
        self.remote.json(method, body).await
    }
}

impl fmt::Debug for HttpBoard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpBoard")
            .field("origin", &self.remote.origin.as_str())
            .field("token", &self.remote.token)
            .field("provider", &self.provider)
            .finish()
    }
}

/// `origin` as a base URL, or why it cannot be one.
fn parse_origin(origin: &str) -> Result<Url> {
    let mut url = Url::parse(origin)
        .map_err(|error| Error::invalid(format!("{origin:?} is not a board origin: {error}")))?;
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(&url) => {}
        "http" => {
            return Err(Error::invalid(format!(
                "{origin} is plain HTTP to a host that is not this machine, and a runner's token \
                 is never sent in clear; use https://"
            )))
        }
        scheme => {
            return Err(Error::invalid(format!(
                "{origin:?} is not a board origin: its scheme is {scheme:?}, not https"
            )))
        }
    }
    // A base that ends in `/`, so every route joins onto it rather than
    // replacing its last segment.
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // An IPv6 host is written in brackets in a URL.
    let address = host.trim_start_matches('[').trim_end_matches(']');
    address
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

impl Remote {
    fn url(&self, method: BoardMethod) -> Url {
        self.origin
            .join(&format!("api/v1/runner/{}", method.as_str()))
            .expect("a route name joins onto an origin")
    }

    async fn json<B: Serialize, T: DeserializeOwned>(
        &self,
        method: BoardMethod,
        body: &B,
    ) -> Result<T> {
        let body = serde_json::to_vec(body).map_err(|error| {
            Error::internal(format!(
                "could not encode a {} request: {error}",
                method.as_str()
            ))
        })?;
        self.send(self.url(method), JSON, body).await
    }

    /// One request, read whole. The answer is a success's body, or the error
    /// its body names.
    async fn send<T: DeserializeOwned>(
        &self,
        url: Url,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<T> {
        let origin = self.origin.as_str().trim_end_matches('/');
        let unreachable = |error: reqwest::Error| {
            Error::Io(io::Error::other(format!(
                "could not reach the board at {origin}: {}",
                error.without_url()
            )))
        };
        let response = self
            .client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token.expose()))
            .header(protocol::HEADER, PROTOCOL_VERSION)
            .header(CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .map_err(unreachable)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(unreachable)?;
        answer(origin, status, &bytes)
    }
}

/// What a reply means: the error its body names, whatever its status; or, on
/// a success, the value it carries; or, for anything else, `Internal` naming
/// the status and the origin, such as a proxy's HTML page.
fn answer<T: DeserializeOwned>(origin: &str, status: StatusCode, body: &[u8]) -> Result<T> {
    if let Ok(error) = serde_json::from_slice::<WireError>(body) {
        return Err(error.into());
    }
    if status.is_success() {
        return serde_json::from_slice(body).map_err(|error| {
            Error::internal(format!(
                "the board at {origin} answered {status} with a body this runner could not read: \
                 {error}"
            ))
        });
    }
    Err(Error::internal(format!(
        "the board at {origin} answered {status} without an error this runner could read"
    )))
}

/// D8's `{ code, message }`, as a reply carries it.
#[derive(Deserialize)]
struct WireError {
    code: WireCode,
    message: String,
}

/// `ErrorCode`'s wire spelling, read back.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireCode {
    Database,
    Io,
    NotFound,
    Invalid,
    Conflict,
    Unauthenticated,
    UpgradeRequired,
    Internal,
}

impl From<WireError> for Error {
    fn from(error: WireError) -> Self {
        let WireError { code, message } = error;
        match code {
            WireCode::Database => Error::Database(sqlx::Error::Protocol(message)),
            WireCode::Io => Error::Io(io::Error::other(message)),
            WireCode::NotFound => Error::not_found(message),
            WireCode::Invalid => Error::invalid(message),
            WireCode::Conflict => Error::conflict(message),
            // One sentence, whatever the body said (task 047).
            WireCode::Unauthenticated => Error::unauthenticated(),
            WireCode::UpgradeRequired => Error::upgrade_required(message),
            WireCode::Internal => Error::internal(message),
        }
    }
}

/// Posts each published tail in turn until the adapter is dropped. A post
/// that fails is dropped: a tail is never resent (D14).
async fn drain_tails(remote: Remote, mut queued: mpsc::Receiver<PublishTailBody>) {
    while let Some(tail) = queued.recv().await {
        if let Err(error) = remote.json::<_, ()>(BoardMethod::PublishTail, &tail).await {
            tracing::debug!(%error, "a tail could not be posted and was dropped");
        }
    }
}

impl BoardPort for HttpBoard {
    fn preview<'a>(&'a self, task_id: &'a str, of: PreviewOf) -> BoardFuture<'a, RunContext> {
        Box::pin(async move {
            self.json(
                BoardMethod::Preview,
                &PreviewBody {
                    task_id: task_id.to_string(),
                    of,
                    provider: self.provider,
                },
            )
            .await
        })
    }

    fn claim<'a>(&'a self, target: ClaimTarget) -> BoardFuture<'a, Option<Claim>> {
        Box::pin(async move {
            self.json(
                BoardMethod::Claim,
                &ClaimBody {
                    target,
                    provider: self.provider,
                },
            )
            .await
        })
    }

    fn heartbeat<'a>(&'a self, held: &'a [LeaseRef]) -> BoardFuture<'a, Heartbeat> {
        Box::pin(async move {
            self.json(
                BoardMethod::Heartbeat,
                &HeartbeatBody {
                    held: held.to_vec(),
                },
            )
            .await
        })
    }

    fn run_context<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, RunContext> {
        Box::pin(async move {
            self.json(
                BoardMethod::RunContext,
                &RunContextBody {
                    lease: lease.clone(),
                    provider: self.provider,
                },
            )
            .await
        })
    }

    fn record_branch<'a>(&'a self, lease: &'a LeaseRef, branch: &'a str) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            self.json(
                BoardMethod::RecordBranch,
                &RecordBranchBody {
                    lease: lease.clone(),
                    branch: branch.to_string(),
                },
            )
            .await
        })
    }

    fn start_run<'a>(&'a self, lease: &'a LeaseRef, run: StartRun) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            self.json(
                BoardMethod::StartRun,
                &StartRunBody {
                    lease: lease.clone(),
                    run,
                },
            )
            .await
        })
    }

    /// The bytes as the body, the lease, the run and the offset in the query
    /// string (ADR-0034 point 2).
    fn append_transcript<'a>(
        &'a self,
        lease: &'a LeaseRef,
        chunk: TranscriptChunk,
    ) -> BoardFuture<'a, TranscriptAck> {
        Box::pin(async move {
            let query = TranscriptQuery::new(lease, &chunk.run_id, chunk.offset);
            let mut url = self.remote.url(BoardMethod::AppendTranscript);
            url.query_pairs_mut()
                .append_pair("taskId", &query.task_id)
                .append_pair("teamId", &query.team_id)
                .append_pair("generation", &query.generation.to_string())
                .append_pair("runId", &query.run_id)
                .append_pair("offset", &query.offset.to_string());
            self.remote.send(url, BYTES, chunk.bytes).await
        })
    }

    /// Queued without waiting, and dropped when the queue is full: it can
    /// neither block nor fail (D14, D31 point 4).
    fn publish_tail(&self, lease: &LeaseRef, tail: RunTail) {
        let queued = self.tails.try_send(PublishTailBody {
            lease: lease.clone(),
            tail,
        });
        if queued.is_err() {
            tracing::debug!(task_id = %lease.task_id, "the tail queue is full; a tail was dropped");
        }
    }

    fn finish_run<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        finish: FinishRun,
    ) -> BoardFuture<'a, FinishReceipt> {
        Box::pin(async move {
            self.json(
                BoardMethod::FinishRun,
                &FinishRunBody {
                    lease: lease.clone(),
                    run_id: run_id.to_string(),
                    finish,
                },
            )
            .await
        })
    }

    fn release<'a>(&'a self, lease: &'a LeaseRef) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            self.json(
                BoardMethod::Release,
                &LeaseBody {
                    lease: lease.clone(),
                },
            )
            .await
        })
    }

    fn record_strategy<'a>(
        &'a self,
        lease: &'a LeaseRef,
        plan: StrategyPlan,
    ) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            self.json(
                BoardMethod::RecordStrategy,
                &RecordStrategyBody {
                    lease: lease.clone(),
                    plan,
                },
            )
            .await
        })
    }

    fn record_review_findings<'a>(
        &'a self,
        lease: &'a LeaseRef,
        run_id: &'a str,
        findings: Vec<NewReviewFinding>,
    ) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            self.json(
                BoardMethod::RecordReviewFindings,
                &RecordReviewFindingsBody {
                    lease: lease.clone(),
                    run_id: run_id.to_string(),
                    findings,
                },
            )
            .await
        })
    }
}
