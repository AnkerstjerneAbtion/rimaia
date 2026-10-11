//! The loopback harness every `rimaia-server` integration test shares (task
//! 046; task 048's events tests reuse it). Pulled in with `mod common;`.
//!
//! A real listener on an ephemeral port and a real HTTP client: the router is
//! exercised exactly as a browser or a connected desktop reaches it.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use rimaia_core::api::{Authenticate, PROTOCOL_VERSION};
use rimaia_core::testing::api::{self as testing_api, FixedCaller};
use rimaia_core::testing::identity::FakeIdentityProvider;
use rimaia_core::testing::teams::TwoTeams;
use rimaia_server::{router, ServerState};
use serde_json::Value;

/// A server on `127.0.0.1:0`, serving until the test ends.
pub struct TestServer {
    address: SocketAddr,
    client: reqwest::Client,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// Binds before spawning, so the port is taken and answering by the time
    /// this returns: no startup race to wait out.
    pub async fn spawn(state: ServerState) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port");
        let address = listener.local_addr().expect("the bound address");
        let task = tokio::spawn(async move {
            rimaia_server::serve(listener, router(state))
                .await
                .expect("the test server serves");
        });
        // A redirect is part of what the sign-in routes answer, so it is read,
        // never followed.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a client");
        Self {
            address,
            client,
            task,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// The one client every request goes through.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// `POST /api/v1/<name>` with `body`, a bearer `token` and a
    /// `Rimaia-Protocol` header when given.
    pub async fn post(
        &self,
        name: &str,
        token: Option<&str>,
        protocol: Option<&str>,
        body: impl Into<reqwest::Body>,
    ) -> reqwest::Response {
        let mut request = self
            .client
            .post(format!("{}/api/v1/{name}", self.base_url()))
            .body(body);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(protocol) = protocol {
            request = request.header("rimaia-protocol", protocol);
        }
        request
            .send()
            .await
            .expect("the request reaches the server")
    }

    /// [`post`](Self::post) of `args` as JSON, as `token`, at this build's
    /// protocol version: what a current client sends.
    pub async fn command(&self, name: &str, token: &str, args: &Value) -> Answer {
        Answer::of(
            self.post(name, Some(token), Some(PROTOCOL_VERSION), args.to_string())
                .await,
        )
        .await
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A response read whole: its status, its `WWW-Authenticate` and
/// `Content-Type`, and its body as JSON.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub www_authenticate: Option<String>,
    pub content_type: Option<String>,
    pub body: Value,
}

impl Answer {
    pub async fn of(response: reqwest::Response) -> Self {
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let www_authenticate = header("www-authenticate");
        let content_type = header("content-type");
        let text = response.text().await.expect("a body");
        let body = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("the body is JSON ({error}): {text}"));
        Self {
            status,
            www_authenticate,
            content_type,
            body,
        }
    }

    /// The error shape's `code`, or `None` for a success.
    pub fn code(&self) -> Option<&str> {
        (self.status != 200)
            .then(|| self.body["code"].as_str())
            .flatten()
    }
}

/// A server state over the fixture's own board and clock, authenticated by
/// `FixedCaller::two_teams`.
pub async fn two_teams_state(teams: &TwoTeams) -> ServerState {
    state_over(
        testing_api::host(teams).await,
        FixedCaller::two_teams(teams),
    )
}

/// A server state over `host`, authenticated by `auth`, signing in with a
/// fake provider nobody here uses.
pub fn state_over(
    host: rimaia_core::api::BoardHost,
    auth: impl Authenticate + 'static,
) -> ServerState {
    ServerState::new(
        host,
        Arc::new(auth),
        Arc::new(FakeIdentityProvider::new()),
        "https://rimaia.test",
    )
}
