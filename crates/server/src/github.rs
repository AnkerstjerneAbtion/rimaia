//! Signing in with GitHub (ADR-0030 point 1), behind core's
//! [`IdentityProvider`] seam.
//!
//! An OAuth app's authorization code flow with PKCE, asking for `read:user`
//! and nothing else. The exchange is two HTTPS requests: the code for an
//! access token, and the token for the user. **Rimaia does not keep the
//! token.** It is a local of [`GitHub::exchange`] and is dropped when the
//! exchange returns: there is no column for it and no field that could carry
//! it out, because nothing Rimaia does acts on GitHub with the sign-in grant.
//!
//! Request bodies are built with `serde_json::to_vec` and an explicit
//! `Content-Type`, and answers are read as bytes, because D34 approves neither
//! reqwest's `json` nor its `form` feature.

use std::fmt;

use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::Url;
use rimaia_core::api::BoxFuture;
use rimaia_core::identity::provider::{IdentityProvider, ProviderIdentity};
use rimaia_core::identity::secret::Secret;
use rimaia_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// What GitHub asks every API client to name itself with.
const CLIENT_NAME: &str = "Rimaia";

/// Where the three requests go. Production uses [`GitHubEndpoints::github`];
/// the tests point the two server-side requests at a loopback stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubEndpoints {
    pub authorize: String,
    pub access_token: String,
    pub user: String,
}

impl GitHubEndpoints {
    pub fn github() -> Self {
        Self {
            authorize: "https://github.com/login/oauth/authorize".to_string(),
            access_token: "https://github.com/login/oauth/access_token".to_string(),
            user: "https://api.github.com/user".to_string(),
        }
    }
}

/// A GitHub OAuth app.
pub struct GitHub {
    client_id: String,
    client_secret: Secret,
    endpoints: GitHubEndpoints,
    http: reqwest::Client,
}

impl fmt::Debug for GitHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHub")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret)
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl GitHub {
    pub fn new(
        client_id: String,
        client_secret: Secret,
        endpoints: GitHubEndpoints,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                Error::internal(format!("could not build an HTTPS client: {error}"))
            })?;
        Ok(Self {
            client_id,
            client_secret,
            endpoints,
            http,
        })
    }

    /// The bytes of a successful answer, or `internal` naming the step.
    async fn read(response: reqwest::Response, step: &str) -> Result<Value> {
        let status = response.status();
        let bytes = response.bytes().await.map_err(|error| {
            Error::internal(format!("GitHub's {step} answer was cut off: {error}"))
        })?;
        if !status.is_success() {
            return Err(Error::internal(format!(
                "GitHub answered the {step} request with {status}"
            )));
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            Error::internal(format!("GitHub's {step} answer is not JSON: {error}"))
        })
    }
}

/// The token endpoint's answer. GitHub answers a refused code with `200` and
/// an `error`, so the token's absence is the refusal.
#[derive(Deserialize)]
struct TokenAnswer {
    access_token: Option<String>,
}

/// The fields of `GET /user` sign-in reads.
#[derive(Deserialize)]
struct UserAnswer {
    id: Option<Value>,
    login: Option<String>,
    avatar_url: Option<String>,
}

