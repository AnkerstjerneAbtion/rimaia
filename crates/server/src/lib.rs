//! The board over HTTP (ADR-0034, seam-contract D32).
//!
//! [`router`] mounts `POST /api/v1/<name>` for every board row of
//! `rimaia_core::api::registry::COMMANDS` and nothing else. Each route takes a
//! [`Caller`] as its first extractor, so no route can be mounted without one,
//! then the `Rimaia-Protocol` header, then the body, and answers through
//! `api::dispatch`: the function the solo shell's `invoke` reaches too, so the
//! two transports answer alike by construction. This crate is only the
//! transport.
//!
//! Until task 047 the only production [`Authenticate`] is
//! [`RefuseAll`](rimaia_core::api::RefuseAll), so a built server answers every
//! board command `401 unauthenticated`: it fails closed.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{FromRequestParts, MatchedPath, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use rimaia_core::api::{self, protocol, Authenticate, BoardHost, Caller, Effect, COMMANDS};
use rimaia_core::{Error, ErrorCode};
use serde_json::Value;
use tower_http::trace::TraceLayer;

mod caller;
pub mod config;

/// What every request reaches: the board host `dispatch` runs against, and
/// whatever decides who a credential is.
#[derive(Clone)]
pub struct ServerState {
    pub host: BoardHost,
    pub auth: Arc<dyn Authenticate>,
}

/// One route per board row, nested under `/api/v1`, whose fallbacks answer in
/// the error shape: a name with no route is `404 not_found` and a method other
/// than `POST` is `400 invalid`, never axum's plain text and never a page.
pub fn router(state: ServerState) -> Router {
    let mut commands = Router::new();
    for command in COMMANDS.iter() {
        let Some(effect) = command.effect() else {
            continue;
        };
        let name = command.name;
        commands = commands.route(
            &format!("/{name}"),
            post(
                move |caller: Caller,
                      protocol_header: ProtocolHeader,
                      State(state): State<ServerState>,
                      body: Result<Bytes, BytesRejection>| {
                    run(caller, protocol_header, state, name, effect, body)
                },
            ),
        );
    }
    let commands = commands
        // The matched route, the method and the status, and never a body, a
        // header value or a query string: bodies carry plans (ADR-0037 point 6).
        .route_layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<_>| {
                let route = request
                    .extensions()
                    .get::<MatchedPath>()
                    .map(|path| path.as_str().to_string())
                    .unwrap_or_default();
                tracing::info_span!("http", method = %request.method(), route)
            }),
        )
        .fallback(|| async { ApiError(Error::not_found("there is no such board command")) })
        .method_not_allowed_fallback(|| async {
            ApiError(Error::invalid("a board command is sent with POST"))
        });
    Router::new().nest("/api/v1", commands).with_state(state)
}

/// The route handler, after the caller is known.
async fn run(
    caller: Caller,
    ProtocolHeader(header): ProtocolHeader,
    state: ServerState,
    name: &'static str,
    effect: Effect,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<Value>, ApiError> {
    protocol::check(header.as_deref(), effect)?;
    let body = body.map_err(|rejection| Error::invalid(rejection.body_text()))?;
    // Parsed here rather than by an extractor, so a missing `Content-Type` is
    // fine and malformed JSON is `invalid` in the error shape.
    let args = if body.is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_slice(&body)
            .map_err(|error| Error::invalid(format!("the request body is not JSON: {error}")))?
    };
    // A unit output is `null` with `200`, so the transport reads every success
    // the same way.
    Ok(Json(api::dispatch(&state.host, caller, name, args).await?))
}

/// The `Rimaia-Protocol` header's value, if there is one that is text.
pub struct ProtocolHeader(pub Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for ProtocolHeader {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .headers
                .get(protocol::HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
        ))
    }
}

/// D32 point 3's table: the status a code is answered with. The body is
/// always the error's `{ code, message }`, which is what a client branches
/// on; the status is for proxies, logs and metrics. No code is a 403: an
/// entity outside the caller's teams does not exist (ADR-0029 point 5).
pub fn status_for(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::Invalid => StatusCode::BAD_REQUEST,
        ErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Conflict => StatusCode::CONFLICT,
        ErrorCode::UpgradeRequired => StatusCode::UPGRADE_REQUIRED,
        ErrorCode::Database | ErrorCode::Io | ErrorCode::Internal => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// An error as a response: [`status_for`] its code, the error shape as the
/// body, and `WWW-Authenticate: Bearer` on a `401`.
pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = self.0.code();
        let mut response = (status_for(code), Json(self.0)).into_response();
        if code == ErrorCode::Unauthenticated {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn every_error_code_has_one_status_and_none_is_403() {
        let table: Vec<(ErrorCode, u16)> = ErrorCode::ALL
            .iter()
            .map(|code| (*code, status_for(*code).as_u16()))
            .collect();

        assert_eq!(
            table,
            vec![
                (ErrorCode::Database, 500),
                (ErrorCode::Io, 500),
                (ErrorCode::NotFound, 404),
                (ErrorCode::Invalid, 400),
                (ErrorCode::Conflict, 409),
                (ErrorCode::Unauthenticated, 401),
                (ErrorCode::UpgradeRequired, 426),
                (ErrorCode::Internal, 500),
            ]
        );
        assert!(table.iter().all(|(_, status)| *status != 403));
    }

    #[test]
    fn only_a_401_carries_www_authenticate() {
        for code in ErrorCode::ALL {
            let error = match code {
                ErrorCode::Unauthenticated => Error::unauthenticated("no"),
                ErrorCode::UpgradeRequired => Error::upgrade_required("no"),
                ErrorCode::NotFound => Error::not_found("no"),
                ErrorCode::Conflict => Error::conflict("no"),
                ErrorCode::Invalid => Error::invalid("no"),
                ErrorCode::Database | ErrorCode::Io | ErrorCode::Internal => {
                    continue;
                }
            };
            let response = ApiError(error).into_response();
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE),
                (*code == ErrorCode::Unauthenticated)
                    .then(|| HeaderValue::from_static("Bearer"))
                    .as_ref(),
                "{code:?}"
            );
        }
    }

    /// ADR-0027 point 6. Cargo refuses a cycle, not a sibling edge, so this
    /// and `scripts/check-crate-boundaries.sh`'s transitive answer are what
    /// keep the runner and the desktop shell out of the server.
    #[test]
    fn rimaia_server_does_not_depend_on_rimaia_runner() {
        let manifest = include_str!("../Cargo.toml");

        let mut in_dependencies = false;
        let mut named = Vec::new();
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_dependencies = line
                    .trim_matches(|c| c == '[' || c == ']')
                    .ends_with("dependencies");
                continue;
            }
            if !in_dependencies || line.starts_with('#') || line.is_empty() {
                continue;
            }
            let key = line.split('=').next().unwrap_or_default().trim();
            if key == "rimaia-runner"
                || key == "rimaia_runner"
                || key.starts_with("tauri")
                || line.contains("crates/runner")
                || line.contains("src-tauri")
            {
                named.push(line.to_string());
            }
        }

        assert_eq!(named, Vec::<String>::new());
    }
}
