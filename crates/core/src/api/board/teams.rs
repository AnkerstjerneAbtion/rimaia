//! The team commands: the caller's memberships (task 050 Scope 4), and
//! forming, joining, leaving and deleting teams (task 051 Scope 11).
//!
//! Every row here is about the person or names its team by id or by a
//! token, so the server never narrows them with `Rimaia-Team`
//! ([`ignores_team_header`]).
//!
//! A write that changes someone's set of teams tells their open streams to
//! resolve their caller again, after the service has committed (ADR-0030
//! point 2, task 048's `StreamControl`): the service names the people, and
//! only this layer knows the host. A stream whose caller now reaches other
//! teams ends, and its reconnect opens with the new set, which is how a
//! creator's own stream comes to carry their new team's events.
//!
//! [`ignores_team_header`]: crate::api::registry::ignores_team_header

use serde::Deserialize;

use super::{ById, NoArgs};
use crate::api::BoardRequest;
use crate::db::Task;
use crate::error::Result;
use crate::identity::teams::{self, TeamListing};
use crate::identity::Role;
use crate::teams::copy::{self, CopyTarget};
use crate::teams::invitations::{self, CreatedInvitation, Invitation, InvitationPreview};
use crate::teams::members::{self, TeamMember};
use crate::teams::{self as team_service, delete, Team};

/// Every team the caller belongs to, personal first, then by name.
pub async fn list_teams(request: BoardRequest, _args: NoArgs) -> Result<Vec<TeamListing>> {
    teams::list(&request.ctx).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTeam {
    pub name: String,
}

/// A new team, the caller its only owner.
pub async fn create_team(request: BoardRequest, args: CreateTeam) -> Result<Team> {
    let team = team_service::create_team(&request.ctx, &args.name).await?;
    request.host.streams.revalidate(&request.caller.user_id);
    Ok(team)
}

/// `{ teamId }`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByTeamId {
    pub team_id: String,
}

/// Every member of a team the caller is in.
pub async fn list_team_members(request: BoardRequest, args: ByTeamId) -> Result<Vec<TeamMember>> {
    members::list_team_members(&request.ctx, &args.team_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetMemberRole {
    pub team_id: String,
    pub user_id: String,
    pub role: Role,
}

/// Makes a member an owner, or an owner a member. Owner only.
pub async fn set_member_role(request: BoardRequest, args: SetMemberRole) -> Result<()> {
    members::set_member_role(&request.ctx, &args.team_id, &args.user_id, args.role).await?;
    request.host.streams.revalidate(&args.user_id);
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveMember {
    pub team_id: String,
    pub user_id: String,
}

/// Takes someone out of a team. Owner only.
pub async fn remove_member(request: BoardRequest, args: RemoveMember) -> Result<()> {
    members::remove_member(&request.ctx, &args.team_id, &args.user_id).await?;
    request.host.streams.revalidate(&args.user_id);
    Ok(())
}

/// Takes the caller out of a team.
pub async fn leave_team(request: BoardRequest, args: ByTeamId) -> Result<()> {
    members::leave_team(&request.ctx, &args.team_id).await?;
    request.host.streams.revalidate(&request.caller.user_id);
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInvitation {
    pub team_id: String,
    pub role: Role,
}

/// An invitation link's token, the one time it is shown. Owner only.
pub async fn create_invitation(
    request: BoardRequest,
    args: CreateInvitation,
) -> Result<CreatedInvitation> {
    invitations::create_invitation(&request.ctx, &args.team_id, args.role).await
}

/// Every invitation of a team, newest first. Owner only.
pub async fn list_invitations(request: BoardRequest, args: ByTeamId) -> Result<Vec<Invitation>> {
    invitations::list_invitations(&request.ctx, &args.team_id).await
}

/// Revokes an invitation. Owner of its team only.
pub async fn revoke_invitation(request: BoardRequest, args: ById) -> Result<()> {
    invitations::revoke_invitation(&request.ctx, &args.id).await
}

/// `{ token }`: an invitation token, which travels in a command's body and
/// never in a URL, so no request line or access log carries it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByToken {
    pub token: String,
}

/// What a link would admit its holder to.
pub async fn preview_invitation(request: BoardRequest, args: ByToken) -> Result<InvitationPreview> {
    invitations::preview_invitation(&request.ctx, &args.token).await
}

/// Joins the team a link names.
pub async fn accept_invitation(request: BoardRequest, args: ByToken) -> Result<Team> {
    let team = invitations::accept_invitation(&request.ctx, &args.token).await?;
    request.host.streams.revalidate(&request.caller.user_id);
    Ok(team)
}

/// Every team the caller is in, with its repositories, for the copy's picker.
pub async fn list_copy_targets(request: BoardRequest, _args: NoArgs) -> Result<Vec<CopyTarget>> {
    copy::list_copy_targets(&request.ctx).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyTaskToTeam {
    pub task_id: String,
    pub repository_id: String,
}

/// A new task in the repository's team, carrying the plan and nothing else.
pub async fn copy_task_to_team(request: BoardRequest, args: CopyTaskToTeam) -> Result<Task> {
    copy::copy_task_to_team(&request.ctx, &args.task_id, &args.repository_id).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteTeam {
    pub team_id: String,
    pub confirm_name: String,
}

/// Deletes a team and everything it owns. Owner only, confirmed by its name.
pub async fn delete_team(request: BoardRequest, args: DeleteTeam) -> Result<()> {
    let purged = delete::delete_team(&request.ctx, &args.team_id, &args.confirm_name).await?;
    for user_id in &purged.former_members {
        request.host.streams.revalidate(user_id);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAccount {
    pub confirm_login: String,
}

/// Deletes the caller's own account and its personal team, confirmed by
/// their login.
pub async fn delete_account(request: BoardRequest, args: DeleteAccount) -> Result<()> {
    let deleted = delete::delete_account(&request.ctx, &args.confirm_login).await?;
    for user_id in &deleted.affected_users {
        request.host.streams.revalidate(user_id);
    }
    Ok(())
}
