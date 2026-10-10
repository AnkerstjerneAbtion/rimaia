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

use rimaia_core::archive;
use rimaia_core::credentials::provision::{self, Verification};
use rimaia_core::credentials::Secret;
use rimaia_core::db::{OnArchive, Repository};
use rimaia_core::machine::{self, Checkout, CheckoutView};
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
///
/// The board's half only. `worktreeRoot` left in task 066 for
/// [`set_repository_worktree_root`]: it is this machine's setting (D32's
/// appendix), and an unknown key off the wire is ignored rather than refused,
/// as it always was.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRepositoryInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
}

/// Every registered repository, alphabetically — the order Settings shows
/// them in. The board's half, with no path (task 066); this machine's clones
/// are [`list_checkouts`].
#[tauri::command]
pub async fn list_repositories(state: State<'_, AppState>) -> Result<Vec<Repository>> {
    repo::list(&state.context).await
}

/// This machine's checkout of every repository it has one of (task 066): the
/// per-machine fields the board's `Repository` no longer carries.
///
/// A local command (D32's appendix). Its one board fact, which repositories
/// the caller can see, comes through `repo::checkouts`'s named read.
#[tauri::command]
pub async fn list_checkouts(state: State<'_, AppState>) -> Result<Vec<CheckoutView>> {
    repo::checkouts(&state.context, &state.machine).await
}

/// Validates and registers a local repository (task 003's four checks; each
/// produces its own message): the board's row, then this machine's checkout.
/// A local command until task 054 splits it (D32's appendix).
#[tauri::command]
pub async fn register_repository(
    state: State<'_, AppState>,
    input: RegisterRepositoryInput,
) -> Result<Repository> {
    let worktrees_dir = state.paths.worktrees_dir();
    repo::register(
        &state.context,
        &state.machine,
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
        },
    )
    .await
}

/// Moves where this machine creates a repository's worktrees (task 066, D32's
/// Binds): `update_repository`'s `worktreeRoot`, now this machine's checkout.
#[tauri::command]
pub async fn set_repository_worktree_root(
    state: State<'_, AppState>,
    repository_id: String,
    worktree_root: String,
) -> Result<CheckoutView> {
    repo::set_worktree_root(
        &state.context,
        &state.machine,
        &repository_id,
        worktree_root,
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
) -> Result<CheckoutView> {
    repo::set_max_concurrency(&state.context, &state.machine, &id, max_concurrency).await
}

/// Chooses what archiving a task in this repository cleans up (ADR-0025
/// point 4).
///
/// Both halves in one call, because the mode and the path are **one**
/// decision: a row spelling `script` with nothing to run is not one of the
/// three states, and two commands would make that intermediate reachable. The
/// script path is validated here rather than when an archive fires, which is
/// the only moment there is a human present to read the refusal.
#[tauri::command]
pub async fn set_repository_on_archive(
    state: State<'_, AppState>,
    id: String,
    on_archive: OnArchive,
    script: Option<String>,
) -> Result<CheckoutView> {
    archive::set_repository_on_archive(&state.context, &state.machine, &id, on_archive, script)
        .await
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
) -> Result<CheckoutView> {
    repo::set_allow_unattended_runs(&state.context, &state.machine, &id, allow).await
}

/// Sets the team ceiling: whether the repository's team allows unattended runs
/// in it at all (ADR-0032 point 4). A board command, refused to a member and
/// on a personal team; [`set_repository_unattended_runs`] stays this runner's
/// own consent.
#[tauri::command]
pub async fn set_repository_unattended_ceiling(
    state: State<'_, AppState>,
    id: String,
    allowed: bool,
) -> Result<Repository> {
    repo::set_repository_unattended_ceiling(&state.context, &id, allowed).await
}

/// Removes a repository. Refused, naming how many, when any task still
/// references it; then this machine forgets its worktree records and its
/// checkout (task 066).
#[tauri::command]
pub async fn remove_repository(state: State<'_, AppState>, id: String) -> Result<()> {
    repo::remove(&state.context, Some(&state.machine), &id).await
}

/// Fresh inspection of a repository's remote and `gh` readiness — never
/// cached, per [`repo::remote_info`]'s own doc comment.
#[tauri::command]
pub async fn get_repository_remote_info(
    state: State<'_, AppState>,
    id: String,
) -> Result<RemoteInfo> {
    let repository = repo::get(&state.context, &id).await?;
    repo::remote_info(&machine::checkout_of(&state.machine, &repository).await?).await
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
    let checkout = machine::checkout_of(&state.machine, &repository).await?;
    credential_status(&state, &repository, &checkout).await
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
    let checkout = machine::checkout_of(&state.machine, &repository).await?;
    let secret = Secret::new(token)?;

    let owner_repo = repo::remote_info(&checkout)
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
    let stored = repo::set_credential_metadata(
        &state.context,
        &state.machine,
        &id,
        verification.login(),
        label.as_deref(),
    )
    .await?;

    credential_status(&state, &repository, &stored).await
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
    let repository = repo::get(&state.context, &id).await?;
    state.runner.credentials.delete(&id).await?;
    let cleared = repo::clear_credential_metadata(&state.context, &state.machine, &id).await?;

    credential_status(&state, &repository, &cleared).await
}

/// The pane's view of one checkout's credential: the metadata off this
/// machine's checkout (task 066), the keychain's own answer beside it.
async fn credential_status(
    state: &State<'_, AppState>,
    repository: &Repository,
    checkout: &Checkout,
) -> Result<repo::CredentialStatus> {
    // Best-effort: a `git remote` that cannot be read is not a reason a
    // credential pane cannot open, and the SSH notice is a caveat rather than a
    // gate.
    let ssh_remote = repo::remote_info(checkout)
        .await
        .ok()
        .and_then(|remote| remote.remote_url)
        .is_some_and(|url| url.starts_with("git@") || url.starts_with("ssh://"));

    Ok(repo::CredentialStatus {
        configured: repo::has_credential(checkout),
        login: checkout.credential_login.clone(),
        label: checkout.credential_label.clone(),
        added_at: checkout.credential_added_at,
        store: state.runner.credentials.status(&repository.id).await,
        ssh_remote,
    })
}
