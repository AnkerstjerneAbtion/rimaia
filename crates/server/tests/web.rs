//! The built bundle, served at `/` beside the API (task 050 Scope 1,
//! ADR-0034 point 6).
//!
//! The bundle is a real `index.html` and a real hashed asset in a `TempDir`,
//! served by a real listener; nothing about the filesystem is faked.

mod common;

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rimaia_core::testing::teams::TwoTeams;
use rimaia_server::config::{config_from_env, WEB_ROOT_ENV};
use rimaia_server::RouterOptions;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use common::{two_teams_state, TestServer};

const INDEX: &str = "<!doctype html><title>Rimaia</title><div id=\"root\"></div>";
const ASSET: &str = "console.log('rimaia');";

/// A web root holding `index.html` and `assets/index-abc123.js`.
fn bundle() -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix("rimaia-web-root-")
        .tempdir()
        .expect("a web root");
    std::fs::write(dir.path().join("index.html"), INDEX).expect("write index.html");
    std::fs::create_dir(dir.path().join("assets")).expect("an assets directory");
    std::fs::write(dir.path().join("assets/index-abc123.js"), ASSET).expect("write an asset");
    dir
}

async fn serving(root: Option<&Path>) -> (TwoTeams, TestServer) {
    let teams = TwoTeams::new().await;
    let options = RouterOptions {
        dev: false,
        web_root: root.map(Path::to_path_buf),
    };
    let server = TestServer::spawn_with(two_teams_state(&teams).await, &options).await;
    (teams, server)
}

struct Fetched {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: String,
}

