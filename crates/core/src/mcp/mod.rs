//! The embedded local MCP server (ADR-0006), so Claude Code sessions the user is
//! already running can read and write the board.
//!
//! It lives in core, not in the shell, precisely so its tool handlers are thin
//! adapters over the same services the Tauri commands call. A rule enforced in
//! only one of the two paths is a bug — the same invariant must produce the same
//! rejection whichever door it comes through.
//!
//! [`settings`] owns the port key, [`requests`] and [`responses`] are the wire
//! DTOs, [`server`] holds the tool handlers, [`scope`] says which of them
//! each door may reach, and [`build`] binds the listener.
//!
//! # Two doors, one service layer
//!
//! [`MCP_PATH`] is the operator's, registered once with `claude mcp add` and
//! fixed by ADR-0006. Since task 020 there is a second, `/mcp/run/{token}`,
//! minted per run and revoked when the run ends — see [`scope`] for what it is
//! for and what it deliberately is not. Both build the same [`RimaiaServer`]
//! over the same [`ServiceContext`]; the only difference is the value of its
//! `scope` field.
//!
//! # Board tools in core, machine tools injected by the host
//!
//! Since task 041 the server's tools are two routers. The board router needs
//! only a `ServiceContext`; the local router holds the tools that inspect,
//! reconfigure or spawn on this machine (ADR-0035 point 6), and is served only
//! when [`build`] is handed a [`LocalTools`]. The shell passes `Some`, so solo
//! serves every tool it served before; a server with no machine passes `None`
//! and serves no machine tool at all.
//!
//! # Loopback is not configurable
//!
//! [`build`] binds `127.0.0.1` as a literal. ADR-0006 makes the *port*
//! configurable for a collision and the *interface* not configurable at all,
//! because the trust boundary this server has — anything on this machine that
//! can reach loopback can drive it — is only defensible while it is loopback.
//! `the_server_binds_loopback_and_not_a_public_interface` is that sentence as a
//! test.
//!
//! # A busy port does not stop the app
//!
//! [`build`] is infallible and status-carrying rather than fallible
//! (seam-contract D16). Seam-contract D11's "startup fails loudly" argument
//! does not transfer: there is no useful UI over a half-migrated database, but
//! the remedy for a taken port — Settings → MCP — lives behind the very window
//! a fatal bind would refuse to open. One call shape also spares the shell a
//! match on a `Result` *and* a match on the status.
//!
//! # Stateless transport
//!
//! `legacy_session_mode: false` and `json_response: true`. A process that runs
//! all night has no business holding per-session state for a client that may
//! have gone away hours ago, and with no long-lived SSE stream to drain,
//! `axum::serve`'s graceful shutdown is sufficient on its own.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use rmcp::model::ProtocolVersion;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::context::{ServiceContext, TeamScope};
use crate::db::MutationSource;
use crate::error::{Error, Result};
use crate::runner::provider::AgentProvider;

pub mod error;
pub mod requests;
pub mod responses;
pub mod scope;
pub mod server;
pub mod settings;

pub use error::ToolError;
pub use scope::{Grant, GrantKind, RunAccess, RunGrant, RunHandles, RunScope, Tool};
pub use server::{LocalTools, RimaiaServer};
pub use settings::{configured_port, set_configured_port, MCP_PORT};

use scope::RUN_ROUTE_PREFIX;

/// The port ADR-0006 fixes as the default, and the one every `claude mcp add`
/// line in the docs uses. Configurable for a collision; the *interface* is not
/// configurable and is hard-coded to loopback.
pub const DEFAULT_PORT: u16 = 4517;

/// The **operator** surface's name: what the operator registers with
/// `claude mcp add`, what the handshake reports on `/mcp`, and the name the
/// denial every spawned run carries is spelled at (`runner::process`), so a
/// tool there wears `mcp__rimaia__<tool>`.
///
/// Never the name of a run's own handle (seam-contract D30 point 1). A run that
/// inherits the operator's configuration holds this registration and its own
/// handle at once, and the denial works by tool name, so the two must not share
/// one: if they did, denying the operator surface would deny the handle too, and
/// the only way out would be to drop the denial for exactly the runs that carry
/// a handle.
pub const MCP_SERVER_NAME: &str = "rimaia";

