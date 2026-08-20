use rimaia_core::{AppPaths, ServiceContext};

/// Everything a command needs, built once at startup and managed by Tauri.
///
/// `context` is the one `ServiceContext` for the whole process (ADR-0018): the
/// `SqlitePool` (ADR-0003), the clock, and the change-event sender travel
/// together, so every command calls a `rimaia-core` service the same way the
/// MCP server (task 010) will — `&state.context`, never a bare pool pulled back
/// out of it. `paths` stays a separate field because it is the shell's own
/// concern (`AppPaths::worktrees_dir` and friends resolve a platform directory
/// core cannot look up itself), not something a service needs ambiently.
pub struct AppState {
    pub context: ServiceContext,
    pub paths: AppPaths,
}
