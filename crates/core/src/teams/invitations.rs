//! Invitation links (task 051 Scope 5, ADR-0029 point 4).
//!
//! An owner makes a link; anyone signed in who holds it joins the team once,
//! with the role it names, within seven days, unless an owner revokes it
//! first. The token is the capability: it is returned once, when it is made,
//! stored only as `identity::secret::hash` of the whole string, and never
//! read back, logged or put in a URL by core. The client builds the link,
//! `<origin>/invite#<token>`, from its own origin.
//!
//! **Every unusable token gets one answer.** Unknown, expired, revoked and
//! already accepted are all [`INVALID_LINK`]: whoever holds one can do the
//! same thing in every case, so telling them apart would help nobody but
//! someone probing for tokens.

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::SqliteConnection;

use super::{refuse_solo, require_owner, role_of, team_row, Team, NO_SUCH_TEAM};
use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::db::new_id;
use crate::error::{Error, Result};
use crate::events::{ChangeEvent, TeamId, UserId};
use crate::identity::secret::{self, Secret};
use crate::identity::teams::listing_of;
use crate::identity::Role;

/// How long a link works after it is made.
pub const INVITATION_LIFETIME_DAYS: i64 = 7;

/// The one answer to an unusable token, from the preview and the acceptance
/// alike.
pub const INVALID_LINK: &str =
    "this invitation link is not valid any more: ask a team owner for a new one";

/// The refusal for revoking a link someone already used.
pub const ALREADY_ACCEPTED: &str =
    "this invitation was already accepted: remove the member instead";

/// What a link's path is, after the origin the client supplies. The MCP tool
/// returns it beside the token, because an agent has no origin of its own to
/// build it from.
pub fn invitation_path(token: &Secret) -> String {
    format!("/invite#{}", token.expose())
}

/// Where an invitation stands, derived from its stamps and the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvitationStatus {
    Pending,
    Accepted,
    Expired,
    Revoked,
}

/// One invitation, as an owner lists it. Never its token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invitation {
    pub id: String,
    pub team_id: TeamId,
    pub role: Role,
    pub status: InvitationStatus,
    /// Who made it, or `None` for a former member.
    pub created_by: Option<UserId>,
    pub created_by_login: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Who used it: the team's record of who let whom in.
    pub accepted_by: Option<UserId>,
    pub accepted_by_login: Option<String>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// A new invitation, the one time its token is shown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedInvitation {
    pub invitation: Invitation,
    pub token: Secret,
}

/// What the accept page shows before the person joins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvitationPreview {
    pub team_name: String,
    pub role: Role,
    pub invited_by_login: Option<String>,
    pub expires_at: DateTime<Utc>,
}

/// A link into `team_id` granting `role`. Owner only.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), team_id = %team_id))]
pub async fn create_invitation(
    ctx: &ServiceContext,
    team_id: &str,
    role: Role,
) -> Result<CreatedInvitation> {
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;
    require_owner(&mut tx, ctx, team_id, "invite people").await?;
    let (id, token) =
        create_invitation_in(&mut tx, ctx.clock.as_ref(), team_id, role, &ctx.actor).await?;
    let invitation = read(&mut tx, ctx.clock.now(), &id).await?;
    tx.commit().await?;
    Ok(CreatedInvitation { invitation, token })
}

/// [`create_invitation`]'s insert, on the caller's transaction, with no check
/// of who asked: the `testing` member helper calls it directly, as the
/// stand-in for an owner's link.
pub(crate) async fn create_invitation_in(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    team_id: &str,
    role: Role,
    created_by: &str,
) -> Result<(String, Secret)> {
    let id = new_id();
    let token = secret::mint_invitation_token();
    let token_hash = secret::hash(token.expose());
    let created_at = clock.now();
    let expires_at = created_at + Duration::days(INVITATION_LIFETIME_DAYS);
    let role = role.as_str();
    sqlx::query!(
        "INSERT INTO invitations (id, team_id, token_hash, role, created_by, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        id,
        team_id,
        token_hash,
        role,
        created_by,
        created_at,
        expires_at,
    )
    .execute(&mut *conn)
    .await?;
    Ok((id, token))
}

/// Every invitation of `team_id`, newest first, used or not. Owner only.
pub async fn list_invitations(ctx: &ServiceContext, team_id: &str) -> Result<Vec<Invitation>> {
    let mut tx = ctx.begin_immediate().await?;
    require_owner(&mut tx, ctx, team_id, "see the team's invitations").await?;
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT i.id, i.team_id, i.role,
                  i.created_by, c.login AS "created_by_login?",
                  i.created_at AS "created_at: DateTime<Utc>",
                  i.expires_at AS "expires_at: DateTime<Utc>",
                  i.accepted_by, a.login AS "accepted_by_login?",
                  i.accepted_at AS "accepted_at: DateTime<Utc>",
                  i.revoked_at AS "revoked_at: DateTime<Utc>"
             FROM invitations i
             LEFT JOIN users c ON c.id = i.created_by
             LEFT JOIN users a ON a.id = i.accepted_by
            WHERE i.team_id = ?1
            ORDER BY i.created_at DESC, i.id"#,
        team_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let now = ctx.clock.now();
    rows.into_iter()
        .map(|row| row.into_invitation(now))
        .collect()
}

