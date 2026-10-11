//! Who a credential is, for real (seam-contract D32 point 7, ADR-0030).
//!
//! [`SessionsAndTokens`] is the server's production [`Authenticate`]. It turns
//! a browser session or a bearer token into a [`Caller`] whose teams are read
//! from `team_memberships` on this request, never from a cache on the session
//! or the token: removing someone from a team takes effect on their next
//! request, which is why ADR-0030 chose server-side sessions over JWTs.
//!
//! **Every failure is the same `unauthenticated`**, `Sign in to continue.`
//! Unknown, expired, revoked, wrong kind, unpaired, no reachable team and a
//! missing or wrong CSRF header are indistinguishable to the caller. The span
//! records which it was, and never the credential.
//!
//! **A hash lookup is not a timing oracle for the secret.** The database
//! compares hashes of 256-bit secrets, and learning a prefix of a hash teaches
//! nothing about the secret. The one secret compared in memory here, the CSRF
//! header, goes through [`secret::csrf_matches`], which is constant time.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tracing::Instrument;

use super::secret::{self, TokenKind};
use super::sessions;
use super::Role;
use crate::api::{Authenticate, BoxFuture, Caller, Credential, Door, TeamGrant};
use crate::clock::Clock;
use crate::error::{Error, Result};

/// Sessions and hashed tokens, over the board's store.
#[derive(Clone)]
pub struct SessionsAndTokens {
    pub pool: SqlitePool,
    pub clock: Arc<dyn Clock>,
}

/// Why a credential was refused: what the span records, and nothing a caller
/// sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    UnknownSession,
    IdleExpired,
    MissingCsrf,
    WrongCsrf,
    UnknownPrefix,
    UnknownToken,
    WrongKind,
    Expired,
    Unpaired,
    NoTeam,
}

impl Refusal {
    fn as_str(self) -> &'static str {
        match self {
            Refusal::UnknownSession => "unknown_session",
            Refusal::IdleExpired => "idle_expired",
            Refusal::MissingCsrf => "missing_csrf",
            Refusal::WrongCsrf => "wrong_csrf",
            Refusal::UnknownPrefix => "unknown_prefix",
            Refusal::UnknownToken => "unknown_token",
            Refusal::WrongKind => "wrong_kind",
            Refusal::Expired => "expired",
            Refusal::Unpaired => "unpaired",
            Refusal::NoTeam => "no_team",
        }
    }
}

/// A credential's answer before it is collapsed into one refusal: a store
/// failure stays what it is, a refusal says why.
type Outcome = Result<std::result::Result<Caller, Refusal>>;

impl Authenticate for SessionsAndTokens {
    fn authenticate<'a>(&'a self, credential: Credential<'a>) -> BoxFuture<'a, Result<Caller>> {
        self.authenticate_from(credential, None)
    }

    fn authenticate_from<'a>(
        &'a self,
        credential: Credential<'a>,
        user_agent: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Caller>> {
        let span = tracing::info_span!(
            "authenticate",
            credential = match credential {
                Credential::Session { .. } => "session",
                Credential::Bearer(_) => "bearer",
            },
            refused = tracing::field::Empty,
        );
        Box::pin(
            async move {
                let outcome = match credential {
                    Credential::Session { secret, csrf } => self.session(secret, csrf).await,
                    Credential::Bearer(token) => self.bearer(token, user_agent).await,
                };
                match outcome? {
                    Ok(caller) => Ok(caller),
                    Err(refusal) => {
                        tracing::Span::current().record("refused", refusal.as_str());
                        Err(Error::unauthenticated())
                    }
                }
            }
            .instrument(span),
        )
    }
}

impl SessionsAndTokens {
    /// `Credential::Session`: look up the secret's hash, apply idle expiry,
    /// check the CSRF header, touch, and answer the browser's door.
    async fn session(&self, session_secret: &str, csrf: Option<&str>) -> Outcome {
        let now = self.clock.now();
        let secret_hash = secret::hash(session_secret);
        let Some(session) = sqlx::query!(
            r#"SELECT id, user_id, last_used_at AS "last_used_at: DateTime<Utc>"
                 FROM sessions WHERE secret_hash = ?1"#,
            secret_hash,
        )
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(Err(Refusal::UnknownSession));
        };

        if sessions::is_idle_expired(session.last_used_at, now) {
            sqlx::query!("DELETE FROM sessions WHERE id = ?1", session.id)
                .execute(&self.pool)
                .await?;
            return Ok(Err(Refusal::IdleExpired));
        }
        // On reads as well as writes: every command is a POST, and no route
        // can be the one that was forgotten (D32's Why, point 7).
        let Some(csrf) = csrf else {
            return Ok(Err(Refusal::MissingCsrf));
        };
        if !secret::csrf_matches(session_secret, csrf) {
            return Ok(Err(Refusal::WrongCsrf));
        }
        if sessions::needs_touch(session.last_used_at, now) {
            sqlx::query!(
                "UPDATE sessions SET last_used_at = ?1 WHERE id = ?2",
                now,
                session.id,
            )
            .execute(&self.pool)
            .await?;
        }

