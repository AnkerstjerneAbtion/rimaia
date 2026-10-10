//! Raw settings reads and writes for a test that arranges configuration
//! (task 039).
//!
//! `db::settings::get` and `set` are gone: production code reaches a key only
//! through the accessor its placement names. A test still needs to plant a
//! hand-edited value, or read back exactly what a writer stored, so these route
//! a key to the store [`placement`] names — the context's one team for a team
//! key, its actor for a user key, the legacy table for a runner key — through
//! the same accessors production uses. Never compiled into the app.

use crate::context::ServiceContext;
use crate::db::settings::{self, placement, Placement};
use crate::error::Result;

/// Writes `key` to the store its placement names, for the context's one team
/// or its actor, and announces it as the accessor does.
pub async fn set(ctx: &ServiceContext, key: &str, value: &str) -> Result<()> {
    match placement(key) {
        Placement::Team => settings::set_team(ctx, ctx.scope.sole()?, key, Some(value)).await,
        Placement::User => settings::set_user(ctx, key, value).await,
        Placement::Runner => settings::set_runner(ctx, key, value).await,
    }
}

/// Writes a team key for `team_id`, which must be in the context's scope.
pub async fn set_team(ctx: &ServiceContext, team_id: &str, key: &str, value: &str) -> Result<()> {
    settings::set_team(ctx, team_id, key, Some(value)).await
}

/// Reads `key` from the store its placement names, for the context's one team
/// or its actor.
pub async fn get(ctx: &ServiceContext, key: &str) -> Result<Option<String>> {
    match placement(key) {
        Placement::Team => settings::get_team(ctx, ctx.scope.sole()?, key).await,
        Placement::User => settings::get_user(ctx, key).await,
        Placement::Runner => settings::get_runner(ctx, key).await,
    }
}

/// Reads a team key for `team_id`, which must be in the context's scope.
pub async fn get_team(ctx: &ServiceContext, team_id: &str, key: &str) -> Result<Option<String>> {
    settings::get_team(ctx, team_id, key).await
}
