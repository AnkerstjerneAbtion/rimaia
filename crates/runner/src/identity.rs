//! Which runner this machine is, and which board it reports to (D28 part 6).

use rimaia_core::Result;

use crate::RunnerStore;

/// The `runner_identity` row: the runner this store is, and the server it
/// reports to. The runner's token is in the keychain, never here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerIdentity {
    pub runner_id: String,
    /// `None` in solo, where the board is this machine's own file.
    pub server_url: Option<String>,
}

/// The identity this store holds, or `None` before adoption has written one.
pub async fn current(store: &RunnerStore) -> Result<Option<RunnerIdentity>> {
    let row = sqlx::query!("SELECT runner_id, server_url FROM runner_identity")
        .fetch_optional(store.pool())
        .await?;
    Ok(row.map(|row| RunnerIdentity {
        runner_id: row.runner_id,
        server_url: row.server_url,
    }))
}
