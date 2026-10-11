//! A person's own runners, as the account page and the runner settings list
//! them (task 050, ADR-0030 point 5).
//!
//! A runner works for exactly one user, so both functions read by the caller's
//! user and never by a team: another user's runner is not the caller's, even
//! one in a shared team, and is answered as a never-issued id.

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::tokens;
use crate::consent::eligibility::RunnerEligibility;
use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::events::{RunnerId, UserId};

/// What the board knows about one of the caller's runners. No setting: those
/// live in `runner.db` (D28 part 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerListing {
    pub id: RunnerId,
    pub label: String,
    pub provider: String,
    pub app_version: Option<String>,
    pub eligibility: RunnerEligibility,
    pub paired_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

fn no_such_runner() -> Error {
    Error::not_found("There is no runner with that id.")
}

/// The caller's paired runners, oldest first. An unpaired one is absent.
pub async fn list(ctx: &ServiceContext) -> Result<Vec<RunnerListing>> {
    let user_id: &UserId = &ctx.actor;
    let rows = sqlx::query!(
        r#"SELECT id, label, provider, app_version,
                  eligibility AS "eligibility: RunnerEligibility",
                  paired_at AS "paired_at: DateTime<Utc>",
                  last_seen_at AS "last_seen_at: DateTime<Utc>"
             FROM runners
            WHERE user_id = ?1 AND unpaired_at IS NULL
            ORDER BY paired_at, id"#,
        user_id,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| RunnerListing {
            id: row.id,
            label: row.label,
            provider: row.provider,
            app_version: row.app_version,
            eligibility: row.eligibility,
            paired_at: row.paired_at,
            last_seen_at: row.last_seen_at,
        })
        .collect())
}

/// Unpairs one of the caller's runners with [`tokens::unpair_runner`], the
/// function revoking its token reaches too, and writes nothing of its own.
/// Another user's runner, an unpaired one and an id nobody issued are all
/// `not_found`.
pub async fn unpair_own(ctx: &ServiceContext, runner_id: &str) -> Result<()> {
    let user_id: &UserId = &ctx.actor;
    let mut tx = ctx.pool.begin().await?;
    sqlx::query!(
        "SELECT id FROM runners WHERE id = ?1 AND user_id = ?2 AND unpaired_at IS NULL",
        runner_id,
        user_id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(no_such_runner)?;
    tokens::unpair_runner(&mut tx, ctx.clock.as_ref(), runner_id).await?;
    tx.commit().await?;
    Ok(())
}