/// The **run-scoped** handle's name (D30 point 1): the key of the
/// `--mcp-config` document a run is handed, what the handshake reports on
/// `/mcp/run/{token}`, the server segment of every `required_tools` entry, and
/// the tool names a prompt tells a run to call. Nothing ever registers it in a
/// user's configuration.
pub const RUN_MCP_SERVER_NAME: &str = "rimaia-run";

/// The path the streamable-HTTP endpoint is mounted at, so
/// `http://127.0.0.1:4517/mcp` is what a user registers.
pub const MCP_PATH: &str = "/mcp";

/// Whether the server is reachable, and if not, why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub state: McpState,
    /// What the port *should* be, read from settings. Disagrees with
    /// `bound_address` in precisely the case Settings → MCP exists to explain,
    /// which is why the panel builds every URL from the address and never from
    /// this.
    pub configured_port: u16,
    /// `"127.0.0.1:4517"`. `Some` only while listening.
    pub bound_address: Option<String>,
    /// The operating system's own words about a failed bind, verbatim, plus
    /// the remedy. `None` when nothing went wrong.
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpState {
    Listening,
    /// Something else already holds the configured port. Not fatal — see this
    /// module's header.
    PortInUse,
    /// Not running, for any other reason: a bind that failed some other way,
    /// or a handle whose task has been shut down.
    Stopped,
}

/// What Test connection measured.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpProbe {
    pub endpoint: String,
    pub latency_ms: u64,
    pub server_name: String,
    pub protocol_version: String,
    pub tool_count: usize,
}

/// The server's control surface. Cheap to clone; every clone reports on and
/// shuts down the same listener.
#[derive(Clone)]
pub struct McpHandle {
    shared: Arc<Shared>,
}

/// The server itself. Spawn [`run`](McpTask::run) once, and only once.
pub struct McpTask {
    /// `None` when the bind failed — [`run`](McpTask::run) then returns
    /// immediately, so the shell spawns it unconditionally and does not have to
    /// branch on the status a second time.
    listener: Option<TcpListener>,
    /// `None` alongside `listener`, and for the same reason.
    routes: Option<Routes>,
    shutdown: watch::Receiver<bool>,
}

/// Both doors: the operator's service, built once, and everything the scoped
/// route needs to build one per request.
struct Routes {
    operator: StreamableHttpService<RimaiaServer, LocalSessionManager>,
    run: RunRoute,
}

/// What `/mcp/run/{token}` needs on every request — the context to build a
/// scoped server from, and the table that says which task a token means.
#[derive(Clone)]
struct RunRoute {
    ctx: ServiceContext,
    handles: RunHandles,
    provider: Arc<dyn AgentProvider>,
    /// Carried even though every local tool is `Refused` to a run: the scoped
    /// server is the *same type* as the operator's and offers the same list
    /// (`tools/list` is not filtered by scope, `scope.rs`'s header), and the
    /// refusal comes from [`RunScope::authorize`] rather than from the tool
    /// being absent. A scope enforced by a missing router would be a second
    /// mechanism, and a run would read an unknown tool where today it reads
    /// the refusal's sentence.
    local: Option<LocalTools>,
}

struct Shared {
    status: McpStatus,
    shutdown: watch::Sender<bool>,
}

