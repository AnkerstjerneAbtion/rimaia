//! Who is in a team, and what changes when someone leaves it (task 051
//! Scopes 2 and 4).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqliteConnection;

use super::{
    ensure_an_owner_remains, invitations, refuse_solo, require_owner, role_of, team_row,
    NO_SUCH_TEAM, PERSONAL_TEAM_KEEPS_ITS_PERSON,
};
use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::events::{ChangeEvent, TaskId, UserId};
use crate::identity::Role;

/// One person in a team, as the Team section lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    pub user_id: UserId,
    pub login: String,
    pub avatar_url: Option<String>,
    pub role: Role,
    pub joined_at: DateTime<Utc>,
    /// Whether this is the person a personal team belongs to, who can be
    /// neither removed nor demoted, and cannot leave.
    pub is_personal_owner: bool,
}

/// Every member of `team_id`, in the order they joined. Any member may read
/// it; a team outside the scope is [`NO_SUCH_TEAM`].
pub async fn list_team_members(ctx: &ServiceContext, team_id: &str) -> Result<Vec<TeamMember>> {
    if !ctx.scope.contains(team_id) {
        return Err(Error::not_found(NO_SUCH_TEAM));
    }
    let members = sqlx::query!(
        r#"SELECT u.id AS user_id, u.login, u.avatar_url, m.role,
                  m.created_at AS "joined_at: DateTime<Utc>",
                  COALESCE(t.personal_user_id = u.id, 0) AS "is_personal_owner!: bool"
             FROM team_memberships m
             JOIN users u ON u.id = m.user_id
             JOIN teams t ON t.id = m.team_id
            WHERE m.team_id = ?1
            ORDER BY m.created_at, u.login, u.id"#,
        team_id,
    )
    .fetch_all(&ctx.pool)
    .await?;

    members
        .into_iter()
        .map(|member| {
            Ok(TeamMember {
                role: member.role.parse()?,
                user_id: member.user_id,
                login: member.login,
                avatar_url: member.avatar_url,
                joined_at: member.joined_at,
                is_personal_owner: member.is_personal_owner,
            })
        })
        .collect()
}

/// Makes `user_id` an owner or a member of `team_id`. Owner only.
///
/// Demoting a personal team's own person is refused, and so is leaving the
/// team with no owner (the last-owner rule, applied after the write). A
/// demotion to member revokes the links the person made while an owner: a
/// member cannot invite, and an owner's link kept from before must not undo
/// the demotion.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), team_id = %team_id))]
pub async fn set_member_role(
    ctx: &ServiceContext,
    team_id: &str,
    user_id: &str,
    role: Role,
) -> Result<()> {
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;
    require_owner(&mut tx, ctx, team_id, "change someone's role").await?;
    ensure_member(&mut tx, team_id, user_id).await?;
    let team = team_row(&mut tx, team_id).await?;
    if role == Role::Member && team.personal_user_id.as_deref() == Some(user_id) {
        return Err(Error::invalid(PERSONAL_TEAM_KEEPS_ITS_PERSON));
    }

    let role_text = role.as_str();
    sqlx::query!(
        "UPDATE team_memberships SET role = ?1 WHERE team_id = ?2 AND user_id = ?3",
        role_text,
        team_id,
        user_id,
    )
    .execute(&mut *tx)
    .await?;
    if role == Role::Member {
        invitations::revoke_links_made_by(&mut tx, ctx.clock.as_ref(), team_id, user_id).await?;
    }
    ensure_an_owner_remains(&mut tx, team_id).await?;
    tx.commit().await?;

    ctx.publish(ChangeEvent::teams(team_id.to_string()));
    Ok(())
}

/// Takes `user_id` out of `team_id` and withdraws them from its work. Owner
/// only.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), team_id = %team_id))]
pub async fn remove_member(ctx: &ServiceContext, team_id: &str, user_id: &str) -> Result<()> {
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;
    require_owner(&mut tx, ctx, team_id, "remove someone from the team").await?;
    ensure_member(&mut tx, team_id, user_id).await?;
    let tasks = depart(&mut tx, ctx.clock.as_ref(), team_id, user_id).await?;
    tx.commit().await?;

    announce_departure(ctx, team_id, tasks);
    Ok(())
}

/// Takes the caller out of `team_id`, owners included, and withdraws them from
/// its work.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), team_id = %team_id))]
pub async fn leave_team(ctx: &ServiceContext, team_id: &str) -> Result<()> {
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;
    if !ctx.scope.contains(team_id) || role_of(&mut tx, team_id, &ctx.actor).await?.is_none() {
        return Err(Error::not_found(NO_SUCH_TEAM));
    }
    let tasks = depart(&mut tx, ctx.clock.as_ref(), team_id, &ctx.actor).await?;
    tx.commit().await?;

    announce_departure(ctx, team_id, tasks);
    Ok(())
}

/// A removal's or a departure's events: the team's, and the tasks it changed,
/// never with an empty list (ADR-0018). Without the second a card keeps
/// showing the old assignee.
fn announce_departure(ctx: &ServiceContext, team_id: &str, tasks: Vec<TaskId>) {
    ctx.publish(ChangeEvent::teams(team_id.to_string()));
    ctx.publish(ChangeEvent::tasks(team_id.to_string(), tasks));
}

