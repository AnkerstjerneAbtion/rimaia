//! Signing in, outside the command registry (task 047, ADR-0030 points 2, 4
//! and 5).
//!
//! These routes are not board commands: they run before there is a caller, or
//! they must set a cookie, which `dispatch` cannot.
//!
//! | Route | Does |
//! | --- | --- |
//! | `GET /auth/github` | A browser's sign-in: to the provider, with a `state` and a PKCE challenge |
//! | `GET /auth/desktop` | The desktop's sign-in through the browser, with a loopback redirect (RFC 8252) |
//! | `GET /auth/github/callback` | Back from the provider: a session for a browser, a one-time code for the desktop |
//! | `POST /api/v1/auth/desktop_token` | The desktop trades its code and verifier for an `rmd_` token |
//! | `POST /api/v1/auth/pair` | A headless runner trades a pairing code for an `rmr_` token |
//!
//! The three `GET`s are browser navigations: they carry no protocol header,
//! and a failed callback redirects to `/?sign_in=denied`, `expired` or
//! `failed`, the whole vocabulary the web app renders. Nothing from the query
//! string is echoed into a page. The two `POST`s answer in the error shape,
//! parse their bodies themselves and check the protocol as a write does.
//!
//! **Every route here is rate limited**, by the peer address axum's
//! `ConnectInfo` gives, which [`serve`](crate::serve) arranges. Behind task
//! 062's proxy every request has the proxy's address and the limit becomes one
//! for the whole instance: still a limit, failing closed. No forwarded header
//! is read until 062 decides which proxy to trust.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::Redirect;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use reqwest::Url;
use rimaia_core::api::{protocol, Effect};
use rimaia_core::identity::pairing::{self, PairedRunner};
use rimaia_core::identity::pending::SignInFlow;
use rimaia_core::identity::rate_limit::Bucket;
use rimaia_core::identity::secret;
use rimaia_core::identity::sessions;
use rimaia_core::identity::sign_in::upsert_user;
use rimaia_core::identity::tokens::{self, NewApiToken};
use rimaia_core::Error;
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::{ApiError, ProtocolHeader, ServerState};

/// The session cookie: the session secret, which only its hash is stored of.
pub const SESSION_COOKIE: &str = "rimaia_session";
/// The CSRF cookie: [`secret::csrf_for`] the session secret, readable by the
/// web app, which sends it back as [`CSRF_HEADER`]. A page on another origin
/// can make the browser send the session cookie, but cannot read this one.
pub const CSRF_COOKIE: &str = "rimaia_csrf";
/// The OAuth `state` of a sign-in this browser started, which refuses a
/// callback started in someone else's browser (login CSRF).
pub const SIGNIN_COOKIE: &str = "rimaia_signin";
/// The header a cookie request carries its CSRF token in.
pub const CSRF_HEADER: &str = "x-rimaia-csrf";

/// The browser maximum, 400 days. The server's 30-day idle rule is the expiry
/// that counts; this only keeps the browser from dropping a live session.
const SESSION_COOKIE_SECONDS: u64 = 400 * 24 * 60 * 60;
/// A sign-in's ten minutes.
const SIGNIN_COOKIE_SECONDS: u64 = 10 * 60;

/// What a desktop token is called when `/auth/desktop` was given no label.
pub const DEFAULT_DESKTOP_LABEL: &str = "Desktop app";

/// What a desktop's unknown, used or mismatched code is told.
const DESKTOP_CODE_INVALID: &str =
    "This sign-in has expired or was already used. Sign in again from the desktop app.";

/// The three browser-navigated routes, at the root.
pub fn routes() -> Router<ServerState> {
    Router::new()
        .route("/auth/github", get(github_sign_in))
        .route("/auth/desktop", get(desktop_sign_in))
        .route("/auth/github/callback", get(callback))
}

/// The two JSON routes, nested under `/api/v1` with the board's.
pub fn api_routes() -> Router<ServerState> {
    Router::new()
        .route("/auth/desktop_token", post(desktop_token))
        .route("/auth/pair", post(pair))
}

/// Where the provider returns a sign-in.
fn callback_url(state: &ServerState) -> String {
    format!("{}/auth/github/callback", state.public_url)
}

