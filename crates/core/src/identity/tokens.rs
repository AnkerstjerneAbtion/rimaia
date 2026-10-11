//! API tokens: one table, three kinds (ADR-0030 points 3, 5 and 6).
//!
//! A desktop app holds an `rmd_` token, a runner an `rmr_` token and an MCP
//! client an `rmp_` personal access token. All three are minted by
//! [`secret::mint_token`], stored as [`secret::hash`], listed on the account
//! page with where they were last used, and revoked by deleting the row. The
//! secret leaves the server exactly once, in the [`NewApiToken`] that answers
//! the call that minted it.
//!
//! A runner token belongs to a `runners` row. It is written in the same
//! transaction as that row, and revoking it is [`unpair_runner`], so "revoke
//! the runner token" and "unpair the runner" are one act and cannot disagree.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::SqliteConnection;
use sqlx::SqlitePool;

use super::secret::{self, Secret, TokenKind};
use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::db::new_id;
use crate::error::{Error, Result};
use crate::events::UserId;

/// A token as the account page lists it. Never a hash.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiTokenSummary {
    pub id: String,
    pub kind: TokenKind,
    pub label: String,
    /// The runner a runner token belongs to.
    pub runner_id: Option<String>,
    /// The teams a personal access token is restricted to. Empty: every team
    /// its user belongs to, now and later.
    pub team_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// The `User-Agent` of the request that last used it, cut to 256
    /// characters.
    pub last_used_from: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    /// Whether this is the token the request listing it came through.
    pub current: bool,
}

/// A token, the one time its secret is shown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApiToken {
    pub token: Secret,
    pub summary: ApiTokenSummary,
}

/// What a new token row holds besides its kind and owner.
struct NewRow<'a> {
    label: String,
    runner_id: Option<&'a str>,
    expires_at: Option<DateTime<Utc>>,
    team_ids: Vec<String>,
}

