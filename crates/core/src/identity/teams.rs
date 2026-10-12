//! The teams a person belongs to, as the switcher lists them (task 050).

use serde::Serialize;
use sqlx::SqliteConnection;

use super::Role;
use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::events::TeamId;

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
    /// How many people belong to the team, the caller included (task 051).
    pub member_count: i64,
    /// How many of them are owners. The account page reads this, with
    /// `personal`, to name the teams that block deleting the account before
    /// anyone types their login (task 051).
    pub owner_count: i64,
}

/// Every team the caller belongs to, personal team first, then by name.
///
/// Read by the caller's user and not by the context's scope: this is the
/// answer to "which boards can I look at", so it is the same whichever team a
/// request was narrowed to. The two doors that reach it, a browser's session
/// and a desktop's token, are never restricted to some teams.
pub async fn list(ctx: &ServiceContext) -> Result<Vec<TeamListing>> {
    let mut conn = ctx.pool.acquire().await?;
    listings(&mut conn, &ctx.actor, None).await
}

/// `user_id`'s listing of the one team `team_id`, read on the caller's
/// connection: what creating a team and accepting an invitation answer, so
/// the switcher can select the team without a second read.
pub(crate) async fn listing_of(
    conn: &mut SqliteConnection,
    user_id: &str,
    team_id: &str,
) -> Result<TeamListing> {
    listings(conn, user_id, Some(team_id))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::not_found("There is no team with that id."))
}

async fn listings(
    conn: &mut SqliteConnection,
    user_id: &str,
    team_id: Option<&str>,
) -> Result<Vec<TeamListing>> {
    let memberships = sqlx::query!(
        r#"SELECT t.id, t.name, m.role,
                  COALESCE(t.personal_user_id = ?1, 0) AS "personal!: bool",
                  (SELECT count(*) FROM team_memberships c
                    WHERE c.team_id = t.id) AS "member_count!: i64",
                  (SELECT count(*) FROM team_memberships c
                    WHERE c.team_id = t.id AND c.role = 'owner') AS "owner_count!: i64"
             FROM team_memberships m JOIN teams t ON t.id = m.team_id
            WHERE m.user_id = ?1 AND (?2 IS NULL OR t.id = ?2)
            ORDER BY 4 DESC, t.name, t.id"#,
        user_id,
        team_id,
    )
    .fetch_all(&mut *conn)
    .await?;

    memberships
        .into_iter()
        .map(|team| {
            Ok(TeamListing {
                role: team.role.parse()?,
                personal: team.personal,
                member_count: team.member_count,
                owner_count: team.owner_count,
                id: team.id,
                name: team.name,
            })
        })
        .collect()
}
