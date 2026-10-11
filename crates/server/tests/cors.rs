//! Cross-origin calls from the Tauri origins, and only with a bearer token
//! (task 050 Scope 2, ADR-0034 point 6).
//!
//! Every router here is built with an explicit `dev`, because
//! `cfg!(debug_assertions)` is always true under `cargo test`.

mod common;

use pretty_assertions::assert_eq;
use rimaia_core::api::PROTOCOL_VERSION;
use rimaia_core::testing::api::TEAM_A_TOKEN;
use rimaia_core::testing::teams::TwoTeams;
use rimaia_server::cors::allowed_origins;
use rimaia_server::RouterOptions;

use common::{two_teams_state, TestServer};

const TAURI_ORIGINS: [&str; 3] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

async fn serving(dev: bool) -> (TwoTeams, TestServer) {
    let teams = TwoTeams::new().await;
    let options = RouterOptions {
        dev,
        web_root: None,
    };
    let server = TestServer::spawn_with(two_teams_state(&teams).await, &options).await;
    (teams, server)
}

/// A preflight for `POST /api/v1/<path>` from `origin`, asking for `headers`.
async fn preflight(
    server: &TestServer,
    path: &str,
    origin: &str,
    headers: &str,
) -> reqwest::Response {
    server
        .client()
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/api/v1/{path}", server.base_url()),
        )
        .header("origin", origin)
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", headers)
        .send()
        .await
        .expect("the preflight reaches the server")
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[test]
fn allowed_origins_are_exactly_the_tauri_origins() {
    assert_eq!(allowed_origins(false), TAURI_ORIGINS.to_vec());
    assert_eq!(
        allowed_origins(true),
        [&TAURI_ORIGINS[..], &["http://localhost:1420"]].concat()
    );
}

#[tokio::test]
async fn a_tauri_origin_may_preflight_a_bearer_request() {
    let (_teams, server) = serving(false).await;

    for origin in TAURI_ORIGINS {
        let response = preflight(
            &server,
            "list_tasks",
            origin,
            "authorization, content-type, rimaia-protocol, rimaia-team",
        )
        .await;

        assert!(response.status().is_success(), "{origin}");
        assert_eq!(
            header(&response, "access-control-allow-origin").as_deref(),
            Some(origin)
        );
        let allowed = header(&response, "access-control-allow-headers").expect("allowed headers");
        for name in [
            "authorization",
            "rimaia-protocol",
            "rimaia-team",
            "content-type",
        ] {
            assert!(
                allowed.split(',').any(|allowed| allowed.trim() == name),
                "{origin}: {allowed}"
            );
        }
        let methods = header(&response, "access-control-allow-methods").expect("methods");
        assert!(methods.contains("POST"), "{methods}");
        assert_eq!(
            header(&response, "access-control-max-age").as_deref(),
            Some("600")
        );
        assert_eq!(
            header(&response, "access-control-allow-credentials"),
            None,
            "{origin}: a cookie never rides a cross-origin call"
        );
    }
}

#[tokio::test]
async fn the_dev_origin_is_allowed_only_when_dev_is() {
    for (dev, expected) in [(true, Some("http://localhost:1420")), (false, None)] {
        let (_teams, server) = serving(dev).await;

        let response = preflight(
            &server,
            "list_tasks",
            "http://localhost:1420",
            "authorization",
        )
        .await;

        assert_eq!(
            header(&response, "access-control-allow-origin").as_deref(),
            expected,
            "dev = {dev}"
        );
    }
}

#[tokio::test]
async fn any_other_origin_gets_no_cors_headers() {
    let (_teams, server) = serving(false).await;

    for origin in ["https://evil.example", "null", "http://localhost:1420"] {
        let response = preflight(&server, "list_tasks", origin, "authorization").await;
        assert_eq!(
            header(&response, "access-control-allow-origin"),
            None,
            "{origin}"
        );

        // And on the request itself, not only on the preflight.
        let answer = server
            .client()
            .post(format!("{}/api/v1/list_repositories", server.base_url()))
            .header("origin", origin)
            .header("authorization", format!("Bearer {TEAM_A_TOKEN}"))
            .header("rimaia-protocol", PROTOCOL_VERSION)
            .send()
            .await
            .expect("the request reaches the server");
        assert_eq!(
            header(&answer, "access-control-allow-origin"),
            None,
            "{origin}"
        );
    }
}

#[tokio::test]
async fn a_tauri_origin_reads_the_answer_to_a_real_request() {
    let (_teams, server) = serving(false).await;

    let answer = server
        .client()
        .post(format!("{}/api/v1/list_repositories", server.base_url()))
        .header("origin", "tauri://localhost")
        .header("authorization", format!("Bearer {TEAM_A_TOKEN}"))
        .header("rimaia-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .expect("the request reaches the server");

    assert_eq!(answer.status().as_u16(), 200);
    assert_eq!(
        header(&answer, "access-control-allow-origin").as_deref(),
        Some("tauri://localhost")
    );
    assert_eq!(header(&answer, "access-control-allow-credentials"), None);
}

#[tokio::test]
async fn the_csrf_header_is_not_allowed_cross_origin() {
    let (_teams, server) = serving(false).await;

    let response = preflight(&server, "list_tasks", "tauri://localhost", "x-rimaia-csrf").await;

    let allowed = header(&response, "access-control-allow-headers").unwrap_or_default();
    assert!(
        !allowed.to_lowercase().contains("x-rimaia-csrf"),
        "{allowed}"
    );
    assert!(!allowed.contains('*'), "{allowed}");
}

#[tokio::test]
async fn the_event_stream_is_covered_and_the_bundle_is_not() {
    let root = tempfile::Builder::new()
        .prefix("rimaia-cors-web-root-")
        .tempdir()
        .expect("a web root");
    std::fs::write(root.path().join("index.html"), "<title>Rimaia</title>").expect("index.html");
    let teams = TwoTeams::new().await;
    let options = RouterOptions {
        dev: false,
        web_root: Some(root.path().to_path_buf()),
    };
    let server = TestServer::spawn_with(two_teams_state(&teams).await, &options).await;

    let events = server
        .client()
        .request(
            reqwest::Method::OPTIONS,
            format!("{}/api/v1/events", server.base_url()),
        )
        .header("origin", "tauri://localhost")
        .header("access-control-request-method", "GET")
        .header("access-control-request-headers", "authorization")
        .send()
        .await
        .expect("the preflight reaches the server");
    let page = server
        .client()
        .get(format!("{}/", server.base_url()))
        .header("origin", "tauri://localhost")
        .send()
        .await
        .expect("the request reaches the server");

    assert_eq!(
        header(&events, "access-control-allow-origin").as_deref(),
        Some("tauri://localhost")
    );
    assert_eq!(header(&page, "access-control-allow-origin"), None);
}
