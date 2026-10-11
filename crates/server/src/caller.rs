//! Who is asking, from the request (seam-contract D32 point 7).
//!
//! Every board route takes a [`Caller`] as its first extractor. A request
//! carries the `rimaia_session` cookie with its `X-Rimaia-CSRF` header, or
//! `Authorization: Bearer <token>`, never both. The server's
//! [`Authenticate`](rimaia_core::api::Authenticate) turns the credential into
//! a caller, and the pair of credential shape and door must be one the board
//! surface accepts: a browser's session, or a connected desktop's token. Any
//! other request is `unauthenticated`, in the one sentence every refusal has.
//!
//! A request may also carry `Rimaia-Team: <team id>` (task 050 Scope 3). On
//! every board route except the commands about the person
//! ([`ignores_team_header`]) and on the event stream, it narrows the caller to
//! that one team, once, after the credential and the door have been checked.
//! The rule is [`Caller::narrow_to`]'s; this module only reads the header.

use std::fmt;

use axum::extract::FromRequestParts;
use axum::extract::MatchedPath;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap};
use axum_extra::extract::cookie::CookieJar;
use rimaia_core::api::registry::ignores_team_header;
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
        let presented = Presented::from_parts(parts)?;
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
    /// The team the request named, kept only for a route it applies to.
    team: Option<String>,
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
    /// What the request to the route `parts` describes presented.
    fn from_parts(parts: &Parts) -> Result<Self, Error> {
        let command = parts
            .extensions
            .get::<MatchedPath>()
            .map(MatchedPath::as_str)
            .unwrap_or_else(|| parts.uri.path())
            .rsplit('/')
            .next()
            .unwrap_or_default();
        Self::from_headers(&parts.headers, command)
    }

    /// The one credential `headers` carry: the session cookie with its CSRF
    /// header, or a bearer, never both and never neither, and the team they
    /// name when `command` is one that header narrows.
    fn from_headers(headers: &HeaderMap, command: &str) -> Result<Self, Error> {
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
            team: team(headers, command)?,
        })
    }

    /// The board surface's whole caller resolution: `Authenticate`, the door
    /// check, then the narrowing to the named team. What a board route runs
    /// once, and what a stream runs again each time it revalidates, so the two
    /// cannot drift: a narrowed stream stays narrowed, and a caller who has
    /// lost the team gets `not_found` and the stream ends.
    pub async fn resolve(&self, auth: &dyn Authenticate) -> Result<Caller, Error> {
        let credential = self.credential();
        let caller = auth
            .authenticate_from(credential, self.user_agent.as_deref())
            .await?;
        accepts(&credential, &caller.door)?;
        match &self.team {
            Some(team_id) => caller.narrow_to(team_id),
            None => Ok(caller),
        }
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
        Ok(Self::from_parts(parts)?)
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

/// A paired runner, from its `rmr_` token and nothing else (seam-contract D32
/// point 7's table, task 052).
///
/// Every runner route takes it as its first extractor, so no runner route can
/// be mounted without one. It accepts only `Authorization: Bearer`, resolved
/// through the server's [`Authenticate`], and only when the answer is
/// [`Door::Runner`]. Anything else is `unauthenticated` in the one sentence:
/// a desktop or personal token, a session cookie with or without its CSRF
/// header, a cookie beside a bearer, or nothing. A revoked token and an
/// unpaired runner's are refused by `Authenticate` itself.
///
/// The runner's identity comes from here and from nowhere else (D31 point 3):
/// no body carries a runner id. It reads no `Rimaia-Team`: a lease names its
/// team (D40 point 4).
///
/// The field is private, so the extractor is the only way to build one, and
/// its door is always a runner's.
#[derive(Debug, Clone)]
pub struct RunnerCaller(Caller);

impl RunnerCaller {
    /// Who the runner works for, with every team they belong to, read on this
    /// request (ADR-0030 point 2).
    pub fn caller(&self) -> &Caller {
        &self.0
    }

    /// The runner the token was issued to.
    pub fn runner_id(&self) -> &str {
        match &self.0.door {
            Door::Runner { runner_id } => runner_id,
            door => unreachable!("a RunnerCaller is built only for a runner, not {door:?}"),
        }
    }
}

impl FromRequestParts<ServerState> for RunnerCaller {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        let headers = &parts.headers;
        if CookieJar::from_headers(headers)
            .get(SESSION_COOKIE)
            .is_some()
        {
            return Err(Error::unauthenticated().into());
        }
        let token = bearer(headers)?.ok_or_else(Error::unauthenticated)?;
        let caller = state
            .auth
            .authenticate_from(
                Credential::Bearer(token),
                text(headers, header::USER_AGENT.as_str()),
            )
            .await?;
        match caller.door {
            Door::Runner { .. } => Ok(Self(caller)),
            _ => Err(Error::unauthenticated().into()),
        }
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

/// The header naming the team on screen.
pub const TEAM_HEADER: &str = "rimaia-team";

/// The team `headers` name, unless `command` is about the person and not the
/// team ([`ignores_team_header`]). A header that is not text, or is empty, is
/// `invalid`: a client with no team chosen sends none.
fn team(headers: &HeaderMap, command: &str) -> Result<Option<String>, Error> {
    if ignores_team_header(command) {
        return Ok(None);
    }
    let Some(value) = headers.get(TEAM_HEADER) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|team_id| !team_id.is_empty())
        .map(|team_id| Some(team_id.to_string()))
        .ok_or_else(|| Error::invalid("Rimaia-Team must name a team id."))
}

/// A header's value, if it is text.
fn text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}
