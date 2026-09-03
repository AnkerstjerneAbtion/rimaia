//! Repository registration commands (task 003, ADR-0002, ADR-0005, ADR-0012).
//!
//! Every validation, every git call, and the unattended-runs opt-in itself all
//! live in `rimaia_core::repo` — this file only reshapes the wire args into
//! that module's types and calls it, per this crate's own module doc.

use rimaia_core::db::Repository;
use rimaia_core::repo::{self, NewRepository, RemoteInfo, RepositoryPatch};
use rimaia_core::Result;
use serde::Deserialize;
use tauri::State;

use crate::state::AppState;

/// What the frontend sends [`register_repository`]. Mirrors [`NewRepository`],
/// as a shape `serde` can pull off the wire — `NewRepository` itself derives
/// no `Deserialize` because it is a service input, not a row (see
/// `db::models`'s own doc comment on that distinction).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRepositoryInput {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub worktree_root: Option<String>,
}

/// What the frontend sends [`update_repository`]. Mirrors [`RepositoryPatch`]
/// field for field — every field left `None` leaves that column unchanged.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRepositoryInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub worktree_root: Option<String>,
}

/// Every registered repository, alphabetically — the order Settings shows
/// them in.
#[tauri::command]
pub async fn list_repositories(state: State<'_, AppState>) -> Result<Vec<Repository>> {
    repo::list(&state.context).await
}

/// Validates and registers a local repository (task 003's four checks; each
/// produces its own message).
#[tauri::command]
pub async fn register_repository(
    state: State<'_, AppState>,
    input: RegisterRepositoryInput,
) -> Result<Repository> {
    let worktrees_dir = state.paths.worktrees_dir();
    repo::register(
        &state.context,
        &worktrees_dir,
        NewRepository {
            path: input.path,
            name: input.name,
            worktree_root: input.worktree_root,
        },
    )
    .await
}

/// Edits an already-registered repository. Does not re-run git validation —
/// see [`repo::update`]'s own doc for why.
#[tauri::command]
pub async fn update_repository(
    state: State<'_, AppState>,
    id: String,
    patch: UpdateRepositoryInput,
) -> Result<Repository> {
    repo::update(
        &state.context,
        &id,
        RepositoryPatch {
            name: patch.name,
            default_branch: patch.default_branch,
            worktree_root: patch.worktree_root,
            // Not on the edit form: raising a repository's cap is the opt-out
            // ADR-0010 wants taken deliberately, so it has its own command and
            // its own explanation next to the control.
            max_concurrency: None,
        },
    )
    .await
}

/// Raises or lowers ADR-0010's per-repository cap on how many runs this
/// repository holds at once.
///
/// Its own command rather than a field on [`update_repository`] for the same
/// reason the unattended-runs opt-in is: it is a deliberate act with a
/// consequence the panel has to state — two agents in one repository fight over
/// ports, test databases and lockfiles — and burying it in an "edit name and
/// branch" form would make it look like a preference.
#[tauri::command]
pub async fn set_repository_max_concurrency(
    state: State<'_, AppState>,
    id: String,
    max_concurrency: i64,
) -> Result<Repository> {
    repo::set_max_concurrency(&state.context, &id, max_concurrency).await
}

/// Flips ADR-0012's per-repository opt-in to unattended runs. The
/// confirmation dialog stating what enabling this permits is the frontend's
/// job (task 003's scope); this is the explicit act itself, called only once
/// the user has agreed.
#[tauri::command]
pub async fn set_repository_unattended_runs(
    state: State<'_, AppState>,
    id: String,
    allow: bool,
) -> Result<Repository> {
    repo::set_allow_unattended_runs(&state.context, &id, allow).await
}

/// Removes a repository. Refused, naming how many, when any task still
/// references it.
#[tauri::command]
pub async fn remove_repository(state: State<'_, AppState>, id: String) -> Result<()> {
    repo::remove(&state.context, &id).await
}

/// Fresh inspection of a repository's remote and `gh` readiness — never
/// cached, per [`repo::remote_info`]'s own doc comment.
#[tauri::command]
pub async fn get_repository_remote_info(
    state: State<'_, AppState>,
    id: String,
) -> Result<RemoteInfo> {
    let repository = repo::get(&state.context, &id).await?;
    repo::remote_info(&repository).await
}
