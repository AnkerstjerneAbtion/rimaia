//! Whether the queue is working, stored where a crash cannot erase it.
//!
//! ADR-0010: "Queue state survives an app restart by being derived from the
//! database." So this is a `settings` row and not a field on
//! [`QueueHandle`](super::QueueHandle) — a queue the user started at 18:30 is
//! still started at 03:00 after the app was force-quit at midnight, and the
//! loop reads this on its next pass rather than remembering it.
//!
//! The key constant lives here rather than in [`crate::db::settings`] for the
//! reason `runner::process::DISALLOWED_TOOLS` gives for living where it does:
//! seam-contract D3 puts the *rules* about a key with the task that has the
//! rules, and nothing outside the scheduler has any business knowing what
//! `queue_state` means. The storage is still task 006's accessor, so there is
//! one `settings` reader and not two.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::db::settings;
use crate::error::Result;

/// The `settings` key holding [`QueueState`].
pub const QUEUE_STATE: &str = "queue_state";

/// Whether the queue starts new runs.
///
/// **Not [`ScheduleMode`](crate::db::ScheduleMode)**, which is ADR-0010's
/// sequential-or-parallel run configuration, and not
/// [`RunState`](crate::db::RunState), which is one *task's* place in the
/// machine's process. This is the whole queue's on/off switch and has exactly
/// two values, because ADR-0010's control verbs collapse onto them: Start and
/// Resume both write [`Running`](QueueState::Running), Pause writes
/// [`Paused`](QueueState::Paused), and Stop writes `Paused` and additionally
/// cancels whatever is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    /// Working the `ready` column top-down, one run at a time.
    Running,
    /// Starting nothing new. An in-flight run is left to finish — that is the
    /// difference between Pause and Stop.
    ///
    /// The default, and what an absent key means: a first launch, or a database
    /// somebody hand-edited, must not start spending tokens unbidden.
    #[default]
    Paused,
}

impl QueueState {
    /// The stored spelling, which is also the wire spelling — one string, so
    /// the row stays legible in the `sqlite3` CLI (ADR-0003).
    pub const fn as_str(self) -> &'static str {
        match self {
            QueueState::Running => "running",
            QueueState::Paused => "paused",
        }
    }

    /// Reads a stored value, falling back to the safe default for anything
    /// else.
    ///
    /// Tolerant rather than fallible, exactly as
    /// [`RunEnvironment::from_stored`](crate::db::settings::RunEnvironment)
    /// is and for the same reason: `settings.value` has no `CHECK` and the user
    /// is a supported writer of this file (ADR-0003). Falling back to `paused`
    /// rather than to `running` is the direction that costs a queue that does
    /// not start rather than one that starts unasked.
    fn from_stored(value: &str) -> Self {
        match value {
            "running" => QueueState::Running,
            "paused" => QueueState::Paused,
            other => {
                tracing::warn!(
                    value = other,
                    "unrecognised queue_state; falling back to paused"
                );
                QueueState::default()
            }
        }
    }
}

/// Whether the queue is working. An absent key is [`QueueState::Paused`].
pub async fn queue_state(pool: &SqlitePool) -> Result<QueueState> {
    Ok(settings::get(pool, QUEUE_STATE)
        .await?
        .as_deref()
        .map(QueueState::from_stored)
        .unwrap_or_default())
}

/// Writes the switch and announces it (ADR-0018: `settings:changed` is what
/// tells the Runs view to re-read the queue's status).
pub async fn set_queue_state(ctx: &ServiceContext, state: QueueState) -> Result<()> {
    settings::set(ctx, QUEUE_STATE, state.as_str()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestContext};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn a_queue_nobody_has_ever_started_is_paused() {
        let pool = test_pool().await;

        assert_eq!(
            settings::get(&pool, QUEUE_STATE)
                .await
                .expect("read the key"),
            None
        );
        assert_eq!(
            queue_state(&pool).await.expect("read the default"),
            QueueState::Paused
        );
    }

    #[tokio::test]
    async fn the_switch_survives_being_read_back_through_the_row_it_wrote() {
        // The whole point of storing it: "queue state derived from the
        // database, so it survives app restart" (ADR-0010). Re-reading through
        // the pool is the closest a unit test gets to a second launch.
        let harness = TestContext::new().await;

        set_queue_state(&harness.context, QueueState::Running)
            .await
            .expect("start the queue");

        assert_eq!(
            settings::get(&harness.context.pool, QUEUE_STATE)
                .await
                .expect("read the row"),
            Some("running".to_string()),
        );
        assert_eq!(
            queue_state(&harness.context.pool)
                .await
                .expect("read it back"),
            QueueState::Running
        );
    }

    #[tokio::test]
    async fn a_hand_edited_switch_falls_back_to_paused_rather_than_to_running() {
        // The direction of the fallback is the decision. A typo in the sqlite3
        // CLI must not be able to start an unattended queue.
        let harness = TestContext::new().await;

        settings::set(&harness.context, QUEUE_STATE, "Running")
            .await
            .expect("store a typo");

        assert_eq!(
            queue_state(&harness.context.pool)
                .await
                .expect("read it back"),
            QueueState::Paused
        );
    }

    #[test]
    fn the_switch_serializes_with_the_spelling_it_stores() {
        for state in [QueueState::Running, QueueState::Paused] {
            assert_eq!(
                serde_json::to_value(state).expect("an enum must serialize"),
                serde_json::Value::String(state.as_str().to_string())
            );
            assert_eq!(QueueState::from_stored(state.as_str()), state);
        }
    }
}
