//! `HttpBoard` and the proxy variables in its environment (task 052).
//!
//! A plain-HTTP origin is admitted only for loopback, on the premise that the
//! runner's token then never leaves the machine. reqwest reads `HTTP_PROXY`,
//! `http_proxy` and `ALL_PROXY` by default, with no loopback bypass unless
//! `NO_PROXY` names it, which would hand the token to the proxy in clear.
//!
//! The case sets process environment variables, so it lives alone in its own
//! test binary: no other test can read the environment while it is changed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::Router;
use rimaia_core::board::{BoardPort, LeaseRef};
use rimaia_core::identity::secret::Secret;
use rimaia_core::runner::provider::ProviderId;
use rimaia_runner::board::HttpBoard;

#[tokio::test]
async fn a_loopback_board_is_reached_directly_even_with_a_proxy_in_the_environment() {
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the proxy's port");
    let proxy_origin = format!("http://{}", proxy.local_addr().expect("an address"));
    // Before anything is spawned that could read the environment.
    for name in ["HTTP_PROXY", "http_proxy", "ALL_PROXY"] {
        std::env::set_var(name, &proxy_origin);
    }
    for name in ["NO_PROXY", "no_proxy", "all_proxy"] {
        std::env::remove_var(name);
    }

    // Accepts and hangs up at once, so a request sent to it fails rather than
    // waits: the flag is set before the hang-up the client then sees.
    let proxied = Arc::new(AtomicBool::new(false));
    let proxy_task = tokio::spawn({
        let proxied = proxied.clone();
        async move {
            while let Ok((connection, _)) = proxy.accept().await {
                proxied.store(true, Ordering::SeqCst);
                drop(connection);
            }
        }
    });

    // Records that a request arrived before answering it; what it answers
    // does not matter.
    let reached = Arc::new(AtomicBool::new(false));
    let board_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the board's port");
    let board_origin = format!(
        "http://{}",
        board_listener.local_addr().expect("an address")
    );
    let router = Router::new().fallback({
        let reached = reached.clone();
        move || async move {
            reached.store(true, Ordering::SeqCst);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    });
    let board_task = tokio::spawn(async move {
        axum::serve(board_listener, router)
            .await
            .expect("the stub serves");
    });

    let board = HttpBoard::new(
        &board_origin,
        Secret::new("rmr_a-runner-token-nobody-may-see"),
        ProviderId::ClaudeCode,
    )
    .expect("a loopback origin");
    let _ = board.release(&LeaseRef::new("task-1", 3, "team-1")).await;

    proxy_task.abort();
    board_task.abort();
    assert!(
        !proxied.load(Ordering::SeqCst),
        "the request, bearer token and all, went to the proxy"
    );
    assert!(
        reached.load(Ordering::SeqCst),
        "the board never saw the request"
    );
}
