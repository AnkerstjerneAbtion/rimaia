//! The global pause a usage limit puts on new starts (ADR-0011).
//!
//! > A usage-limit hit pauses new starts globally for the duration of the wait,
//! > in both modes. Starting a fresh task into a limited window just burns a
//! > start.
//!
//! ADR-0011 says that and does not say where it lives. It lives here, as one
//! `settings` key in the shape seam-contract D3 fixes and [`state`](super::state)
//! and [`capacity`](super::capacity) already use: storage through task 006's
//! accessor, the rules about the key in the module that has the rules.
//!
//! # Stored, not held in memory
//!
//! A field on the queue would be lost by a relaunch, and a relaunch at 03:00 is
//! exactly when this matters: the window is still closed, and the first thing
//! the queue would do is spend a start proving it. The row survives, so the
//! second launch honours a wait the first one learned about.
//!
//! # It does not stop what is already running
//!
//! [`active_until`] is read by `try_step` **before** the plan, so both modes
//! honour it by construction and neither has a branch for it. Nothing here
//! cancels anything: a run mid-edit when *another* task hits a limit has done
//! nothing wrong, and killing it would throw away work to enforce a rule about
//! *starting*.
//!
//! # A second limit may only ever lengthen the wait
//!
//! [`note_usage_limit`] keeps the later of the two instants. Two tasks hitting
//! the same wall report resets a few seconds apart, and taking the newest
//! blindly would let the second one shorten a pause the first one set — which
//! is the failure this key exists to prevent, arrived at from the inside.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::db::settings;
use crate::error::Result;

/// The `settings` key holding the instant new starts are held until.
pub const USAGE_LIMIT_PAUSE_UNTIL: &str = "usage_limit_pause_until";

/// When new starts are held until, or `None` when they are not.
///
/// `now` is a parameter rather than a clock read, so this stays a pure question
/// about the stored row and the caller's instant — the queue already has one
/// from [`ServiceContext::clock`](crate::ServiceContext), and a second read
/// inside here could disagree with the one the same pass used for selection.
///
/// A stored value that is absent, unparseable or already past all read as "not
/// paused", which is the tolerant direction seam-contract D3's siblings take
/// and for the same ADR-0003 reason: the user is a supported writer of this
/// file, and a queue that refused to run all night over a typo in the `sqlite3`
/// CLI is the worse outcome.
pub async fn active_until(pool: &SqlitePool, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    let Some(stored) = settings::get(pool, USAGE_LIMIT_PAUSE_UNTIL).await? else {
        return Ok(None);
    };

    let Ok(until) = stored.trim().parse::<DateTime<Utc>>() else {
        tracing::warn!(
            value = stored,
            "unreadable usage_limit_pause_until; treating the queue as unpaused"
        );
        return Ok(None);
    };

    Ok((until > now).then_some(until))
}

/// Records that a run hit a usage limit that clears at `until`.
///
/// Keeps the **later** of the stored instant and this one — see this module's
/// header. Writes nothing when the stored value is already later, so a second
/// limit inside one window costs no `settings:changed` event and cannot move a
/// deadline the Runs view is already showing.
pub async fn note_usage_limit(ctx: &ServiceContext, until: DateTime<Utc>) -> Result<()> {
    let existing = match settings::get(&ctx.pool, USAGE_LIMIT_PAUSE_UNTIL).await? {
        Some(stored) => stored.trim().parse::<DateTime<Utc>>().ok(),
        None => None,
    };

    if existing.is_some_and(|stored| stored >= until) {
        return Ok(());
    }

    tracing::warn!(
        until = %until.to_rfc3339(),
        "a run hit a usage limit; holding new starts until the window reopens",
    );
    settings::set(ctx, USAGE_LIMIT_PAUSE_UNTIL, &until.to_rfc3339()).await
}

/// Lifts the hold.
///
/// Not called by the queue, which simply reads a pause that has expired as no
/// pause at all — [`active_until`] compares against `now` for exactly that
/// reason, so nothing has to remember to clean up at 06:00. This exists for the
/// operator who wants the row gone, and for tests.
pub async fn clear(ctx: &ServiceContext) -> Result<()> {
    settings::set(ctx, USAGE_LIMIT_PAUSE_UNTIL, "").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestContext};
    use pretty_assertions::assert_eq;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }

    #[tokio::test]
    async fn a_queue_that_has_never_hit_a_wall_is_not_paused() {
        let pool = test_pool().await;

        assert_eq!(
            active_until(&pool, at("2026-08-20T02:00:00Z"))
                .await
                .expect("read the key"),
            None
        );
    }

    #[tokio::test]
    async fn a_pause_survives_being_read_back_through_the_row_it_wrote() {
        // The whole reason it is a row: a relaunch at 03:00 must not burn a
        // start into a window that is still closed.
        let harness = TestContext::new().await;
        let until = at("2026-08-20T06:00:00Z");

        note_usage_limit(&harness.context, until)
            .await
            .expect("record the limit");

        assert_eq!(
            active_until(&harness.context.pool, at("2026-08-20T02:00:00Z"))
                .await
                .expect("read it back"),
            Some(until),
        );
    }

    #[tokio::test]
    async fn a_second_limit_may_lengthen_a_pending_wait_but_never_shorten_it() {
        let harness = TestContext::new().await;
        let now = at("2026-08-20T02:00:00Z");

        note_usage_limit(&harness.context, at("2026-08-20T06:00:00Z"))
            .await
            .expect("the first wall");
        note_usage_limit(&harness.context, at("2026-08-20T05:00:00Z"))
            .await
            .expect("a second wall reporting an earlier reset");

        assert_eq!(
            active_until(&harness.context.pool, now)
                .await
                .expect("read"),
            Some(at("2026-08-20T06:00:00Z")),
        );

        note_usage_limit(&harness.context, at("2026-08-20T07:00:00Z"))
            .await
            .expect("a later wall");
        assert_eq!(
            active_until(&harness.context.pool, now)
                .await
                .expect("read"),
            Some(at("2026-08-20T07:00:00Z")),
        );
    }

    #[tokio::test]
    async fn a_pause_whose_instant_has_passed_is_no_pause_at_all() {
        // Nothing has to remember to clear it at 06:00, which is what keeps a
        // crashed launch from leaving the queue paused forever.
        let harness = TestContext::new().await;
        note_usage_limit(&harness.context, at("2026-08-20T06:00:00Z"))
            .await
            .expect("record the limit");

        assert_eq!(
            active_until(&harness.context.pool, at("2026-08-20T06:00:01Z"))
                .await
                .expect("read"),
            None
        );
    }

    #[tokio::test]
    async fn a_hand_edited_pause_is_read_as_no_pause_rather_than_failing_the_night() {
        let harness = TestContext::new().await;

        for nonsense in ["soon", "", "1787224800"] {
            settings::set(&harness.context, USAGE_LIMIT_PAUSE_UNTIL, nonsense)
                .await
                .expect("store nonsense");
            assert_eq!(
                active_until(&harness.context.pool, at("2026-08-20T02:00:00Z"))
                    .await
                    .expect("a bad row is not an error"),
                None,
                "{nonsense:?}",
            );
        }
    }

    #[tokio::test]
    async fn clearing_lifts_the_hold() {
        let harness = TestContext::new().await;
        note_usage_limit(&harness.context, at("2026-08-20T06:00:00Z"))
            .await
            .expect("record the limit");

        clear(&harness.context).await.expect("lift the hold");

        assert_eq!(
            active_until(&harness.context.pool, at("2026-08-20T02:00:00Z"))
                .await
                .expect("read"),
            None
        );
    }
}