impl Fetched {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

async fn get(server: &TestServer, path: &str) -> Fetched {
    let response = server
        .client()
        .get(format!("{}{path}", server.base_url()))
        .send()
        .await
        .expect("the request reaches the server");
    Fetched {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: response.text().await.expect("a body"),
    }
}

/// `target` sent exactly as written, because a client library resolves `..`
/// and decodes `%2e` before a request is ever made.
async fn raw_get(server: &TestServer, target: &str) -> (u16, String) {
    let address = server.base_url().replace("http://", "");
    let mut stream = tokio::net::TcpStream::connect(&address)
        .await
        .expect("connect");
    stream
        .write_all(
            format!("GET {target} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .expect("send");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .expect("a status line");
    (status, response)
}

#[tokio::test]
async fn the_bundle_is_served_at_the_root() {
    let root = bundle();
    let (_teams, server) = serving(Some(root.path())).await;

    let page = get(&server, "/").await;

    assert_eq!(page.status, 200);
    assert!(page
        .header("content-type")
        .is_some_and(|value| value.starts_with("text/html")));
    assert_eq!(page.body, INDEX);
    assert_eq!(page.header("cache-control"), Some("no-cache"));
}

#[tokio::test]
async fn hashed_assets_are_cached_for_a_year() {
    let root = bundle();
    let (_teams, server) = serving(Some(root.path())).await;

    let asset = get(&server, "/assets/index-abc123.js").await;

    assert_eq!(asset.status, 200);
    assert_eq!(asset.body, ASSET);
    assert_eq!(
        asset.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
}

#[tokio::test]
async fn an_asset_that_is_not_there_is_not_cached_for_a_year() {
    let root = bundle();
    let (_teams, server) = serving(Some(root.path())).await;

    let missing = get(&server, "/assets/index-def456.js").await;

    assert_eq!(missing.status, 404);
    assert_eq!(missing.header("cache-control"), None);
}

#[tokio::test]
async fn an_api_path_never_falls_through_to_the_bundle() {
    let root = bundle();
    let (_teams, server) = serving(Some(root.path())).await;

    let unknown_command = server.post("no_such_command", None, None, "{}").await;
    let unknown_status = unknown_command.status().as_u16();
    let unknown_body = unknown_command.text().await.expect("a body");
    let api_root = get(&server, "/api/v1/").await;
    let other_version = get(&server, "/api/v2/list_tasks").await;

    assert_eq!(unknown_status, 404);
    assert!(
        unknown_body.starts_with(r#"{"code":"not_found""#),
        "{unknown_body}"
    );
    for page in [api_root, other_version] {
        assert_eq!(page.status, 404);
        assert!(
            page.body.starts_with(r#"{"code":"not_found""#),
            "{}",
            page.body
        );
        assert_ne!(page.body, INDEX);
    }
}

#[tokio::test]
async fn an_unknown_path_is_not_the_app() {
    let root = bundle();
    let (_teams, server) = serving(Some(root.path())).await;

    for path in ["/account", "/assets/", "/index"] {
        let page = get(&server, path).await;

        assert_eq!(page.status, 404, "{path}");
        assert_ne!(page.body, INDEX, "{path}");
    }
}

#[tokio::test]
async fn the_invitation_page_is_the_app() {
    // Task 051's one deep link: the token rides in the fragment, which no
    // request carries, so the path alone is the page.
    let root = bundle();
    let (_teams, server) = serving(Some(root.path())).await;

    let page = get(&server, "/invite").await;

    assert_eq!(page.status, 200);
    assert_eq!(page.body, INDEX);
    assert!(page
        .header("content-type")
        .is_some_and(|value| value.starts_with("text/html")));
    assert_eq!(page.header("cache-control"), Some("no-cache"));
    assert_eq!(page.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        page.header("content-security-policy"),
        Some("frame-ancestors 'none'")
    );
    for path in ["/invite/anything", "/invite/", "/invites"] {
        let other = get(&server, path).await;
        assert_eq!(other.status, 404, "{path}");
        assert_ne!(other.body, INDEX, "{path}");
    }
}

#[tokio::test]
async fn a_path_outside_the_web_root_is_never_served() {
    let outer = tempfile::Builder::new()
        .prefix("rimaia-web-outer-")
        .tempdir()
        .expect("an outer directory");
    let secret = "a secret outside the web root";
    std::fs::write(outer.path().join("secret.txt"), secret).expect("write a secret");
    let root = outer.path().join("dist");
    std::fs::create_dir(&root).expect("a web root");
    std::fs::write(root.join("index.html"), INDEX).expect("write index.html");
    let (_teams, server) = serving(Some(&root)).await;

    for target in [
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/%2E%2E/secret.txt",
        "/..%2fsecret.txt",
        "/assets/../../secret.txt",
        "/%2e%2e%2fsecret.txt",
    ] {
        let (status, response) = raw_get(&server, target).await;

        assert!(
            !response.contains(secret),
            "{target} served a file: {response}"
        );
        assert_ne!(status, 200, "{target}");
    }
}

#[tokio::test]
async fn the_bundle_cannot_be_framed() {
    let root = bundle();
    let (_teams, server) = serving(Some(root.path())).await;

    for path in ["/", "/assets/index-abc123.js", "/nothing-here"] {
        let page = get(&server, path).await;

        assert_eq!(
            page.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
        assert_eq!(
            page.header("content-security-policy"),
            Some("frame-ancestors 'none'"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn without_a_web_root_only_the_api_is_served() {
    let (_teams, server) = serving(None).await;

    let page = get(&server, "/").await;
    let api = server.post("no_such_command", None, None, "{}").await;

    assert_eq!(page.status, 404);
    assert_eq!(page.body, "");
    assert_eq!(api.status().as_u16(), 404);
}

#[test]
fn a_web_root_without_an_index_refuses_to_start() {
    let empty = tempfile::Builder::new()
        .prefix("rimaia-web-root-")
        .tempdir()
        .expect("a web root");
    let file = empty.path().join("not-a-directory");
    std::fs::write(&file, "").expect("a file");

    for (root, why) in [
        (empty.path().to_path_buf(), "has no index.html"),
        (empty.path().join("missing"), "is not a directory"),
        (file, "is not a directory"),
    ] {
        let refused = config_from_env(environment(root.as_os_str().to_owned()))
            .expect_err("a web root that cannot be served");

        let message = refused.to_string();
        assert!(
            message.starts_with(&format!("{WEB_ROOT_ENV} is {}", root.display())),
            "{message}"
        );
        assert!(message.contains(why), "{message}");
    }
}

#[test]
fn a_relative_web_root_is_refused_by_name() {
    let refused = config_from_env(environment("dist".into())).expect_err("a relative path");

    assert!(refused.to_string().contains(WEB_ROOT_ENV), "{refused}");
    assert!(refused.to_string().contains("absolute"), "{refused}");
}

#[test]
fn a_web_root_that_can_be_served_is_read_and_an_unset_one_is_none() {
    let root = bundle();

    let served =
        config_from_env(environment(root.path().as_os_str().to_owned())).expect("a bundle");
    let api_only = config_from_env(|name| {
        (name != WEB_ROOT_ENV)
            .then(|| environment(String::new().into())(name))
            .flatten()
    })
    .expect("no web root");

    assert_eq!(served.web_root, Some(PathBuf::from(root.path())));
    assert_eq!(api_only.web_root, None);
}

/// The server's other variables, valid, and `RIMAIA_WEB_ROOT` set to `root`.
fn environment(root: std::ffi::OsString) -> impl Fn(&str) -> Option<std::ffi::OsString> {
    let data = std::env::temp_dir().join("rimaia-server-web-test");
    move |name| match name {
        "RIMAIA_DATA_DIR" => Some(data.clone().into_os_string()),
        "RIMAIA_LISTEN" => Some("127.0.0.1:8787".into()),
        "RIMAIA_PUBLIC_URL" => Some("https://rimaia.test".into()),
        "RIMAIA_GITHUB_CLIENT_ID" => Some("Iv1.test".into()),
        "RIMAIA_GITHUB_CLIENT_SECRET" => Some("a-github-client-secret".into()),
        "RIMAIA_WEB_ROOT" => Some(root.clone()),
        _ => None,
    }
}