/// Binds the server and hands back the handle to keep and the task to spawn.
///
/// The same split as `rimaia_runner::queue::build`, for the same reason: the caller owns
/// the runtime, and the handle has to exist before the task does so the shell
/// can wire a command to it inside one `setup()` hook.
///
/// Binds **eagerly**, so [`McpHandle::status`] is truthful the instant this
/// returns rather than at some point after the task is spawned. Pass `0` for an
/// OS-chosen port, which is what makes this testable without fighting over
/// 4517.
///
/// Infallible: see this module's header on why a busy port is surfaced rather
/// than fatal.
///
/// `handles` is built by the shell before either subsystem and shared with the
/// runner, so this function can tell it where the server actually landed on
/// every bind — including the rebind `set_mcp_port` performs at runtime. That
/// is what makes a scoped URL truthful and what removes the ordering constraint
/// between `rimaia_runner::queue::build` and this one (seam-contract D17.4).
///
/// `provider` is the agent CLI whose catalogue the board tools read (see
/// [`RimaiaServer`]'s field). `local` is this machine's tools, served on both
/// doors when `Some`. `None` serves the board router alone: what task 046's
/// server and task 060's hosted `/mcp` pass, where a local tool is an unknown
/// tool.
pub async fn build(
    ctx: ServiceContext,
    port: u16,
    handles: RunHandles,
    provider: Arc<dyn AgentProvider>,
    local: Option<LocalTools>,
) -> (McpHandle, McpTask) {
    // Every write this server makes is an agent's, not the user's (ADR-0019).
    // Re-sourced here, once, so no handler has to remember.
    let ctx = ctx.with_source(MutationSource::Mcp);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let bind = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await;

    let (status, listener, operator) = match bind {
        Ok(listener) => {
            let bound = listener
                .local_addr()
                .ok()
                .map(|address| address.to_string());
            tracing::info!(address = ?bound, "the MCP server is listening");
            (
                McpStatus {
                    state: McpState::Listening,
                    configured_port: port,
                    bound_address: bound,
                    message: None,
                },
                Some(listener),
                Some(streamable_service(
                    ctx.clone(),
                    provider.clone(),
                    local.clone(),
                )),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            let message = port_in_use_message(port);
            tracing::error!(port, %error, "{message}");
            (
                McpStatus {
                    state: McpState::PortInUse,
                    configured_port: port,
                    bound_address: None,
                    message: Some(message),
                },
                None,
                None,
            )
        }
        Err(error) => {
            tracing::error!(port, %error, "the MCP server could not bind");
            (
                McpStatus {
                    state: McpState::Stopped,
                    configured_port: port,
                    bound_address: None,
                    message: Some(format!(
                        "the MCP server could not start on port {port}: {error}"
                    )),
                },
                None,
                None,
            )
        }
    };

    // On every bind, and on every failure to bind. A runner holding these
    // handles must never be left pointing at a port nothing answers, and
    // `None` is what makes it refuse to start a planner rather than start one
    // that cannot reply.
    handles.set_endpoint(
        status
            .bound_address
            .as_ref()
            .map(|address| format!("http://{address}")),
    );

    let routes = operator.map(|operator| Routes {
        operator,
        run: RunRoute {
            ctx,
            handles,
            provider,
            local,
        },
    });

    (
        McpHandle {
            shared: Arc::new(Shared {
                status,
                shutdown: shutdown_tx,
            }),
        },
        McpTask {
            listener,
            routes,
            shutdown: shutdown_rx,
        },
    )
}

impl McpHandle {
    /// What the panel renders and what the log line said at startup.
    ///
    /// A cached snapshot, not a live check: it is truthful about the bind,
    /// which is the thing that fails. The one hole — an axum task that died
    /// after a successful bind — is what Test connection is for, and the panel
    /// presents that as the only live check.
    pub fn status(&self) -> McpStatus {
        self.shared.status.clone()
    }

    /// `http://127.0.0.1:4517/mcp`, built from the address actually bound.
    /// `None` when nothing is listening — there is no URL to offer, and
    /// offering the configured one would be a lie in exactly the case that
    /// matters.
    pub fn url(&self) -> Option<String> {
        self.shared
            .status
            .bound_address
            .as_ref()
            .map(|address| format!("http://{address}{MCP_PATH}"))
    }

    /// Stops accepting connections.
    ///
    /// Synchronous and infallible, exactly like `QueueHandle::shutdown` and for
    /// the same reason: it is called from an exit path, where an `await` is one
    /// more thing that can fail to happen.
    pub fn shutdown(&self) {
        let _ = self.shared.shutdown.send(true);
    }
}

impl McpTask {
    /// Serves until [`McpHandle::shutdown`]. Returns immediately if the bind
    /// failed.
    pub async fn run(self) {
        let (Some(listener), Some(routes)) = (self.listener, self.routes) else {
            return;
        };

        let router = axum::Router::new()
            // Untouched: this is the URL in every `claude mcp add` line ever
            // pasted, and ADR-0006 fixes it.
            .nest_service(MCP_PATH, routes.operator)
            .route(&run_route_path(), any(dispatch))
            .with_state(routes.run);
        let mut shutdown = self.shutdown;

        let served = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                // `changed()` errs only when every sender is gone, which means
                // the handle was dropped — indistinguishable, here, from being
                // asked to stop.
                let _ = shutdown.changed().await;
            })
            .await;

        if let Err(error) = served {
            tracing::error!(%error, "the MCP server stopped serving");
        }
    }
}