impl IdentityProvider for GitHub {
    fn id(&self) -> &'static str {
        "github"
    }

    fn authorize_url(&self, state: &str, code_challenge: &str, redirect_uri: &str) -> String {
        Url::parse_with_params(
            &self.endpoints.authorize,
            &[
                ("client_id", self.client_id.as_str()),
                ("redirect_uri", redirect_uri),
                ("scope", "read:user"),
                ("state", state),
                ("code_challenge", code_challenge),
                ("code_challenge_method", "S256"),
            ],
        )
        .map(String::from)
        // The endpoint is a constant, or a test's loopback URL.
        .unwrap_or_else(|_| self.endpoints.authorize.clone())
    }

    fn exchange<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<ProviderIdentity>> {
        Box::pin(async move {
            let body = serde_json::to_vec(&json!({
                "client_id": self.client_id,
                "client_secret": self.client_secret.expose(),
                "code": code,
                "redirect_uri": redirect_uri,
                "code_verifier": code_verifier,
            }))
            .map_err(|error| Error::internal(format!("could not encode the exchange: {error}")))?;
            let answer = self
                .http
                .post(&self.endpoints.access_token)
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json")
                .header(USER_AGENT, CLIENT_NAME)
                .body(body)
                .send()
                .await
                .map_err(|error| Error::internal(format!("could not reach GitHub: {error}")))?;
            let answer: TokenAnswer = serde_json::from_value(Self::read(answer, "token").await?)
                .map_err(|_| Error::internal("GitHub's token answer has an unexpected shape"))?;
            // Dropped when this future returns: never stored, never returned.
            let access_token = answer
                .access_token
                .ok_or_else(|| Error::internal("GitHub did not accept the sign-in code"))?;

            let user = self
                .http
                .get(&self.endpoints.user)
                .header(AUTHORIZATION, format!("Bearer {access_token}"))
                .header(ACCEPT, "application/vnd.github+json")
                .header(USER_AGENT, CLIENT_NAME)
                .send()
                .await
                .map_err(|error| Error::internal(format!("could not reach GitHub: {error}")))?;
            let user: UserAnswer = serde_json::from_value(Self::read(user, "user").await?)
                .map_err(|_| Error::internal("GitHub's user answer has an unexpected shape"))?;

            // The numeric id, never the login, which a rename changes.
            let subject = user
                .id
                .as_ref()
                .and_then(Value::as_u64)
                .map(|id| id.to_string())
                .ok_or_else(|| Error::internal("GitHub's user answer has no numeric id"))?;
            let login = user
                .login
                .ok_or_else(|| Error::internal("GitHub's user answer has no login"))?;
            Ok(ProviderIdentity {
                subject,
                login,
                avatar_url: user.avatar_url,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use pretty_assertions::assert_eq;
    use rimaia_core::ErrorCode;

    #[test]
    fn github_builds_the_authorize_url_github_documents() {
        let github = GitHub::new(
            "Iv1.test".to_string(),
            Secret::new("a-client-secret"),
            GitHubEndpoints::github(),
        )
        .expect("a client");

        assert_eq!(
            github.authorize_url(
                "state-1",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "https://rimaia.test/auth/github/callback",
            ),
            "https://github.com/login/oauth/authorize?client_id=Iv1.test\
             &redirect_uri=https%3A%2F%2Frimaia.test%2Fauth%2Fgithub%2Fcallback\
             &scope=read%3Auser&state=state-1\
             &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM\
             &code_challenge_method=S256"
        );
        assert!(!format!("{github:?}").contains("a-client-secret"));
    }

    /// What the stub saw of the token request.
    #[derive(Debug, Clone, Default)]
    struct Seen {
        token_requests: Vec<(HeaderMap, Value)>,
        user_authorization: Vec<String>,
    }

    #[derive(Clone)]
    struct Stub {
        seen: Arc<Mutex<Seen>>,
        user: Value,
    }

    /// A loopback stub serving GitHub's documented token and `/user` shapes,
    /// and the endpoints that point at it.
    async fn stub(user: Value) -> (GitHubEndpoints, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let app = Router::new()
            .route(
                "/login/oauth/access_token",
                post(
                    |State(stub): State<Stub>, headers: HeaderMap, body: axum::body::Bytes| async move {
                        let body: Value = serde_json::from_slice(&body).expect("a JSON body");
                        stub.seen
                            .lock()
                            .expect("seen")
                            .token_requests
                            .push((headers, body));
                        Json(json!({
                            "access_token": "gho_16C7e42F292c6912E7710c838347Ae178B4a",
                            "scope": "read:user",
                            "token_type": "bearer",
                        }))
                    },
                ),
            )
            .route(
                "/user",
                get(|State(stub): State<Stub>, headers: HeaderMap| async move {
                    let authorization = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    stub.seen
                        .lock()
                        .expect("seen")
                        .user_authorization
                        .push(authorization);
                    Json(stub.user.clone())
                }),
            )
            .with_state(Stub {
                seen: seen.clone(),
                user,
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("an address");
        tokio::spawn(async move {
            crate::serve(listener, app).await.expect("the stub serves");
        });
        let endpoints = GitHubEndpoints {
            authorize: "https://github.com/login/oauth/authorize".to_string(),
            access_token: format!("http://{address}/login/oauth/access_token"),
            user: format!("http://{address}/user"),
        };
        (endpoints, seen)
    }

    #[tokio::test]
    async fn github_exchanges_a_code_for_the_numeric_subject_and_keeps_no_token() {
        let (endpoints, seen) = stub(json!({
            "login": "octocat",
            "id": 583231,
            "node_id": "MDQ6VXNlcjE=",
            "avatar_url": "https://avatars.githubusercontent.com/u/583231?v=4",
            "type": "User",
        }))
        .await;
        let github = GitHub::new(
            "Iv1.test".to_string(),
            Secret::new("the-client-secret"),
            endpoints,
        )
        .expect("a client");

        let identity = github
            .exchange(
                "the-code",
                "the-verifier",
                "https://rimaia.test/auth/github/callback",
            )
            .await
            .expect("an identity");

        assert_eq!(
            identity,
            ProviderIdentity {
                subject: "583231".to_string(),
                login: "octocat".to_string(),
                avatar_url: Some("https://avatars.githubusercontent.com/u/583231?v=4".to_string()),
            }
        );
        let seen = seen.lock().expect("seen").clone();
        let (headers, body) = &seen.token_requests[0];
        assert_eq!(
            body,
            &json!({
                "client_id": "Iv1.test",
                "client_secret": "the-client-secret",
                "code": "the-code",
                "redirect_uri": "https://rimaia.test/auth/github/callback",
                "code_verifier": "the-verifier",
            })
        );
        assert_eq!(headers["accept"], "application/json");
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(
            seen.user_authorization,
            vec!["Bearer gho_16C7e42F292c6912E7710c838347Ae178B4a".to_string()]
        );
        // The provider's token is nowhere in what came back.
        assert!(!format!("{identity:?}").contains("gho_"));

        // A `/user` answer without a numeric id is `internal`.
        let (endpoints, _) = stub(json!({ "login": "octocat", "id": "583231" })).await;
        let github =
            GitHub::new("Iv1.test".to_string(), Secret::new("s"), endpoints).expect("a client");
        let refused = github
            .exchange("the-code", "the-verifier", "https://rimaia.test/cb")
            .await
            .expect_err("a string id");
        assert_eq!(refused.code(), ErrorCode::Internal);
    }

    #[tokio::test]
    async fn a_user_answer_without_a_numeric_id_is_internal() {
        for user in [
            json!({ "login": "octocat" }),
            json!({ "login": "octocat", "id": "583231" }),
            json!({ "login": "octocat", "id": null }),
        ] {
            let (endpoints, _) = stub(user.clone()).await;
            let github =
                GitHub::new("Iv1.test".to_string(), Secret::new("s"), endpoints).expect("a client");

            let refused = github
                .exchange("the-code", "the-verifier", "https://rimaia.test/cb")
                .await
                .expect_err("no numeric id");

            assert_eq!(refused.code(), ErrorCode::Internal, "{user}");
        }
    }
}
