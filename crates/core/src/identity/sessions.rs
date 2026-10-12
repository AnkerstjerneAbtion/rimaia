//! Browser sessions (ADR-0030 point 2).
//!
//! A session is a `sessions` row and a cookie holding a random secret whose
//! hash alone is stored. Every sign-in creates a new row with a new secret, so
//! no session id is ever accepted from the client (no fixation). Revoking
//! deletes the row, and signing out is revoking: the next request presents a
//! secret that no longer exists.
//!
//! **Idle expiry is 30 days from `last_used_at`.** A session is valid while
//! `now - last_used_at < 30 days`; [`authenticate`](super::authenticate)
//! refuses and deletes one past it, [`list`] does not show one, and the user's
//! next [`create`] deletes every one, so an abandoned session does not ride in
//! every backup (ADR-0037 point 2). **`last_used_at` is written at most once an
//! hour**, because every write reaches the backup stream and an hour's
//! precision on a thirty-day rule costs nothing.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};

use super::secret::{self, Secret};
use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::db::new_id;
use crate::error::{Error, Result};
use crate::events::UserId;

/// How long a session lives without a request.
pub fn idle_expiry() -> Duration {
    Duration::days(30)
}

/// How stale `last_used_at` may be before a request writes it again.
pub fn touch_interval() -> Duration {
    Duration::hours(1)
}

/// Whether a session last used at `last_used_at` has expired at `now`.
pub fn is_idle_expired(last_used_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - last_used_at >= idle_expiry()
}

/// Whether a request at `now` writes `last_used_at` again. The same rule holds
/// for an API token's last use.
pub fn needs_touch(last_used_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - last_used_at >= touch_interval()
}

/// One live session, as the account page lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    /// Whether this is the session the request listing it came through.
    pub current: bool,
}

/// A new session for `user_id`: its row id and the secret the cookie carries,
/// which leaves the server once, in that cookie. `user_agent` is the
/// request's header, cut to 256 characters.
///
/// In the same transaction it deletes the user's sessions that are past the
/// idle rule.
pub async fn create(
    pool: &SqlitePool,
    clock: &dyn Clock,
    user_id: &str,
    user_agent: Option<&str>,
) -> Result<(String, Secret)> {
    let now = clock.now();
    let mut tx = pool.begin().await?;
    delete_idle(&mut tx, user_id, now).await?;

    let id = new_id();
    let secret = secret::mint_session_secret();
    let secret_hash = secret::hash(secret.expose());
    let user_agent = secret::truncate_user_agent(user_agent);
    sqlx::query!(
        "INSERT INTO sessions (id, user_id, secret_hash, user_agent, created_at, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        id,
        user_id,
        secret_hash,
        user_agent,
        now,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((id, secret))
}

/// Deletes `user_id`'s sessions that have expired at `now`. Compared in Rust,
/// not in SQL, so the rule has one spelling: [`is_idle_expired`].
async fn delete_idle(conn: &mut SqliteConnection, user_id: &str, now: DateTime<Utc>) -> Result<()> {
    let sessions = sqlx::query!(
        r#"SELECT id, last_used_at AS "last_used_at: DateTime<Utc>"
             FROM sessions WHERE user_id = ?1"#,
        user_id,
    )
    .fetch_all(&mut *conn)
    .await?;
    for session in sessions {
        if is_idle_expired(session.last_used_at, now) {
            sqlx::query!("DELETE FROM sessions WHERE id = ?1", session.id)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

/// The caller's live sessions, newest first. `current` is the session the
/// request came through, if it came through one.
pub async fn list(ctx: &ServiceContext, current: Option<&str>) -> Result<Vec<SessionSummary>> {
    let user_id: &UserId = &ctx.actor;
    let now = ctx.clock.now();
    let rows = sqlx::query!(
        r#"SELECT id, user_agent,
                  created_at AS "created_at: DateTime<Utc>",
                  last_used_at AS "last_used_at: DateTime<Utc>"
             FROM sessions
            WHERE user_id = ?1
            ORDER BY created_at DESC, id"#,
        user_id,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|row| !is_idle_expired(row.last_used_at, now))
        .map(|row| SessionSummary {
            current: current == Some(row.id.as_str()),
            id: row.id,
            user_agent: row.user_agent,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
        })
        .collect())
}

/// Deletes one of the caller's sessions. Another user's session id is answered
/// exactly as a never-issued one (ADR-0029 point 5).
pub async fn revoke(ctx: &ServiceContext, id: &str) -> Result<()> {
    let user_id: &UserId = &ctx.actor;
    let deleted = sqlx::query!(
        "DELETE FROM sessions WHERE id = ?1 AND user_id = ?2",
        id,
        user_id,
    )
    .execute(&ctx.pool)
    .await?;
    if deleted.rows_affected() == 0 {
        return Err(Error::not_found("There is no session with that id."));
    }
    Ok(())
}
