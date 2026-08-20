use std::sync::Arc;

use rimaia_core::{AppPaths, Clock};
use sqlx::SqlitePool;

/// Everything a command needs, built once at startup and managed by Tauri.
///
/// One `SqlitePool` for the whole process (ADR-0003) — the UI, the scheduler and
/// the MCP server all share it. The clock is here rather than constructed at the
/// point of use so that nothing timestamps against the wall clock directly
/// (ADR-0015).
pub struct AppState {
    pub pool: SqlitePool,
    pub paths: AppPaths,
    pub clock: Arc<dyn Clock>,
}
