//! Pairing a runner (ADR-0030 point 5).
//!
//! A runner gets its `rmr_` token one of two ways. A connected desktop pairs
//! its own runner with [`pair_own_runner`]. A headless machine redeems a
//! pairing code: the person creates one on the web ([`create_code`]), it lives
//! ten minutes, and `rimaia-runner pair` exchanges it with [`redeem`]. A code
//! is useless once used, whether or not the redemption that used it went on to
//! succeed.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::{SqliteConnection, SqlitePool};

use super::secret::{self, Secret};
use super::tokens;
use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::db::new_id;
use crate::error::{Error, Result};
use crate::events::{RunnerId, UserId};
use crate::runner::provider::ProviderId;

/// How long a pairing code can be redeemed.
pub fn code_lifetime() -> Duration {
    Duration::minutes(10)
}

/// What an unknown, a used and an expired code are all told, so the answer is
/// not an oracle for which codes exist.
pub const INVALID_CODE: &str =
    "This pairing code is not valid. Codes work once, for ten minutes: create a new one.";

/// A pairing code, the one time it is shown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    /// `XXXX-XXXX`, over Crockford's base32 alphabet.
    pub code: Secret,
    pub expires_at: DateTime<Utc>,
}

/// A runner that has just been paired, and its token, the one time it is
/// shown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedRunner {
    pub runner_id: RunnerId,
    pub token: Secret,
}

/// A new pairing code for the caller. Only its hash is stored. The same
/// transaction deletes the caller's expired codes, so an unredeemed code does
/// not outlive its ten minutes in the table or in any backup.
pub async fn create_code(ctx: &ServiceContext) -> Result<PairingCode> {
    let user_id: &UserId = &ctx.actor;
    let now = ctx.clock.now();
    let code = secret::mint_pairing_code();
    let code_hash = code_hash(code.expose());
    let expires_at = now + code_lifetime();

    let mut tx = ctx.pool.begin().await?;
    let codes = sqlx::query!(
        r#"SELECT code_hash, expires_at AS "expires_at: DateTime<Utc>"
             FROM pairing_codes WHERE user_id = ?1"#,
        user_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    for stale in codes.into_iter().filter(|code| code.expires_at <= now) {
        sqlx::query!(
            "DELETE FROM pairing_codes WHERE code_hash = ?1",
            stale.code_hash
        )
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query!(
        "INSERT INTO pairing_codes (code_hash, user_id, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4)",
        code_hash,
        user_id,
        now,
        expires_at,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(PairingCode { code, expires_at })
}

/// Exchanges a pairing code for a new runner of the code's owner, labelled
/// `label`, running `provider`.
///
/// Three steps, each with a reason:
///
/// 1. The label and the provider are validated before any row is touched, so
///    a typo costs the person nothing: the code stays redeemable.
/// 2. The code row is deleted by its hash and **that delete commits on its
///    own**. Nothing came back, or the code had expired: [`INVALID_CODE`].
/// 3. Only then is the runner written, with its token, in a second
///    transaction. A failure here costs a new code, never a reusable one.
///
/// The code's hash is the table's primary key, so the lookup is the database
/// matching a hash of a secret, and there is no in-memory comparison left to
/// make in constant time.
pub async fn redeem(
    pool: &SqlitePool,
    clock: &dyn Clock,
    code: &str,
    label: &str,
    provider: &str,
) -> Result<PairedRunner> {
    let label = secret::label(label)?;
    let provider = ProviderId::parse(provider)?;

    let code_hash = code_hash(code);
    let spent = sqlx::query!(
        r#"DELETE FROM pairing_codes WHERE code_hash = ?1
           RETURNING user_id, expires_at AS "expires_at: DateTime<Utc>""#,
        code_hash,
    )
    .fetch_optional(pool)
    .await?;
    let user_id = match spent {
        Some(spent) if clock.now() < spent.expires_at => spent.user_id,
        _ => return Err(Error::invalid(INVALID_CODE)),
    };

    let mut tx = pool.begin().await?;
    let paired = insert_runner(&mut tx, clock, &user_id, &label, provider).await?;
    tx.commit().await?;
    Ok(paired)
}

/// A connected desktop's own runner, paired with no code (ADR-0030 point 5).
/// Only a desktop caller may ask, which the account command checks.
pub async fn pair_own_runner(
    ctx: &ServiceContext,
    label: &str,
    provider: &str,
) -> Result<PairedRunner> {
    let label = secret::label(label)?;
    let provider = ProviderId::parse(provider)?;
    let mut tx = ctx.pool.begin().await?;
    let paired = insert_runner(&mut tx, ctx.clock.as_ref(), &ctx.actor, &label, provider).await?;
    tx.commit().await?;
    Ok(paired)
}

/// The stored form of a typed or minted code: the hash of its normalized
/// spelling, so `abcd-efgh` finds `ABCD-EFGH`.
fn code_hash(code: &str) -> String {
    secret::hash(&secret::normalize_pairing_code(code))
}

/// A `runners` row and its token, on `conn`.
async fn insert_runner(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    user_id: &str,
    label: &str,
    provider: ProviderId,
) -> Result<PairedRunner> {
    let runner_id = new_id();
    let provider = provider.as_str();
    let paired_at = clock.now();
    sqlx::query!(
        "INSERT INTO runners (id, user_id, label, provider, paired_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        runner_id,
        user_id,
        label,
        provider,
        paired_at,
    )
    .execute(&mut *conn)
    .await?;
    let token = tokens::create_runner(conn, clock, user_id, &runner_id, label).await?;
    Ok(PairedRunner {
        runner_id,
        token: token.token,
    })
}