        let teams = self.teams(&session.user_id, None).await?;
        Ok(self.caller(
            session.user_id,
            teams,
            Door::Browser {
                session_id: session.id,
            },
        ))
    }

    /// `Credential::Bearer`: the prefix names the kind; look up the token's
    /// hash; refuse a kind that disagrees, an expired token and an unpaired
    /// runner's; touch, and answer the kind's door.
    async fn bearer(&self, token: &str, user_agent: Option<&str>) -> Outcome {
        let now = self.clock.now();
        let Some(kind) = TokenKind::of_token(token) else {
            return Ok(Err(Refusal::UnknownPrefix));
        };
        let secret_hash = secret::hash(token);
        let Some(row) = sqlx::query!(
            r#"SELECT t.id, t.user_id, t.kind, t.runner_id,
                      t.last_used_at AS "last_used_at: DateTime<Utc>",
                      t.last_used_from,
                      t.expires_at AS "expires_at: DateTime<Utc>",
                      r.unpaired_at AS "unpaired_at?: DateTime<Utc>"
                 FROM api_tokens t LEFT JOIN runners r ON r.id = t.runner_id
                WHERE t.secret_hash = ?1"#,
            secret_hash,
        )
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(Err(Refusal::UnknownToken));
        };

        if TokenKind::parse(&row.kind) != Some(kind) {
            return Ok(Err(Refusal::WrongKind));
        }
        if row.expires_at.is_some_and(|expires_at| now >= expires_at) {
            return Ok(Err(Refusal::Expired));
        }
        if row.unpaired_at.is_some() {
            return Ok(Err(Refusal::Unpaired));
        }

        // Once an hour, or at once when the token turns up somewhere new: a
        // runner heartbeating every minute is not a write a minute.
        let from = secret::truncate_user_agent(user_agent);
        let moved = from.is_some() && from != row.last_used_from;
        let stale = row
            .last_used_at
            .is_none_or(|last_used_at| sessions::needs_touch(last_used_at, now));
        if moved || stale {
            let from = from.or(row.last_used_from);
            sqlx::query!(
                "UPDATE api_tokens SET last_used_at = ?1, last_used_from = ?2 WHERE id = ?3",
                now,
                from,
                row.id,
            )
            .execute(&self.pool)
            .await?;
        }

        let door = match kind {
            TokenKind::Desktop => Door::Desktop {
                token_id: row.id.clone(),
            },
            TokenKind::Personal => Door::Mcp {
                token_id: Some(row.id.clone()),
            },
            TokenKind::Runner => match row.runner_id {
                Some(runner_id) => Door::Runner { runner_id },
                None => return Ok(Err(Refusal::WrongKind)),
            },
        };
        let restriction = (kind == TokenKind::Personal).then_some(row.id.as_str());
        let teams = self.teams(&row.user_id, restriction).await?;
        Ok(self.caller(row.user_id, teams, door))
    }

    /// `user_id`'s memberships, read now. For a personal access token with a
    /// team restriction, only the teams it names that the user still belongs
    /// to.
    async fn teams(&self, user_id: &str, token_id: Option<&str>) -> Result<Vec<TeamGrant>> {
        let memberships = sqlx::query!(
            "SELECT team_id, role FROM team_memberships WHERE user_id = ?1 ORDER BY team_id",
            user_id,
        )
        .fetch_all(&self.pool)
        .await?;
        let restriction: Vec<String> = match token_id {
            Some(token_id) => {
                sqlx::query_scalar!(
                    "SELECT team_id FROM api_token_teams WHERE token_id = ?1",
                    token_id,
                )
                .fetch_all(&self.pool)
                .await?
            }
            None => Vec::new(),
        };
        memberships
            .into_iter()
            .filter(|membership| {
                restriction.is_empty() || restriction.contains(&membership.team_id)
            })
            .map(|membership| {
                Ok(TeamGrant {
                    role: membership.role.parse::<Role>()?,
                    team_id: membership.team_id,
                })
            })
            .collect()
    }

    /// The caller, or a refusal when it reaches no team: a restricted token
    /// whose user has left every team it names gets the same answer as a
    /// revoked one, rather than `for_caller`'s `invalid`.
    fn caller(
        &self,
        user_id: String,
        teams: Vec<TeamGrant>,
        door: Door,
    ) -> std::result::Result<Caller, Refusal> {
        if teams.is_empty() {
            return Err(Refusal::NoTeam);
        }
        Ok(Caller {
            user_id,
            teams,
            door,
        })
    }
}
