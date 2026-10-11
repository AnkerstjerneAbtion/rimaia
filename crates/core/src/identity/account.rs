//! Who the caller is, as the account page shows it (task 047 Scope 12).

use serde::Serialize;

use super::Role;
use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::events::{TeamId, UserId};

/// The signed-in user, their teams, and the credential this request used.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: UserId,
    pub login: String,
    pub avatar_url: Option<String>,
    pub teams: Vec<AccountTeam>,
    pub current: CurrentCredential,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountTeam {
    pub id: TeamId,
    pub name: String,
    pub role: Role,
}

/// The session or desktop token a request came through: what signing out
/// revokes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentCredential {
    pub kind: CurrentKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentKind {
    Session,
    Desktop,
}

/// The caller's account: the user row, and every team of theirs this
/// request's scope reaches, by name.
pub async fn get(ctx: &ServiceContext, current: CurrentCredential) -> Result<Account> {
    let user_id: &UserId = &ctx.actor;
    let user = sqlx::query!(
        "SELECT id, login, avatar_url FROM users WHERE id = ?1",
        user_id,
    )
    .fetch_optional(&ctx.pool)
    .await?
    .ok_or_else(|| Error::not_found("There is no such user."))?;
    let memberships = sqlx::query!(
        "SELECT t.id, t.name, m.role
           FROM team_memberships m JOIN teams t ON t.id = m.team_id
          WHERE m.user_id = ?1
          ORDER BY t.name, t.id",
        user_id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    let teams = memberships
        .into_iter()
        .filter(|team| ctx.scope.contains(&team.id))
        .map(|team| {
            Ok(AccountTeam {
                role: team.role.parse()?,
                id: team.id,
                name: team.name,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Account {
        id: user.id,
        login: user.login,
        avatar_url: user.avatar_url,
        teams,
        current,
    })
}