/// The axum path the scoped route is registered at — `/mcp/run/{token}`.
///
/// Built from [`MCP_PATH`] and the same prefix constant
/// [`RunHandles::endpoint_for`] builds its URL from, so the route the server
/// listens on and the URL a run is handed cannot drift apart.
fn run_route_path() -> String {
    format!("{MCP_PATH}{RUN_ROUTE_PREFIX}{{token}}")
}

/// The scoped route: a token in the path, and a server value carrying the task
/// it resolves to.
///
/// A fresh [`StreamableHttpService`] per request rather than one cached per
/// token, because the transport is stateless (see this module's header) and a
/// run makes a handful of calls in its life. A cache keyed by token would be a
/// second place that has to remember revocation, and forgetting there would
/// keep a dead run's handle alive.
async fn dispatch(
    State(route): State<RunRoute>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    let Some((scope @ RunScope::Run { .. }, team_id)) = route.handles.resolve_with_team(&token)
    else {
        // A bare 404 with no body. An unknown token and a revoked one must be
        // indistinguishable, and neither may hint that some *other* token would
        // have worked — this route is not an oracle for which runs exist.
        return StatusCode::NOT_FOUND.into_response();
    };

    // Every call through the handle runs under its task's one team (ADR-0029
    // point 5), whatever the operator's context reaches.
    scoped_service(
        route.ctx.with_scope(TeamScope::one(team_id)),
        route.provider.clone(),
        route.local.clone(),
        scope,
    )
    .handle(request)
    .await
    .into_response()
}

/// The tower service [`build`] and the in-process tests mount at [`MCP_PATH`].
///
/// `pub(crate)` so a test can drive one JSON-RPC request through the real
/// transport without a socket, and so neither can drift onto a different
/// configuration than the other.
pub(crate) fn streamable_service(
    ctx: ServiceContext,
    provider: Arc<dyn AgentProvider>,
    local: Option<LocalTools>,
) -> StreamableHttpService<RimaiaServer, LocalSessionManager> {
    service_over(move || RimaiaServer::new(ctx.clone(), provider.clone(), local.clone()))
}

/// The same transport, serving one run's scoped view of the same services.
fn scoped_service(
    ctx: ServiceContext,
    provider: Arc<dyn AgentProvider>,
    local: Option<LocalTools>,
    scope: RunScope,
) -> StreamableHttpService<RimaiaServer, LocalSessionManager> {
    service_over(move || {
        RimaiaServer::scoped(ctx.clone(), provider.clone(), local.clone(), scope.clone())
    })
}

/// One transport configuration, so the operator's door and a run's cannot
/// answer to different rules about sessions or response framing.
fn service_over<F>(server: F) -> StreamableHttpService<RimaiaServer, LocalSessionManager>
where
    F: Fn() -> RimaiaServer + Send + Sync + 'static,
{
    let mut config = StreamableHttpServerConfig::default();
    // Stateless: see this module's header.
    config.legacy_session_mode = false;
    config.json_response = true;

    StreamableHttpService::new(
        move || Ok(server()),
        Arc::new(LocalSessionManager::default()),
        config,
    )
}

