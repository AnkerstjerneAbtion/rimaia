//! `list_teams`: the caller's memberships (task 050 Scope 4).
//!
//! A command about the person, not the team on screen, so the server never
//! narrows it with `Rimaia-Team` ([`ignores_team_header`]).
//!
//! [`ignores_team_header`]: crate::api::registry::ignores_team_header

use super::NoArgs;
use crate::api::BoardRequest;
use crate::error::Result;
use crate::identity::teams::{self, TeamListing};

/// Every team the caller belongs to, personal first, then by name.
pub async fn list_teams(request: BoardRequest, _args: NoArgs) -> Result<Vec<TeamListing>> {
    teams::list(&request.ctx).await
}