/// `user_id`'s departure from `team_id`, on the caller's `BEGIN IMMEDIATE`
/// transaction: the personal-team refusal, the withdrawal, the membership's
/// delete, then the last-owner rule. The ids of the tasks it changed.
async fn depart(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    team_id: &str,
    user_id: &str,
) -> Result<Vec<TaskId>> {
    let team = team_row(conn, team_id).await?;
    if team.personal_user_id.as_deref() == Some(user_id) {
        return Err(Error::invalid(PERSONAL_TEAM_KEEPS_ITS_PERSON));
    }
    let tasks = remove_member_in(conn, clock, team_id, user_id).await?;
    ensure_an_owner_remains(conn, team_id).await?;
    Ok(tasks)
}

/// Withdraws `user_id` from `team_id`'s work and deletes their membership, on
/// the caller's transaction, with no check of who asked: [`remove_member`]
/// and [`leave_team`] apply those first, and the `testing` member helper,
/// which stands in for a removal, calls this directly.
///
/// The withdrawal, so that a person invited back starts with no trust and no
/// restricted token, and no runner of theirs can reach the team's work:
///
/// - their tasks in the team are unassigned;
/// - every pin on the team's tasks naming one of their runners is cleared
///   (057 replaces this with its `release_pin`);
/// - their runners leave the team's pool;
/// - every lease their runners hold on the team's tasks expires now, rather
///   than being deleted, so 053's sweep closes its run as `interrupted`; a
///   deleted lease would leave the run `running` with nothing to close it;
/// - their trust rows in the team go in both directions;
/// - every invitation into the team they made that could still be used is
///   revoked, so neither they nor anyone they gave a link to gets in on the
///   authority they no longer have;
/// - a token of theirs restricted to this team alone is deleted, and one
///   restricted to it and others loses this team only: D28 reads no rows as
///   every team, so a restriction whose last row went would widen the token.
///
/// Every `UPDATE` of `tasks` returns its ids, and the leases' task ids are
/// collected with them: the tasks a `tasks` event names.
pub(crate) async fn remove_member_in(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    team_id: &str,
    user_id: &str,
) -> Result<Vec<TaskId>> {
    let now = clock.now();
    let mut changed: Vec<TaskId> = Vec::new();

    changed.extend(
        sqlx::query_scalar!(
            "UPDATE tasks SET assignee_id = NULL, assigned_by = NULL
              WHERE team_id = ?1 AND assignee_id = ?2
             RETURNING id",
            team_id,
            user_id,
        )
        .fetch_all(&mut *conn)
        .await?,
    );
    changed.extend(
        sqlx::query_scalar!(
            "UPDATE tasks SET pinned_runner_id = NULL
              WHERE team_id = ?1
                AND pinned_runner_id IN (SELECT id FROM runners WHERE user_id = ?2)
             RETURNING id",
            team_id,
            user_id,
        )
        .fetch_all(&mut *conn)
        .await?,
    );
    sqlx::query!(
        "DELETE FROM runner_pool_teams
          WHERE team_id = ?1 AND runner_id IN (SELECT id FROM runners WHERE user_id = ?2)",
        team_id,
        user_id,
    )
    .execute(&mut *conn)
    .await?;
    changed.extend(
        sqlx::query_scalar!(
            "UPDATE runner_leases SET expires_at = ?3
              WHERE runner_id IN (SELECT id FROM runners WHERE user_id = ?2)
                AND task_id IN (SELECT id FROM tasks WHERE team_id = ?1)
                AND (expires_at IS NULL OR expires_at > ?3)
             RETURNING task_id",
            team_id,
            user_id,
            now,
        )
        .fetch_all(&mut *conn)
        .await?,
    );
    sqlx::query!(
        "DELETE FROM trusted_authors
          WHERE team_id = ?1 AND (user_id = ?2 OR trusted_user_id = ?2)",
        team_id,
        user_id,
    )
    .execute(&mut *conn)
    .await?;
    invitations::revoke_links_made_by(&mut *conn, clock, team_id, user_id).await?;
    sqlx::query!(
        "DELETE FROM api_tokens
          WHERE user_id = ?2
            AND id IN (SELECT token_id FROM api_token_teams WHERE team_id = ?1)
            AND NOT EXISTS (SELECT 1 FROM api_token_teams o
                             WHERE o.token_id = api_tokens.id AND o.team_id <> ?1)",
        team_id,
        user_id,
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "DELETE FROM api_token_teams
          WHERE team_id = ?1 AND token_id IN (SELECT id FROM api_tokens WHERE user_id = ?2)",
        team_id,
        user_id,
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "DELETE FROM team_memberships WHERE team_id = ?1 AND user_id = ?2",
        team_id,
        user_id,
    )
    .execute(&mut *conn)
    .await?;

    let mut seen = std::collections::BTreeSet::new();
    changed.retain(|id| seen.insert(id.clone()));
    Ok(changed)
}

/// `NotFound` for someone who is not a member of `team_id`, worded as for a
/// user who does not exist (039's rule).
async fn ensure_member(conn: &mut SqliteConnection, team_id: &str, user_id: &str) -> Result<()> {
    crate::consent::ensure_member(conn, team_id, user_id).await
}