/// One real MCP `initialize` + `tools/list` round trip against a bound address.
///
/// Uses rmcp's own client, so the probe cannot disagree with the server about
/// the wire format — which is the whole value of a Test connection button over
/// a `TcpStream::connect` that proves only that something is listening.
///
/// It runs in Rust rather than as a `fetch` from the frontend deliberately: a
/// request from `tauri://localhost` is cross-origin, and answering it would
/// mean putting CORS on this server — widening ADR-0006's trust boundary from
/// *processes on this machine* to *any browser tab on this machine*.
pub async fn probe(address: &str) -> Result<McpProbe> {
    let endpoint = format!("http://{address}{MCP_PATH}");

    // `Instant`, not the injected clock: this measures the duration of a real
    // round trip, which is the thing being reported, not a decision the code
    // makes about time.
    let started = Instant::now();

    let transport = StreamableHttpClientTransport::with_client(
        reqwest::Client::default(),
        StreamableHttpClientTransportConfig::with_uri(endpoint.clone()),
    );
    let client = ()
        .serve(transport)
        .await
        .map_err(|error| Error::invalid(format!("could not reach {endpoint}: {error}")))?;

    let tools = client.list_all_tools().await.map_err(|error| {
        Error::invalid(format!(
            "{endpoint} answered, but listing its tools failed: {error}"
        ))
    })?;

    let peer = client.peer_info();
    let server_name = peer
        .as_ref()
        .and_then(|info| info.server_info.as_ref())
        .map(|implementation| implementation.name.clone())
        // A server that answered but declined to name itself is still a
        // reachable server; the panel says so rather than failing the probe.
        .unwrap_or_else(|| "unknown".to_string());
    let protocol_version = peer
        .as_ref()
        .map(|info| info.protocol_version.clone())
        .unwrap_or_else(ProtocolVersion::default)
        .to_string();

    let latency_ms = started.elapsed().as_millis() as u64;

    // Best effort: the probe's answer does not depend on a clean goodbye, and
    // a server that has already gone away must not turn a successful round
    // trip into a failure.
    let _ = client.cancel().await;

    Ok(McpProbe {
        endpoint,
        latency_ms,
        server_name,
        protocol_version,
        tool_count: tools.len(),
    })
}

