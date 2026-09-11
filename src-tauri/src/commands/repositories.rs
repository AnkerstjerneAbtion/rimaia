//! Repository registration commands (task 003, ADR-0002, ADR-0005, ADR-0012).
//!
//! Every validation, every git call, and the unattended-runs opt-in itself all
//! live in `rimaia_core::repo` — this file only reshapes the wire args into
//! that module's types and calls it, per this crate's own module doc.
//!
//! # Two commands here deliberately have no MCP tool (task 022, ADR-0020)
//!
//! [`set_repository_credential`] and [`remove_repository_credential`] join
//! `delete_task` and task 016's three cleanup commands as standing exceptions
//! to ADR-0021 point 1, and the ground is neither destructiveness nor a desktop
//! referent: **the argument is a live forge token**, and putting one on a
//! loopback protocol into a process's argv is a widening nothing asked for. The
//! read — [`get_repository_credential_status`] — does get a tool, because it
//! carries the login, the label and the date and never the secret.
//! Seam-contract D25 records it.

use rimaia_core::credentials::provision::{self, Verification};
use rimaia_core::credentials::Secret;
use rimaia_core::db::Repository;
use rimaia_core::repo::{self, NewRepository, RemoteInfo, RepositoryPatch};
use rimaia_core::{Error, Result};
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

// ---------------------------------------------------------------------------
// Per-repository forge credentials (task 022, ADR-0020)
// ---------------------------------------------------------------------------

/// What a repository's credential pane shows: whose token it is, what it is
/// called, when it was added, whether the keychain actually still holds it, and
/// whether `origin` is an SSH remote.
///
/// **Never the token.** After saving, the value is write-only — replace and
/// remove, never show — and there is no read path from this command to the
/// keychain's contents.
#[tauri::command]
pub async fn get_repository_credential_status(
    state: State<'_, AppState>,
    id: String,
) -> Result<repo::CredentialStatus> {
    let repository = repo::get(&state.context, &id).await?;
    credential_status(&state, &repository).await
}

/// Verifies a pasted token and stores it.
///
/// Three outcomes, three different answers, and the middle one is the reason
/// this is not a plain write: **the forge rejecting a token refuses the save**
/// (ADR-0020's "refused at paste time"), because a token that cannot open a
/// pull request is a run that fails at 2am having already done the work.
///
/// The order is deliberate — verify, then keychain, then the row. A row that
/// claimed a credential the keychain does not have would make every later run
/// of that repository refuse, which is a worse state than the one the user was
/// trying to leave.
#[tauri::command]
pub async fn set_repository_credential(
    state: State<'_, AppState>,
    id: String,
    token: String,
    label: Option<String>,
) -> Result<repo::CredentialStatus> {
    let repository = repo::get(&state.context, &id).await?;
    let secret = Secret::new(token)?;

    let owner_repo = repo::remote_info(&repository)
        .await
        .ok()
        .and_then(|remote| remote.remote_url)
        .as_deref()
        .and_then(provision::owner_repo_from_remote);

    let verification =
        provision::verify(provision::default_gh(), &secret, owner_repo.as_deref()).await;

    if let Verification::Rejected { reason } = &verification {
        return Err(Error::invalid(reason.clone()));
    }
    if let Verification::Unverifiable { reason } = &verification {
        // Saved anyway, and the absent login is what marks it: a missing local
        // tool says nothing about the token, and refusing here would make the
        // feature unusable on a machine with git but not `gh`.
        tracing::warn!(repository = %repository.name, %reason, "storing an unverified credential");
    }

    state.runner.credentials.set(&id, secret).await?;
    let stored =
        repo::set_credential_metadata(&state.context, &id, verification.login(), label.as_deref())
            .await?;

    credential_status(&state, &stored).await
}

/// Removes it, keychain first.
///
/// Keychain before row for the mirror of the save's reason: a row cleared while
/// the item survived would leave a secret on the machine that nothing in Rimaia
/// can find again to delete.
#[tauri::command]
pub async fn remove_repository_credential(
    state: State<'_, AppState>,
    id: String,
) -> Result<repo::CredentialStatus> {
    state.runner.credentials.delete(&id).await?;
    let cleared = repo::clear_credential_metadata(&state.context, &id).await?;

    credential_status(&state, &cleared).await
}

async fn credential_status(
    state: &State<'_, AppState>,
    repository: &rimaia_core::db::Repository,
) -> Result<repo::CredentialStatus> {
    // Best-effort: a `git remote` that cannot be read is not a reason a
    // credential pane cannot open, and the SSH notice is a caveat rather than a
    // gate.
    let ssh_remote = repo::remote_info(repository)
        .await
        .ok()
        .and_then(|remote| remote.remote_url)
        .is_some_and(|url| url.starts_with("git@") || url.starts_with("ssh://"));

    Ok(repo::CredentialStatus {
        configured: repo::has_credential(repository),
        login: repository.credential_login.clone(),
        label: repository.credential_label.clone(),
        added_at: repository.credential_added_at,
        store: state.runner.credentials.status(&repository.id).await,
        ssh_remote,
    })
}