/// A cookie with the attributes every one of ours has: `Secure`, `Path=/`, no
/// `Domain`, and `SameSite=Lax`.
fn cookie(name: &'static str, value: String, http_only: bool, max_age: u64) -> Cookie<'static> {
    let mut cookie = Cookie::build((name, value))
        .path("/")
        .secure(true)
        .http_only(http_only)
        .same_site(SameSite::Lax)
        .build();
    // `cookie`'s duration type is `time`'s, which is not a dependency here;
    // converting a std duration names it by inference alone.
    cookie.set_max_age(std::time::Duration::from_secs(max_age).try_into().ok());
    cookie
}

/// Counts one attempt by the peer, or refuses it.
fn limit(state: &ServerState, bucket: Bucket, peer: SocketAddr) -> Result<(), ApiError> {
    state
        .rate_limits
        .check(bucket, &peer.ip().to_string())
        .map_err(ApiError)
}

/// Records a pending sign-in, sets the sign-in cookie to its `state`, and
/// sends the browser to the provider.
fn start(state: &ServerState, jar: CookieJar, flow: SignInFlow) -> (CookieJar, Redirect) {
    let oauth_state = secret::mint_session_secret();
    let verifier = secret::mint_verifier();
    let challenge = secret::pkce_challenge(verifier.expose());
    state.pending.insert(oauth_state.expose(), verifier, flow);
    let to = state
        .identity
        .authorize_url(oauth_state.expose(), &challenge, &callback_url(state));
    let jar = jar.add(cookie(
        SIGNIN_COOKIE,
        oauth_state.expose().to_string(),
        true,
        SIGNIN_COOKIE_SECONDS,
    ));
    (jar, Redirect::to(&to))
}

async fn github_sign_in(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), ApiError> {
    limit(&state, Bucket::SignIn, peer)?;
    Ok(start(&state, jar, SignInFlow::Browser))
}

/// `/auth/desktop`'s query, every field optional so a missing one is refused
/// in the error shape rather than by axum's plain-text rejection.
#[derive(Debug, Default, Deserialize)]
struct DesktopQuery {
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    label: Option<String>,
}

async fn desktop_sign_in(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    query: Result<Query<DesktopQuery>, QueryRejection>,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), ApiError> {
    limit(&state, Bucket::SignIn, peer)?;
    let Query(query) = query.map_err(|rejection| Error::invalid(rejection.body_text()))?;
    let flow = desktop_flow(query)?;
    Ok(start(&state, jar, flow))
}

/// The desktop's request, checked before the provider is contacted: a
/// loopback redirect on `127.0.0.1` with a port and the `/callback` path (RFC
/// 8252 section 7.3), an S256 challenge, a `state`, and a label held to 1 to
/// 100 characters.
fn desktop_flow(query: DesktopQuery) -> Result<SignInFlow, Error> {
    let redirect_uri = query
        .redirect_uri
        .filter(|uri| is_loopback_callback(uri))
        .ok_or_else(|| {
            Error::invalid("The desktop's redirect must be http://127.0.0.1:<port>/callback.")
        })?;
    if query.code_challenge_method.as_deref() != Some("S256") {
        return Err(Error::invalid(
            "The desktop's sign-in needs a PKCE challenge with the S256 method.",
        ));
    }
    let desktop_challenge = query
        .code_challenge
        .filter(|challenge| is_challenge(challenge))
        .ok_or_else(|| {
            Error::invalid("The desktop's sign-in needs a PKCE challenge with the S256 method.")
        })?;
    let desktop_state = query
        .state
        .filter(|desktop_state| !desktop_state.is_empty())
        .ok_or_else(|| Error::invalid("The desktop's sign-in needs a state."))?;
    let label = secret::label(query.label.as_deref().unwrap_or(DEFAULT_DESKTOP_LABEL))?;
    Ok(SignInFlow::Desktop {
        redirect_uri,
        desktop_state,
        desktop_challenge,
        label,
    })
}

/// `http://127.0.0.1:<port>/callback`, exactly: not `localhost`, which a
/// hosts file can point anywhere, and not `https`, which a loopback listener
/// does not serve.
fn is_loopback_callback(uri: &str) -> bool {
    let Ok(url) = Url::parse(uri) else {
        return false;
    };
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some()
        && url.path() == "/callback"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

/// An S256 challenge's shape: 43 base64url characters.
fn is_challenge(challenge: &str) -> bool {
    challenge.len() == 43
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[derive(Debug, Default, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// How a callback ended, as the web app's `?sign_in=` vocabulary.
#[derive(Debug, Clone, Copy)]
enum SignInFailure {
    /// The person declined at the provider.
    Denied,
    /// The sign-in was older than ten minutes, already used, or started
    /// before a restart.
    Expired,
    /// Anything else, a callback started in another browser included.
    Failed,
}

impl SignInFailure {
    fn redirect(self, jar: CookieJar) -> (CookieJar, Redirect) {
        let to = match self {
            SignInFailure::Denied => "/?sign_in=denied",
            SignInFailure::Expired => "/?sign_in=expired",
            SignInFailure::Failed => "/?sign_in=failed",
        };
        (clear_signin(jar), Redirect::to(to))
    }
}

fn clear_signin(jar: CookieJar) -> CookieJar {
    jar.remove(Cookie::build(SIGNIN_COOKIE).path("/"))
}

async fn callback(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    query: Result<Query<CallbackQuery>, QueryRejection>,
    headers: HeaderMap,
    jar: CookieJar,
) -> Result<(CookieJar, Redirect), ApiError> {
    limit(&state, Bucket::SignIn, peer)?;
    let Ok(Query(query)) = query else {
        return Ok(SignInFailure::Failed.redirect(jar));
    };
    match complete(&state, query, &headers, &jar).await {
        Ok(Completed::Browser { secret: session }) => {
            let csrf = secret::csrf_for(session.expose());
            let jar = clear_signin(jar)
                .add(cookie(
                    SESSION_COOKIE,
                    session.expose().to_string(),
                    true,
                    SESSION_COOKIE_SECONDS,
                ))
                .add(cookie(CSRF_COOKIE, csrf, false, SESSION_COOKIE_SECONDS));
            Ok((jar, Redirect::to("/")))
        }
        Ok(Completed::Desktop { to }) => Ok((clear_signin(jar), Redirect::to(&to))),
        Err(failure) => {
            tracing::info!(failure = ?failure, "a sign-in did not complete");
            Ok(failure.redirect(jar))
        }
    }
}

enum Completed {
    Browser { secret: secret::Secret },
    Desktop { to: String },
}

/// The callback's work, after the rate limit: the `state` against this
/// browser's sign-in cookie, the pending entry, the exchange, the user, and
/// then a session or a desktop code.
async fn complete(
    state: &ServerState,
    query: CallbackQuery,
    headers: &HeaderMap,
    jar: &CookieJar,
) -> Result<Completed, SignInFailure> {
    if let Some(error) = query.error {
        return Err(if error == "access_denied" {
            SignInFailure::Denied
        } else {
            SignInFailure::Failed
        });
    }
    let (Some(code), Some(oauth_state)) = (query.code, query.state) else {
        return Err(SignInFailure::Failed);
    };
    let started_here = jar
        .get(SIGNIN_COOKIE)
        .is_some_and(|cookie| secret::constant_time_eq(cookie.value(), &oauth_state));
    if !started_here {
        return Err(SignInFailure::Failed);
    }
    let pending = state
        .pending
        .take(&oauth_state)
        .ok_or(SignInFailure::Expired)?;

    let identity = state
        .identity
        .exchange(&code, pending.verifier.expose(), &callback_url(state))
        .await
        .map_err(|error| {
            tracing::warn!(%error, "the identity provider refused a sign-in");
            SignInFailure::Failed
        })?;
    let ctx = &state.host.context;
    let user_id = upsert_user(
        &ctx.pool,
        ctx.clock.as_ref(),
        state.identity.id(),
        &identity,
    )
    .await
    .map_err(|error| {
        tracing::warn!(%error, "a signed-in user could not be recorded");
        SignInFailure::Failed
    })?;

    match pending.flow {
        SignInFlow::Browser => {
            let user_agent = headers
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok());
            let (_, session) =
                sessions::create(&ctx.pool, ctx.clock.as_ref(), &user_id, user_agent)
                    .await
                    .map_err(|error| {
                        tracing::warn!(%error, "a session could not be created");
                        SignInFailure::Failed
                    })?;
            Ok(Completed::Browser { secret: session })
        }
        SignInFlow::Desktop {
            redirect_uri,
            desktop_state,
            desktop_challenge,
            label,
        } => {
            let code =
                state
                    .desktop_codes
                    .mint(&user_id, &desktop_challenge, &redirect_uri, &label);
            let to = Url::parse_with_params(
                &redirect_uri,
                &[("code", code.expose()), ("state", desktop_state.as_str())],
            )
            .map_err(|_| SignInFailure::Failed)?;
            Ok(Completed::Desktop { to: to.into() })
        }
    }
}

/// A JSON body parsed by hand, as the board routes parse theirs: a missing
/// `Content-Type` is fine, and malformed JSON is `invalid` in the error shape.
fn body<T: DeserializeOwned>(body: Result<Bytes, BytesRejection>) -> Result<T, Error> {
    let body = body.map_err(|rejection| Error::invalid(rejection.body_text()))?;
    serde_json::from_slice(&body)
        .map_err(|error| Error::invalid(format!("the request body could not be read: {error}")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopTokenRequest {
    code: String,
    code_verifier: String,
    redirect_uri: String,
}

/// The desktop's half of ADR-0030 point 4: its one-time code, the verifier of
/// the challenge it sent to `/auth/desktop`, and the same redirect, for a
/// desktop token labelled as it asked.
async fn desktop_token(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ProtocolHeader(protocol_header): ProtocolHeader,
    request: Result<Bytes, BytesRejection>,
) -> Result<Json<NewApiToken>, ApiError> {
    limit(&state, Bucket::DesktopToken, peer)?;
    protocol::check(protocol_header.as_deref(), Effect::Write)?;
    let request: DesktopTokenRequest = body(request)?;

    // Taken first, so a wrong verifier spends the code as a right one would.
    let pending = state
        .desktop_codes
        .take(&request.code)
        .ok_or_else(|| Error::invalid(DESKTOP_CODE_INVALID))?;
    let verified = secret::constant_time_eq(
        &secret::pkce_challenge(&request.code_verifier),
        &pending.desktop_challenge,
    );
    if !verified || request.redirect_uri != pending.redirect_uri {
        return Err(Error::invalid(DESKTOP_CODE_INVALID).into());
    }
    let ctx = &state.host.context;
    let token = tokens::create_desktop(
        &ctx.pool,
        ctx.clock.as_ref(),
        &pending.user_id,
        &pending.label,
    )
    .await?;
    Ok(Json(token))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    code: String,
    label: String,
    provider: String,
}

/// `rimaia-runner pair`'s call (task 058): a pairing code for a runner and its
/// token.
async fn pair(
    State(state): State<ServerState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ProtocolHeader(protocol_header): ProtocolHeader,
    request: Result<Bytes, BytesRejection>,
) -> Result<Json<PairedRunner>, ApiError> {
    limit(&state, Bucket::Pair, peer)?;
    protocol::check(protocol_header.as_deref(), Effect::Write)?;
    let request: PairRequest = body(request)?;
    let ctx = &state.host.context;
    let paired = pairing::redeem(
        &ctx.pool,
        ctx.clock.as_ref(),
        &request.code,
        &request.label,
        &request.provider,
    )
    .await?;
    Ok(Json(paired))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_loopback_callback_on_127_0_0_1_with_a_port_is_a_desktop_redirect() {
        assert!(is_loopback_callback("http://127.0.0.1:5555/callback"));
        for refused in [
            "https://127.0.0.1:5555/callback",
            "http://localhost:5555/callback",
            "http://127.0.0.2:5555/callback",
            "http://evil.test:5555/callback",
            "http://127.0.0.1/callback",
            "http://127.0.0.1:5555/elsewhere",
            "http://127.0.0.1:5555/callback?next=x",
            "http://user@127.0.0.1:5555/callback",
            "not a url",
        ] {
            assert!(!is_loopback_callback(refused), "{refused}");
        }
    }
}