/// Writes one token row, and its team restriction, on `conn`.
async fn insert(
    conn: &mut SqliteConnection,
    now: DateTime<Utc>,
    user_id: &str,
    kind: TokenKind,
    row: NewRow<'_>,
) -> Result<NewApiToken> {
    let id = new_id();
    let token = secret::mint_token(kind);
    let secret_hash = secret::hash(token.expose());
    let kind_text = kind.as_str();
    sqlx::query!(
        "INSERT INTO api_tokens
            (id, user_id, kind, secret_hash, label, runner_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        id,
        user_id,
        kind_text,
        secret_hash,
        row.label,
        row.runner_id,
        now,
        row.expires_at,
    )
    .execute(&mut *conn)
    .await?;
    for team_id in &row.team_ids {
        sqlx::query!(
            "INSERT INTO api_token_teams (token_id, team_id) VALUES (?1, ?2)",
            id,
            team_id,
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(NewApiToken {
        token,
        summary: ApiTokenSummary {
            id,
            kind,
            label: row.label,
            runner_id: row.runner_id.map(str::to_string),
            team_ids: row.team_ids,
            created_at: now,
            last_used_at: None,
            last_used_from: None,
            expires_at: row.expires_at,
            current: false,
        },
    })
}

/// A personal access token for the caller (ADR-0030 point 6).
///
/// `label` is trimmed and must be 1 to 100 characters. Every id in `team_ids`
/// must be a team the caller belongs to, and one that is not is `not_found`,
/// never a sentence that confirms it exists (ADR-0029 point 5). An empty list
/// means every team the user belongs to, now and later. `expires_in_days`, if
/// present, is 1 to 365.
pub async fn create_personal(
    ctx: &ServiceContext,
    label: &str,
    team_ids: &[String],
    expires_in_days: Option<i64>,
) -> Result<NewApiToken> {
    let label = secret::label(label)?;
    let expires_at = match expires_in_days {
        None => None,
        Some(days) if (1..=365).contains(&days) => Some(ctx.clock.now() + Duration::days(days)),
        Some(_) => {
            return Err(Error::invalid(
                "A token expires in 1 to 365 days, or never.",
            ))
        }
    };
    let team_ids: Vec<String> = team_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if team_ids.iter().any(|team_id| !ctx.scope.contains(team_id)) {
        return Err(Error::not_found("There is no team with that id."));
    }

    let mut tx = ctx.pool.begin().await?;
    let created = insert(
        &mut tx,
        ctx.clock.now(),
        &ctx.actor,
        TokenKind::Personal,
        NewRow {
            label,
            runner_id: None,
            expires_at,
            team_ids,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(created)
}

/// A desktop token for `user_id`, at the end of the desktop's browser sign-in
/// (ADR-0030 point 4). It does not expire.
pub async fn create_desktop(
    pool: &SqlitePool,
    clock: &dyn Clock,
    user_id: &str,
    label: &str,
) -> Result<NewApiToken> {
    let label = secret::label(label)?;
    let mut tx = pool.begin().await?;
    let created = insert(
        &mut tx,
        clock.now(),
        user_id,
        TokenKind::Desktop,
        NewRow {
            label,
            runner_id: None,
            expires_at: None,
            team_ids: Vec::new(),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(created)
}

/// The token of `runner_id`, written inside the caller's transaction, which
/// is the one that writes the `runners` row (ADR-0030 point 5). It does not
/// expire.
pub async fn create_runner(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    user_id: &str,
    runner_id: &str,
    label: &str,
) -> Result<NewApiToken> {
    let label = secret::label(label)?;
    insert(
        conn,
        clock.now(),
        user_id,
        TokenKind::Runner,
        NewRow {
            label,
            runner_id: Some(runner_id),
            expires_at: None,
            team_ids: Vec::new(),
        },
    )
    .await
}

/// The caller's tokens, oldest first. `current` is the token the request came
/// through, if it came through one.
pub async fn list(ctx: &ServiceContext, current: Option<&str>) -> Result<Vec<ApiTokenSummary>> {
    let user_id: &UserId = &ctx.actor;
    let rows = sqlx::query!(
        r#"SELECT id, kind, label, runner_id,
                  created_at AS "created_at: DateTime<Utc>",
                  last_used_at AS "last_used_at: DateTime<Utc>",
                  last_used_from,
                  expires_at AS "expires_at: DateTime<Utc>"
             FROM api_tokens
            WHERE user_id = ?1
            ORDER BY created_at, id"#,
        user_id,
    )
    .fetch_all(&ctx.pool)
    .await?;
    let restrictions = sqlx::query!(
        "SELECT r.token_id, r.team_id
           FROM api_token_teams r JOIN api_tokens t ON t.id = r.token_id
          WHERE t.user_id = ?1
          ORDER BY r.team_id",
        user_id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let kind = TokenKind::parse(&row.kind)
                .ok_or_else(|| Error::internal(format!("unknown token kind `{}`", row.kind)))?;
            Ok(ApiTokenSummary {
                current: current == Some(row.id.as_str()),
                team_ids: restrictions
                    .iter()
                    .filter(|restriction| restriction.token_id == row.id)
                    .map(|restriction| restriction.team_id.clone())
                    .collect(),
                id: row.id,
                kind,
                label: row.label,
                runner_id: row.runner_id,
                created_at: row.created_at,
                last_used_at: row.last_used_at,
                last_used_from: row.last_used_from,
                expires_at: row.expires_at,
            })
        })
        .collect()
}

/// Revokes one of the caller's tokens. A runner token unpairs its runner.
/// Another user's token id is answered exactly as a never-issued one
/// (ADR-0029 point 5).
pub async fn revoke(ctx: &ServiceContext, id: &str) -> Result<()> {
    let user_id: &UserId = &ctx.actor;
    let mut tx = ctx.pool.begin().await?;
    let token = sqlx::query!(
        "SELECT runner_id FROM api_tokens WHERE id = ?1 AND user_id = ?2",
        id,
        user_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| Error::not_found("There is no token with that id."))?;

    match token.runner_id {
        Some(runner_id) => unpair_runner(&mut tx, ctx.clock.as_ref(), &runner_id).await?,
        None => {
            sqlx::query!("DELETE FROM api_tokens WHERE id = ?1", id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Unpairs `runner_id`: deletes its tokens and sets `runners.unpaired_at`,
/// inside the caller's transaction so both happen or neither does. The row
/// stays, because runs keep naming it (D28). Task 057 adds the release of the
/// runner's pins here.
pub async fn unpair_runner(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    runner_id: &str,
) -> Result<()> {
    let now = clock.now();
    sqlx::query!("DELETE FROM api_tokens WHERE runner_id = ?1", runner_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!(
        "UPDATE runners SET unpaired_at = ?1 WHERE id = ?2 AND unpaired_at IS NULL",
        now,
        runner_id,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}
