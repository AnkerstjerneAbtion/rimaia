//! Who is asking, from the request (seam-contract D32 point 7).
//!
//! Every board route takes a [`Caller`] as its first extractor. A request
//! carries `Authorization: Bearer <token>` or the `rimaia_session` cookie, never
//! both; the server's [`Authenticate`](rimaia_core::api::Authenticate) turns
//! the credential into a caller, and the pair of credential shape and door must
//! be one the board surface accepts. Any other request is `unauthenticated`.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap};
use rimaia_core::api::{Caller, Credential, Door};
use rimaia_core::Error;

use crate::{ApiError, ServerState};

/// The session cookie's name (D32 point 7). Nothing reads its value until task
/// 047 lands sessions.
pub const SESSION_COOKIE: &str = "rimaia_session";

impl FromRequestParts<ServerState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        let bearer = bearer(&parts.headers)?;
        let session = has_session_cookie(&parts.headers);
        let credential = match (bearer, session) {
            (Some(_), true) => {
                return Err(Error::unauthenticated(
                    "a request carries a session or a bearer token, not both",
                )
                .into())
            }
            (None, true) => {
                return Err(Error::unauthenticated(
                    "this server does not accept a session yet; send a bearer token",
                )
                .into())
            }
            (None, false) => {
                return Err(Error::unauthenticated("this request carries no credential").into())
            }
            (Some(token), false) => Credential::Bearer(token),
        };
        let caller = state.auth.authenticate(credential).await?;
        accepts(&credential, &caller.door)?;
        Ok(caller)
    }
}

/// Whether the board surface accepts a caller of `door` who presented
/// `credential` (D32 point 7's table). A browser reaches the board only with a
/// session, whose CSRF header the session's `Authenticate` checks; a bearer
/// only as a connected desktop. A runner's, an MCP token's or a shell's caller
/// cannot drive board commands over HTTP, and neither can a bearer some
/// `Authenticate` resolves to a browser.
fn accepts(credential: &Credential<'_>, door: &Door) -> Result<(), Error> {
    match (credential, door) {
        (Credential::Bearer(_), Door::Desktop { .. }) => Ok(()),
        // Reachable once task 047 hands a session to `Authenticate`.
        (Credential::Session { .. }, Door::Browser { .. }) => Ok(()),
        _ => Err(Error::unauthenticated(
            "this credential cannot drive board commands",
        )),
    }
}

/// The token of an `Authorization: Bearer <token>` header, `None` with no
/// header, and `unauthenticated` for any other `Authorization`.
fn bearer(headers: &HeaderMap) -> Result<Option<&str>, Error> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(Some)
        .ok_or_else(|| Error::unauthenticated("an Authorization header must be `Bearer <token>`"))
}

/// Whether any `Cookie` header names the session cookie.
fn has_session_cookie(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.split_once('='))
        .any(|(name, _)| name.trim() == SESSION_COOKIE)
}
