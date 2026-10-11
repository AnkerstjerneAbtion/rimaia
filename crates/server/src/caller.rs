//! Who is asking, from the request (seam-contract D32 point 7).
//!
//! Every board route takes a [`Caller`] as its first extractor. A request
//! carries the `rimaia_session` cookie with its `X-Rimaia-CSRF` header, or
//! `Authorization: Bearer <token>`, never both. The server's
//! [`Authenticate`](rimaia_core::api::Authenticate) turns the credential into
//! a caller, and the pair of credential shape and door must be one the board
//! surface accepts: a browser's session, or a connected desktop's token. Any
//! other request is `unauthenticated`, in the one sentence every refusal has.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap};
use axum_extra::extract::cookie::CookieJar;
use rimaia_core::api::{Caller, Credential, Door};
use rimaia_core::Error;

use crate::auth::{CSRF_HEADER, SESSION_COOKIE};
use crate::{ApiError, ServerState};

impl FromRequestParts<ServerState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        let bearer = bearer(&parts.headers)?;
        let jar = CookieJar::from_headers(&parts.headers);
        let session = jar.get(SESSION_COOKIE).map(|cookie| cookie.value());
        let csrf = text(&parts.headers, CSRF_HEADER);
        let credential = match (bearer, session) {
            (Some(_), Some(_)) | (None, None) => return Err(Error::unauthenticated().into()),
            (Some(token), None) => Credential::Bearer(token),
            // A missing header still reaches `Authenticate`, which refuses it
            // in the same constant-time comparison as a wrong one.
            (None, Some(secret)) => Credential::Session { secret, csrf },
        };
        let user_agent = text(&parts.headers, header::USER_AGENT.as_str());
        let caller = state.auth.authenticate_from(credential, user_agent).await?;
        accepts(&credential, &caller.door)?;
        Ok(caller)
    }
}

/// Whether the board surface accepts a caller of `door` who presented
/// `credential` (D32 point 7's table). A browser reaches the board only with a
/// session, whose CSRF header `Authenticate` checks; a bearer only as a
/// connected desktop. A runner's, an MCP token's or a shell's caller cannot
/// drive board commands over HTTP, and neither can a bearer some
/// `Authenticate` resolves to a browser.
fn accepts(credential: &Credential<'_>, door: &Door) -> Result<(), Error> {
    match (credential, door) {
        (Credential::Bearer(_), Door::Desktop { .. })
        | (Credential::Session { .. }, Door::Browser { .. }) => Ok(()),
        _ => Err(Error::unauthenticated()),
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
        .ok_or_else(Error::unauthenticated)
}

/// A header's value, if it is text.
fn text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}
