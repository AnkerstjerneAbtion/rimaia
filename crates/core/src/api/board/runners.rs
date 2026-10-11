//! The caller's own runners (task 050 Scope 4, ADR-0030 point 5).
//!
//! A runner works for exactly one user, so both commands are about the person
//! and are never narrowed by `Rimaia-Team`. They publish no `ChangeEvent`: the
//! account page re-reads after its own write.

use super::{ById, NoArgs};
use crate::api::BoardRequest;
use crate::error::Result;
use crate::identity::runners::{self, RunnerListing};

/// The caller's paired runners, with what the board knows about each.
pub async fn list_runners(request: BoardRequest, _args: NoArgs) -> Result<Vec<RunnerListing>> {
    runners::list(&request.ctx).await
}

/// Unpairs one of the caller's runners: its tokens are revoked and its row
/// stays, because runs keep naming it. The caller's open streams resolve
/// again, as revoking the runner's token does.
pub async fn unpair_runner(request: BoardRequest, args: ById) -> Result<()> {
    runners::unpair_own(&request.ctx, &args.id).await?;
    request.host.streams.revalidate(&request.caller.user_id);
    Ok(())
}
