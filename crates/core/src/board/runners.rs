//! What the board knows about a runner that talks to it over the network
//! (task 052): when it was last heard from, and the provider it was paired
//! as.
//!
//! The runner protocol's handlers call these and decide nothing themselves
//! (ADR-0006): a handler that compared a provider or a timestamp would be a
//! second copy of a rule about runners.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::runner::provider::ProviderId;

use super::lease::{HEARTBEAT_INTERVAL, LEASE_LIFETIME};

/// Records that `runner_id` was just heard from: `runners.last_seen_at`, the
/// one writer of it.
///
/// One conditional `UPDATE`, which writes at most once per
/// [`HEARTBEAT_INTERVAL`]: a runner publishing a tail several times a second
/// must not become a stream of writes that Litestream replicates (ADR-0037
/// point 2). It publishes no change event: nothing on a board reads presence
/// live. Every authenticated runner request calls it; the in-process adapter
/// never does, because in solo nobody reads presence and a loop that claims
/// with a zero wait would write the row on every pass.
pub async fn mark_seen(ctx: &ServiceContext, runner_id: &str) -> Result<()> {
    let now = ctx.clock.now();
    let stale_before = before(now, HEARTBEAT_INTERVAL);
    sqlx::query!(
        "UPDATE runners SET last_seen_at = ?1
          WHERE id = ?2 AND (last_seen_at IS NULL OR last_seen_at < ?3)",
        now,
        runner_id,
        stale_before,
    )
    .execute(&ctx.pool)
    .await?;
    Ok(())
}

/// Whether `runner_id` has been heard from within one [`LEASE_LIFETIME`]: the
/// window in which a lease it holds would still be alive, and so the window
/// in which a Run now pressed for it can expect to be collected (ADR-0031
/// point 7). A runner never heard from is offline.
pub async fn is_online(ctx: &ServiceContext, runner_id: &str) -> Result<bool> {
    let last_seen_at = sqlx::query_scalar!(
        r#"SELECT last_seen_at AS "last_seen_at: DateTime<Utc>" FROM runners WHERE id = ?1"#,
        runner_id,
    )
    .fetch_optional(&ctx.pool)
    .await?
    .flatten();
    let now = ctx.clock.now();
    Ok(last_seen_at.is_some_and(|seen| seen >= before(now, LEASE_LIFETIME)))
}

/// The provider `runner_id` was paired as (task 047, seam-contract D28), held
/// against the one its request names, or `None` when the request names none.
///
/// A body's provider is a claim to verify, never an input, as a lease's team
/// is (D31 point 3): a runner that reported another provider would be offered
/// tasks under another catalogue and ADR-0031 point 1's model rule. A runner
/// whose provider changed is paired again.
pub async fn paired_provider(
    ctx: &ServiceContext,
    runner_id: &str,
    claimed: Option<ProviderId>,
) -> Result<ProviderId> {
    let stored = sqlx::query_scalar!("SELECT provider FROM runners WHERE id = ?1", runner_id)
        .fetch_optional(&ctx.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no runner with id {runner_id}")))?;
    let paired = ProviderId::parse(&stored)?;
    match claimed {
        Some(claimed) if claimed != paired => Err(Error::invalid(format!(
            "this runner was paired as {paired}, and its request names {claimed}; a runner \
             whose provider changed is paired again"
        ))),
        _ => Ok(paired),
    }
}

fn before(now: DateTime<Utc>, by: Duration) -> DateTime<Utc> {
    chrono::Duration::from_std(by)
        .ok()
        .and_then(|by| now.checked_sub_signed(by))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}