/// Revokes an invitation, whichever owner made it. Owner of its team only.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str()))]
pub async fn revoke_invitation(ctx: &ServiceContext, invitation_id: &str) -> Result<()> {
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;
    let scope = tx.scope().json();
    let Some(found) = sqlx::query!(
        r#"SELECT team_id, accepted_at IS NOT NULL AS "accepted!: bool" FROM invitations
            WHERE id = ?1 AND team_id IN (SELECT value FROM json_each(?2))"#,
        invitation_id,
        scope,
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Err(Error::not_found("There is no invitation with that id."));
    };
    require_owner(&mut tx, ctx, &found.team_id, "revoke an invitation").await?;
    if found.accepted {
        return Err(Error::invalid(ALREADY_ACCEPTED));
    }
    let now = ctx.clock.now();
    sqlx::query!(
        "UPDATE invitations SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
        now,
        invitation_id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    ctx.publish(ChangeEvent::teams(found.team_id));
    Ok(())
}

/// What a usable token would admit its holder to. The token is the
/// capability, so this is the one read that names a team outside the
/// caller's scope.
pub async fn preview_invitation(ctx: &ServiceContext, token: &str) -> Result<InvitationPreview> {
    let mut conn = ctx.pool.acquire().await?;
    refuse_solo(&mut conn).await?;
    let token_hash = secret::hash(token);
    let now = ctx.clock.now();
    let preview = sqlx::query!(
        r#"SELECT t.name AS team_name, i.role, c.login AS "invited_by_login?",
                  i.expires_at AS "expires_at: DateTime<Utc>"
             FROM invitations i
             JOIN teams t ON t.id = i.team_id
             LEFT JOIN users c ON c.id = i.created_by
            WHERE i.token_hash = ?1 AND i.accepted_at IS NULL AND i.revoked_at IS NULL
              AND i.expires_at > ?2"#,
        token_hash,
        now,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found(INVALID_LINK))?;
    Ok(InvitationPreview {
        team_name: preview.team_name,
        role: preview.role.parse()?,
        invited_by_login: preview.invited_by_login,
        expires_at: preview.expires_at,
    })
}

/// Joins the team a usable token names, with its role, and uses the token up.
///
/// A caller who is already a member is refused, and the token stays unused:
/// an invitation is never a way to change one's own role, and a member who is
/// forwarded an owner's link must not use it up for the person it was for.
/// For the same reason a link is never usable by the person who made it: once
/// they are out of the team, their own link gets the one unusable-token
/// answer, so a demoted or removed owner cannot walk back in as an owner.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str()))]
pub async fn accept_invitation(ctx: &ServiceContext, token: &str) -> Result<Team> {
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;
    let team_id = accept_invitation_in(&mut tx, ctx.clock.as_ref(), token, &ctx.actor).await?;
    let team = listing_of(&mut tx, &ctx.actor, &team_id).await?;
    tx.commit().await?;

    ctx.publish(ChangeEvent::teams(team_id));
    Ok(team)
}

/// [`accept_invitation`]'s two statements, on the caller's transaction: the
/// conditional `UPDATE` that only a usable token's row matches, so single use
/// needs no lock of its own, then the membership. The team joined.
///
/// The two refusals after the `UPDATE` rely on the caller rolling back, which
/// leaves the invitation unused. A current member is told so before the
/// creator check, so an owner who opens their own link hears that they are
/// already in, not that the link is dead.
pub(crate) async fn accept_invitation_in(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    token: &str,
    user_id: &str,
) -> Result<TeamId> {
    let token_hash = secret::hash(token);
    let now = clock.now();
    let accepted = sqlx::query!(
        "UPDATE invitations SET accepted_by = ?1, accepted_at = ?2
          WHERE token_hash = ?3 AND accepted_at IS NULL AND revoked_at IS NULL
            AND expires_at > ?2
         RETURNING team_id, role, created_by",
        user_id,
        now,
        token_hash,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found(INVALID_LINK))?;

    if role_of(conn, &accepted.team_id, user_id).await?.is_some() {
        let team = team_row(conn, &accepted.team_id).await?;
        return Err(Error::invalid(format!(
            "you are already a member of {}",
            team.name
        )));
    }
    if accepted.created_by.as_deref() == Some(user_id) {
        return Err(Error::not_found(INVALID_LINK));
    }
    sqlx::query!(
        "INSERT INTO team_memberships (team_id, user_id, role, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        accepted.team_id,
        user_id,
        accepted.role,
        now,
    )
    .execute(&mut *conn)
    .await?;
    Ok(accepted.team_id)
}

/// Revokes every link `created_by` made into `team_id` that could still be
/// used, on the caller's transaction: what a removal, a departure and a
/// demotion to member do (seam-contract D41 point 6). A link carries its
/// maker's authority to admit, and someone who lost that authority must not
/// keep exercising it through a token they still hold or already handed out.
/// Accepted and expired rows are left as they are, so the history still says
/// what happened to them.
pub(crate) async fn revoke_links_made_by(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    team_id: &str,
    created_by: &str,
) -> Result<()> {
    let now = clock.now();
    sqlx::query!(
        "UPDATE invitations SET revoked_at = ?3
          WHERE team_id = ?1 AND created_by = ?2
            AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > ?3",
        team_id,
        created_by,
        now,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One invitation by id, for the answer to its creation.
async fn read(conn: &mut SqliteConnection, now: DateTime<Utc>, id: &str) -> Result<Invitation> {
    sqlx::query_as!(
        Row,
        r#"SELECT i.id, i.team_id, i.role,
                  i.created_by, c.login AS "created_by_login?",
                  i.created_at AS "created_at: DateTime<Utc>",
                  i.expires_at AS "expires_at: DateTime<Utc>",
                  i.accepted_by, a.login AS "accepted_by_login?",
                  i.accepted_at AS "accepted_at: DateTime<Utc>",
                  i.revoked_at AS "revoked_at: DateTime<Utc>"
             FROM invitations i
             LEFT JOIN users c ON c.id = i.created_by
             LEFT JOIN users a ON a.id = i.accepted_by
            WHERE i.id = ?1"#,
        id,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found(NO_SUCH_TEAM))?
    .into_invitation(now)
}

struct Row {
    id: String,
    team_id: String,
    role: String,
    created_by: Option<String>,
    created_by_login: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    accepted_by: Option<String>,
    accepted_by_login: Option<String>,
    accepted_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
}

impl Row {
    fn into_invitation(self, now: DateTime<Utc>) -> Result<Invitation> {
        let status = if self.accepted_at.is_some() {
            InvitationStatus::Accepted
        } else if self.revoked_at.is_some() {
            InvitationStatus::Revoked
        } else if self.expires_at <= now {
            InvitationStatus::Expired
        } else {
            InvitationStatus::Pending
        };
        Ok(Invitation {
            role: self.role.parse()?,
            status,
            id: self.id,
            team_id: self.team_id,
            created_by: self.created_by,
            created_by_login: self.created_by_login,
            created_at: self.created_at,
            expires_at: self.expires_at,
            accepted_by: self.accepted_by,
            accepted_by_login: self.accepted_by_login,
            accepted_at: self.accepted_at,
            revoked_at: self.revoked_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{create_personal_team, insert_team};
    use crate::testing::{test_epoch, test_pool, TestClock};
    use crate::ErrorCode;
    use pretty_assertions::assert_eq;

    /// A state no service leaves behind, because every way out of a team, and
    /// a demotion, revokes the person's links: a usable link whose maker is
    /// not a member. The creator check is what still refuses it, should a
    /// later path that ends a membership forget the revocation.
    #[tokio::test]
    async fn a_link_is_never_usable_by_the_person_who_made_it() {
        let pool = test_pool().await;
        let clock = TestClock::new(test_epoch());
        let mut conn = pool.acquire().await.expect("a connection");
        let alice = create_personal_team(&mut conn, &clock, "alice")
            .await
            .expect("alice signs up");
        let carol = create_personal_team(&mut conn, &clock, "carol")
            .await
            .expect("carol signs up");
        let team = insert_team(&mut conn, &clock, "Acme", None, &alice.user_id)
            .await
            .expect("alice's team");
        let (id, token) =
            create_invitation_in(&mut conn, &clock, &team, Role::Owner, &carol.user_id)
                .await
                .expect("a link carol made");
        drop(conn);

        let mut tx = pool.begin().await.expect("a transaction");
        let error = accept_invitation_in(&mut tx, &clock, token.expose(), &carol.user_id)
            .await
            .expect_err("carol cannot use her own link");
        drop(tx);

        assert_eq!(error.code(), ErrorCode::NotFound);
        assert_eq!(error.to_string(), INVALID_LINK);
        let mut conn = pool.acquire().await.expect("a connection");
        assert_eq!(
            role_of(&mut conn, &team, &carol.user_id)
                .await
                .expect("read"),
            None
        );
        let accepted_at: Option<String> =
            sqlx::query_scalar("SELECT accepted_at FROM invitations WHERE id = ?1")
                .bind(&id)
                .fetch_one(&mut *conn)
                .await
                .expect("the invitation");
        assert_eq!(accepted_at, None, "the link stays unused");
    }
}