/// What the user is told when the port is taken — in the log, in
/// [`McpStatus::message`], and verbatim in Settings → MCP.
///
/// It names the port, both plausible culprits, and the two things that fix it,
/// because this message is the entire recovery path: there is no retry and no
/// automatic fallback to another port, which would silently invalidate the URL
/// the user registered with `claude mcp add`.
fn port_in_use_message(port: u16) -> String {
    format!(
        "the MCP server could not start: port {port} on 127.0.0.1 is already in use. Another \
         Rimaia window, or another program, is listening on it. Change the port in \
         Settings → MCP, or quit whatever is using it."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestContext;
    use pretty_assertions::assert_eq;
    use tokio::net::TcpStream;

    /// A bound server on an OS-chosen port, already spawned.
    async fn serving(harness: &TestContext) -> (McpHandle, tokio::task::JoinHandle<()>) {
        let (handle, task) = build(
            harness.context.clone(),
            0,
            RunHandles::default(),
            Arc::new(crate::runner::provider::ClaudeProvider),
            Some(crate::testing::doctor::local_tools(harness.machine())),
        )
        .await;
        assert_eq!(handle.status().state, McpState::Listening);
        (handle, tokio::spawn(task.run()))
    }

    #[tokio::test]
    async fn the_server_binds_loopback_and_not_a_public_interface() {
        // ADR-0006's hard-coded interface, given a test rather than a comment.
        let harness = TestContext::new().await;
        let (handle, server) = serving(&harness).await;

        let status = handle.status();
        let bound = status.bound_address.expect("a bound address");
        assert!(
            bound.starts_with("127.0.0.1:"),
            "the port is configurable, the interface is not: {bound}"
        );
        assert_eq!(handle.url(), Some(format!("http://{bound}/mcp")));

        handle.shutdown();
        server.await.expect("the server task ends");
    }

    #[tokio::test]
    async fn the_server_stops_listening_when_it_is_shut_down() {
        // Task 010's "stopping the app makes the server unreachable with a
        // normal connection error". No sleep anywhere: awaiting the spawned
        // task is what makes "it has stopped" a fact rather than a guess.
        let harness = TestContext::new().await;
        let (handle, server) = serving(&harness).await;
        let address = handle.status().bound_address.expect("a bound address");

        TcpStream::connect(&address)
            .await
            .expect("it is listening now");

        handle.shutdown();
        server.await.expect("the server task ends");

        let refused = TcpStream::connect(&address)
            .await
            .expect_err("nothing is listening any more");
        assert_eq!(refused.kind(), std::io::ErrorKind::ConnectionRefused);
    }

    #[tokio::test]
    async fn a_port_already_in_use_is_reported_with_the_port_in_the_message() {
        let harness = TestContext::new().await;
        // Bind on 0 to learn a port that is definitely taken, and keep it.
        let squatter = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("take a port");
        let taken = squatter.local_addr().expect("its address").port();

        let handles = RunHandles::default();
        let (handle, task) = build(
            harness.context.clone(),
            taken,
            handles.clone(),
            Arc::new(crate::runner::provider::ClaudeProvider),
            Some(crate::testing::doctor::local_tools(harness.machine())),
        )
        .await;

        let status = handle.status();
        assert_eq!(status.state, McpState::PortInUse);
        assert_eq!(status.configured_port, taken);
        assert_eq!(status.bound_address, None, "there is no URL to offer");
        assert_eq!(handle.url(), None);
        assert_eq!(
            status.message.as_deref(),
            Some(port_in_use_message(taken).as_str())
        );
        assert_eq!(
            handles.endpoint(),
            None,
            "and no run is handed a URL nothing answers (seam-contract D16.7)"
        );

        // And the task is spawnable regardless, which is what lets the shell
        // spawn it without branching on the status a second time.
        task.run().await;
    }

    /// The number Settings → MCP shows after "Test connection", so it is
    /// asserted where the count actually crosses the wire rather than only
    /// against the router.
    ///
    /// **Ten until task 020**, then eleven, and nineteen since ADR-0021 made
    /// capability parity a rule: `set_task_strategy` is ADR-0006's
    /// 2026-08-28 amendment, which widens the table by exactly one and restates
    /// that it is otherwise closed. The count is pinned in two places on
    /// purpose — here and `server::tests::REGISTERED_TOOLS` — because a tool that
    /// registers but never reaches the wire, or the reverse, is a bug neither
    /// assertion catches alone.
    #[tokio::test]
    async fn the_probe_reports_the_server_it_is_pointed_at() {
        let harness = TestContext::new().await;
        let (handle, server) = serving(&harness).await;
        let address = handle.status().bound_address.expect("a bound address");

        let probed = probe(&address).await.expect("a real round trip");

        assert_eq!(probed.endpoint, format!("http://{address}/mcp"));
        assert_eq!(probed.server_name, "rimaia");
        assert_eq!(
            probed.tool_count,
            crate::mcp::Tool::ALL.len(),
            "every registered tool, over the wire this time (ADR-0021)"
        );
        assert!(!probed.protocol_version.is_empty());

        handle.shutdown();
        server.await.expect("the server task ends");
    }

    #[tokio::test]
    async fn probing_an_address_with_nothing_on_it_names_the_endpoint() {
        // What Test connection renders when the answer is "no". A specific
        // message, not a bare failure (seam-contract D8).
        let free = {
            let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .expect("take a port");
            listener.local_addr().expect("its address")
        };

        let error = probe(&free.to_string())
            .await
            .expect_err("nothing is listening there");

        assert!(
            error.to_string().contains(&format!("http://{free}/mcp")),
            "the message names what it tried: {error}"
        );
    }
}
