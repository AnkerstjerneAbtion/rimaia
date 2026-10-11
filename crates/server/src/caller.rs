//! Who is asking, from the request (seam-contract D32 point 7).
//!
//! Every board route takes a [`Caller`] as its first extractor. A request
//! carries the `rimaia_session` cookie with its `X-Rimaia-CSRF` header, or
//! `Authorization: Bearer <token>`, never both. The server's
//! [`Authenticate`](rimaia_core::api::Authenticate) turns the credential into
//! a caller, and the pair of credential shape and door must be one the board
//! surface accepts: a browser's session, or a connected desktop's token. Any
//! other request is `unauthenticated`, in the one sentence every refusal has.

use std::fmt;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap};
use axum_extra::extract::cookie::CookieJar;
use rimaia_core::api::{Authenticate, Caller, Credential, Door};
use rimaia_core::Error;

use crate::auth::{CSRF_HEADER, SESSION_COOKIE};
use crate::{ApiError, ServerState};

impl FromRequestParts<ServerState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        let presented = Presented::from_headers(&parts.headers)?;
        Ok(presented.resolve(state.auth.as_ref()).await?)
    }
}

/// What a request presented, owned: its credential and its `User-Agent`.
///
/// Owned rather than borrowed out of the request, because a stream resolves
/// its caller again for as long as it is open (task 048), long after the
/// request's headers are gone. Held in memory for that stream's life and
/// never logged: its `Debug` is redacted, as [`Credential`]'s is.
#[derive(Clone)]
pub struct Presented {
    credential: HeldCredential,
    user_agent: Option<String>,
}

#[derive(Clone)]
enum HeldCredential {
    Session {
        secret: String,
        csrf: Option<String>,
    },
    Bearer(String),
}

impl Presented {
    /// The one credential `headers` carry: the session cookie with its CSRF
    /// header, or a bearer, never both and never neither.
    fn from_headers(headers: &HeaderMap) -> Result<Self, Error> {
        let bearer = bearer(headers)?;
        let jar = CookieJar::from_headers(headers);
        let session = jar.get(SESSION_COOKIE).map(|cookie| cookie.value());
        let credential = match (bearer, session) {
            (Some(_), Some(_)) | (None, None) => return Err(Error::unauthenticated()),
            (Some(token), None) => HeldCredential::Bearer(token.to_string()),
            // A missing header still reaches `Authenticate`, which refuses it
            // in the same constant-time comparison as a wrong one.
            (None, Some(secret)) => HeldCredential::Session {
                secret: secret.to_string(),
                csrf: text(headers, CSRF_HEADER).map(str::to_string),
            },
        };
        Ok(Self {
            credential,
            user_agent: text(headers, header::USER_AGENT.as_str()).map(str::to_string),
        })
    }

    /// The board surface's whole caller resolution: `Authenticate`, then the
    /// door check. What a board route runs once, and what a stream runs again
    /// each time it revalidates, so the two cannot drift.
    pub async fn resolve(&self, auth: &dyn Authenticate) -> Result<Caller, Error> {
        let credential = self.credential();
        let caller = auth
            .authenticate_from(credential, self.user_agent.as_deref())
            .await?;
        accepts(&credential, &caller.door)?;
        Ok(caller)
    }

    fn credential(&self) -> Credential<'_> {
        match &self.credential {
            HeldCredential::Session { secret, csrf } => Credential::Session {
                secret,
                csrf: csrf.as_deref(),
            },
            HeldCredential::Bearer(token) => Credential::Bearer(token),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Presented {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_headers(&parts.headers)?)
    }
}

impl fmt::Debug for Presented {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Presented")
            .field("credential", &self.credential())
            .finish_non_exhaustive()
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
