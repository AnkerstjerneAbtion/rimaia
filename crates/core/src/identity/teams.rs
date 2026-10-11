//! The teams a person belongs to, as the switcher lists them (task 050).

use serde::Serialize;

use super::Role;
use crate::context::ServiceContext;
use crate::error::Result;
use crate::events::{TeamId, UserId};

/// One membership of the caller's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamListing {
    pub id: TeamId,
    pub name: String,
    pub role: Role,
    /// Whether this is the caller's own personal team (ADR-0029 point 2): the
    /// one the switcher falls back to when the team on screen is gone.
    pub personal: bool,
}

/// Every team the caller belongs to, personal team first, then by name.
///
/// Read by the caller's user and not by the context's scope: this is the
/// answer to "which boards can I look at", so it is the same whichever team a
/// request was narrowed to. The two doors that reach it, a browser's session
/// and a desktop's token, are never restricted to some teams.
pub async fn list(ctx: &ServiceContext) -> Result<Vec<TeamListing>> {
    let user_id: &UserId = &ctx.actor;
    let memberships = sqlx::query!(
        r#"SELECT t.id, t.name, m.role,
                  COALESCE(t.personal_user_id = ?1, 0) AS "personal!: bool"
             FROM team_memberships m JOIN teams t ON t.id = m.team_id
            WHERE m.user_id = ?1
            ORDER BY 4 DESC, t.name, t.id"#,
        user_id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    memberships
        .into_iter()
        .map(|team| {
            Ok(TeamListing {
                role: team.role.parse()?,
                personal: team.personal,
                id: team.id,
                name: team.name,
            })
        })
        .collect()
}
