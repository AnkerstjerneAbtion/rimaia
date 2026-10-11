//! The board's half of the repository commands (task 003, ADR-0002, ADR-0005;
//! task 066, ADR-0028 point 2).
//!
//! Every validation lives in `crate::repo` — this file only reshapes the wire
//! args into that module's types and calls it, per [`api::board`](super)'s own
//! module doc. A repository's checkout, consent, cap, archive policy and
//! credential are this machine's, and their commands stay local in the shell.

use serde::Deserialize;

use super::{ById, NoArgs};
use crate::api::BoardRequest;
use crate::db::Repository;
use crate::repo::{self, RepositoryPatch};
use crate::Result;

/// What the frontend sends [`update_repository`]. Mirrors [`RepositoryPatch`]
/// field for field — every field left `None` leaves that column unchanged.
///
/// The board's half only. `worktreeRoot` left in task 066 for
/// `set_repository_worktree_root`: it is this machine's setting (D32's
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
/// are `list_checkouts`.
pub async fn list_repositories(request: BoardRequest, _: NoArgs) -> Result<Vec<Repository>> {
    repo::list(&request.ctx).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRepository {
    pub id: String,
    pub patch: UpdateRepositoryInput,
}

/// Edits an already-registered repository. Does not re-run git validation —
/// see [`repo::update`]'s own doc for why.
pub async fn update_repository(
    request: BoardRequest,
    args: UpdateRepository,
) -> Result<Repository> {
    repo::update(
        &request.ctx,
        &args.id,
        RepositoryPatch {
            name: args.patch.name,
            default_branch: args.patch.default_branch,
        },
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetRepositoryUnattendedCeiling {
    pub id: String,
    pub allowed: bool,
}

/// Sets the team ceiling: whether the repository's team allows unattended runs
/// in it at all (ADR-0032 point 4). Refused to a member and on a personal
/// team; `set_repository_unattended_runs` stays this runner's own consent.
pub async fn set_repository_unattended_ceiling(
    request: BoardRequest,
    args: SetRepositoryUnattendedCeiling,
) -> Result<Repository> {
    repo::set_repository_unattended_ceiling(&request.ctx, &args.id, args.allowed).await
}

/// Removes a repository. Refused, naming how many, when any task still
/// references it; then, given this machine, forgets its worktree records and
/// its checkout in the same call (task 066, task 046 point 3a). Without one,
/// the checkout stays on whichever machine mapped it.
pub async fn remove_repository(request: BoardRequest, args: ById) -> Result<()> {
    repo::remove(&request.ctx, request.machine.as_ref(), &args.id).await
}
