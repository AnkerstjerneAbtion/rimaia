//! The port the MCP server listens on (ADR-0006, seam-contract D16).
//!
//! The key constant and its rules live here rather than in
//! [`crate::db::settings`], for the reason `scheduler::state`'s `QUEUE_STATE`
//! gives at its own: seam-contract D3 puts the rules about a key with the code
//! that has the rules, and nothing outside this module has any business
//! knowing what `mcp_port` means. Storage is still task 006's accessor, so
//! there is one `settings` reader and not two.
//!
//! Unseeded, like `run_environment`: an absent key *is* [`DEFAULT_PORT`], and
//! a row only appears once the user has changed it. That is also what makes
//! ADR-0006's "registration is one time" true — the URL a user pasted into
//! `claude mcp add` keeps working across launches because nothing rewrites the
//! port behind them.

use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::db::settings;
use crate::error::{Error, Result};
use crate::mcp::DEFAULT_PORT;

/// The `settings` key holding the listening port.
pub const MCP_PORT: &str = "mcp_port";

/// The lowest port this app may ask for.
///
/// Below 1024 needs privileges a desktop app does not have and should not
/// acquire, and `0` would hand out an OS-chosen ephemeral port — which works,
/// and then changes on the next launch, silently invalidating the URL the user
/// registered with `claude mcp add`.
const LOWEST_USABLE_PORT: u16 = 1024;

/// The configured port, or [`DEFAULT_PORT`] when the key is absent or holds
/// something unusable.
///
/// Tolerant rather than fallible, exactly as
/// [`RunEnvironment`](crate::db::RunEnvironment) and `queue_state` are, and for
/// the same reason: `settings.value` has no `CHECK` and the user is a supported
/// writer of this file (ADR-0003). A typo hand-edited into the row costs a log
/// line and the default, never a launch.
pub async fn configured_port(pool: &SqlitePool) -> Result<u16> {
    let Some(stored) = settings::get(pool, MCP_PORT).await? else {
        return Ok(DEFAULT_PORT);
    };

    match stored.trim().parse::<u16>() {
        Ok(port) if port >= LOWEST_USABLE_PORT => Ok(port),
        _ => {
            tracing::warn!(
                value = stored,
                default = DEFAULT_PORT,
                "unusable mcp_port; falling back to the default"
            );
            Ok(DEFAULT_PORT)
        }
    }
}

/// Stores the port the server should listen on, announcing it as a settings
/// change (ADR-0018).
///
/// Refuses anything below [`LOWEST_USABLE_PORT`] with its own sentence rather
/// than a constraint violation, because the Settings panel renders this
/// message verbatim. `Error::invalid` and no new `ErrorCode` (seam-contract
/// D8): the specificity that matters is in the words.
///
/// This writes the setting and nothing else — restarting the listener on the
/// new port is the shell's `set_mcp_port` command, which owns the handle.
pub async fn set_configured_port(ctx: &ServiceContext, port: u16) -> Result<()> {
    if port < LOWEST_USABLE_PORT {
        return Err(Error::invalid(format!(
            "port {port} is not usable: ports below {LOWEST_USABLE_PORT} need privileges Rimaia \
             does not have. Pick a port between {LOWEST_USABLE_PORT} and 65535."
        )));
    }

    settings::set(ctx, MCP_PORT, &port.to_string()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestContext};
    use crate::ChangeEvent;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn an_unconfigured_port_is_4517() {
        let pool = test_pool().await;

        assert_eq!(
            settings::get(&pool, MCP_PORT).await.expect("read the key"),
            None,
            "the key is deliberately unseeded"
        );
        assert_eq!(
            configured_port(&pool).await.expect("read the default"),
            DEFAULT_PORT
        );
    }

    #[tokio::test]
    async fn a_stored_port_round_trips() {
        let harness = TestContext::new().await;

        set_configured_port(&harness.context, 4599)
            .await
            .expect("store a port");

        assert_eq!(
            settings::get(&harness.context.pool, MCP_PORT)
                .await
                .expect("read the row"),
            Some("4599".to_string()),
            "stored as digits, so the row is legible in the sqlite3 CLI"
        );
        assert_eq!(
            configured_port(&harness.context.pool)
                .await
                .expect("read it back"),
            4599
        );
    }

    #[tokio::test]
    async fn a_hand_edited_port_falls_back_to_the_default_rather_than_failing() {
        let harness = TestContext::new().await;

        for typo in ["four thousand", "", "70000", "-1", "80"] {
            settings::set(&harness.context, MCP_PORT, typo)
                .await
                .expect("store a typo");

            assert_eq!(
                configured_port(&harness.context.pool)
                    .await
                    .expect("read it back"),
                DEFAULT_PORT,
                "a hand-edited {typo:?} must cost a log line, not a launch"
            );
        }
    }

    #[tokio::test]
    async fn a_port_below_1024_is_refused() {
        let harness = TestContext::new().await;

        let error = set_configured_port(&harness.context, 80)
            .await
            .expect_err("a privileged port is refused");

        assert_eq!(
            error.to_string(),
            "port 80 is not usable: ports below 1024 need privileges Rimaia does not have. \
             Pick a port between 1024 and 65535."
        );
        assert_eq!(
            settings::get(&harness.context.pool, MCP_PORT)
                .await
                .expect("read the key"),
            None,
            "a refused write stores nothing"
        );
    }

    #[tokio::test]
    async fn writing_the_port_publishes_settings() {
        // The panel re-reads its status on `settings:changed`, which is the
        // only way a second window learns the port moved (ADR-0018).
        let mut harness = TestContext::new().await;

        set_configured_port(&harness.context, 4600)
            .await
            .expect("store a port");

        assert_eq!(
            harness.changes.try_recv().expect("a publication"),
            ChangeEvent::Settings
        );
    }
}
