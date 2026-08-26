//! The embedded local MCP server (ADR-0006), so Claude Code sessions the user is
//! already running can read and write the board.
//!
//! It lives in core, not in the shell, precisely so its tool handlers are thin
//! adapters over the same services the Tauri commands call. A rule enforced in
//! only one of the two paths is a bug — the same invariant must produce the same
//! rejection whichever door it comes through.
//!
//! [`settings`] owns the port key, [`requests`] and [`responses`] are the wire
//! DTOs, [`server`] holds the ten tool handlers, and [`build`] binds the
//! listener.
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

use rmcp::model::ProtocolVersion;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::context::ServiceContext;
use crate::db::MutationSource;
use crate::error::{Error, Result};

pub mod error;
pub mod requests;
pub mod responses;
pub mod server;
pub mod settings;

pub use error::ToolError;
pub use server::RimaiaServer;
pub use settings::{configured_port, set_configured_port, MCP_PORT};

/// The port ADR-0006 fixes as the default, and the one every `claude mcp add`
/// line in the docs uses. Configurable for a collision; the *interface* is not
/// configurable and is hard-coded to loopback.
pub const DEFAULT_PORT: u16 = 4517;

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
    service: Option<StreamableHttpService<RimaiaServer, LocalSessionManager>>,
    shutdown: watch::Receiver<bool>,
}

struct Shared {
    status: McpStatus,
    shutdown: watch::Sender<bool>,
}

/// Binds the server and hands back the handle to keep and the task to spawn.
///
/// The same split as `scheduler::build`, for the same reason: the caller owns
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
pub async fn build(ctx: ServiceContext, port: u16) -> (McpHandle, McpTask) {
    // Every write this server makes is an agent's, not the user's (ADR-0019).
    // Re-sourced here, once, so no handler has to remember.
    let ctx = ctx.with_source(MutationSource::Mcp);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let bind = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await;

    let (status, listener, service) = match bind {
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
                Some(streamable_service(ctx)),
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

    (
        McpHandle {
            shared: Arc::new(Shared {
                status,
                shutdown: shutdown_tx,
            }),
        },
        McpTask {
            listener,
            service,
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
        let (Some(listener), Some(service)) = (self.listener, self.service) else {
            return;
        };

        let router = axum::Router::new().nest_service(MCP_PATH, service);
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

/// The tower service both [`build`] and the in-process tests mount.
///
/// `pub(crate)` so a test can drive one JSON-RPC request through the real
/// transport without a socket, and so neither can drift onto a different
/// configuration than the other.
pub(crate) fn streamable_service(
    ctx: ServiceContext,
) -> StreamableHttpService<RimaiaServer, LocalSessionManager> {
    let mut config = StreamableHttpServerConfig::default();
    // Stateless: see this module's header.
    config.legacy_session_mode = false;
    config.json_response = true;

    StreamableHttpService::new(
        move || Ok(RimaiaServer::new(ctx.clone())),
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
        let (handle, task) = build(harness.context.clone(), 0).await;
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

        let (handle, task) = build(harness.context.clone(), taken).await;

        let status = handle.status();
        assert_eq!(status.state, McpState::PortInUse);
        assert_eq!(status.configured_port, taken);
        assert_eq!(status.bound_address, None, "there is no URL to offer");
        assert_eq!(handle.url(), None);
        assert_eq!(
            status.message.as_deref(),
            Some(port_in_use_message(taken).as_str())
        );

        // And the task is spawnable regardless, which is what lets the shell
        // spawn it without branching on the status a second time.
        task.run().await;
    }

    #[tokio::test]
    async fn the_probe_reports_the_server_it_is_pointed_at() {
        let harness = TestContext::new().await;
        let (handle, server) = serving(&harness).await;
        let address = handle.status().bound_address.expect("a bound address");

        let probed = probe(&address).await.expect("a real round trip");

        assert_eq!(probed.endpoint, format!("http://{address}/mcp"));
        assert_eq!(probed.server_name, "rimaia");
        assert_eq!(
            probed.tool_count, 10,
            "ADR-0006's ten tools, over the wire this time"
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
