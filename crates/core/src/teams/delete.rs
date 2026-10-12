//! Deleting a team, and deleting an account (task 051 Scopes 6 and 7,
//! ADR-0029 point 6, ADR-0037 point 6).
//!
//! Deletion is immediate in the live database. [`purge`] is the only place a
//! team's rows are deleted, in the order D28's Why requires: the store's
//! `RESTRICT` references refuse any other, because inside a cascade SQLite
//! checks a `RESTRICT` row by row in no guaranteed order. Nothing here tells a
//! runner to delete anything: its worktrees are its owner's files.

use std::collections::BTreeSet;

use serde::Serialize;
use sqlx::SqliteConnection;
use tracing::Instrument;

use super::{invitations, refuse_solo, require_owner, team_row};
use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::events::{per_team, ChangeEvent, RepositoryId, TaskId, TeamId, UserId};

/// The refusal for a confirmation that is not the team's name, exactly.
pub const TYPE_THE_TEAM_NAME: &str = "type the team's name exactly to delete it";

/// The refusal for deleting a personal team on its own.
pub const PERSONAL_TEAM_GOES_WITH_ITS_ACCOUNT: &str =
    "a personal team is deleted with its account, not on its own";

/// The refusal for a confirmation that is not the caller's login, exactly.
pub const TYPE_YOUR_LOGIN: &str = "type your login exactly to delete your account";

/// What [`purge`] deleted: published after commit under the deleted team's
/// id, and the former members, whose streams then resolve again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Purged {
    pub team_id: TeamId,
    pub task_ids: Vec<TaskId>,
    pub repository_ids: Vec<RepositoryId>,
    pub former_members: Vec<UserId>,
}

impl Purged {
    /// The events a deletion publishes after its commit: the deleted tasks,
    /// the deleted repositories, and the team itself.
    fn announce(&self, ctx: &ServiceContext) {
        ctx.publish(ChangeEvent::tasks(
            self.team_id.clone(),
            self.task_ids.clone(),
        ));
        ctx.publish(ChangeEvent::repositories(
            self.team_id.clone(),
            self.repository_ids.clone(),
        ));
        ctx.publish(ChangeEvent::teams(self.team_id.clone()));
    }
}

/// Deletes `team_id` and everything it owns. Owner only, confirmed by typing
/// the team's name exactly, and never a personal team.
///
/// The refusals are the service's, not only the screen's, so every door
/// applies them.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), team_id = %team_id))]
pub async fn delete_team(
    ctx: &ServiceContext,
    team_id: &str,
    confirm_name: &str,
) -> Result<Purged> {
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;
    require_owner(&mut tx, ctx, team_id, "delete the team").await?;
    let team = team_row(&mut tx, team_id).await?;
    if confirm_name != team.name {
        return Err(Error::invalid(TYPE_THE_TEAM_NAME));
    }
    if team.personal_user_id.is_some() {
        return Err(Error::invalid(PERSONAL_TEAM_GOES_WITH_ITS_ACCOUNT));
    }
    let purged = purge(&mut tx, ctx, team_id).await?;
    tx.commit().await?;

    purged.announce(ctx);
    Ok(purged)
}

/// Deletes `team_id`'s rows on the caller's `BEGIN IMMEDIATE` transaction,
/// in D28's order, for [`delete_team`] and [`delete_account`] alike. Checks
/// nothing about who asked; it refuses only running work.
///
/// 1. **Running work is refused.** An unexpired lease on any of the team's
///    tasks, whoever's runner holds it: a deleted lease cannot fence the
///    runner still working under it. An expired lease does not block, because
///    its task goes with the team and nothing is left to close.
/// 2. **Tokens restricted to this team alone are deleted.** D28 reads "no
///    rows" as every team, so a cascade that removed a restricted token's
///    last row would silently widen it to its user's whole account. A token
///    restricted to several teams loses only this one, by the team's cascade.
/// 3. **Dependency edges, then tasks, then repositories, then the team.** The
///    tasks cascade their runs, links, bundles, findings, acceptances and
///    leases; the team its memberships, settings, invitations, team-wide
///    acceptances, trust rows, pool rows and remaining token rows.
///
/// The span records the team id and the row counts, never the team's name or
/// a plan (ADR-0037 point 6).
pub(crate) async fn purge(
    conn: &mut SqliteConnection,
    ctx: &ServiceContext,
    team_id: &str,
) -> Result<Purged> {
    let span = tracing::info_span!(
        "purge_team",
        team_id = %team_id,
        tasks = tracing::field::Empty,
        repositories = tracing::field::Empty,
        members = tracing::field::Empty,
    );
    purge_rows(conn, ctx, team_id).instrument(span).await
}

async fn purge_rows(
    conn: &mut SqliteConnection,
    ctx: &ServiceContext,
    team_id: &str,
) -> Result<Purged> {
    let now = ctx.clock.now();
    let running = sqlx::query_scalar!(
        r#"SELECT count(*) AS "running!: i64"
             FROM runner_leases l JOIN tasks t ON t.id = l.task_id
            WHERE t.team_id = ?1 AND (l.expires_at IS NULL OR l.expires_at > ?2)"#,
        team_id,
        now,
    )
    .fetch_one(&mut *conn)
    .await?;
    if running > 0 {
        let team = team_row(conn, team_id).await?;
        return Err(Error::invalid(format!(
            "{running} of {}'s tasks are running: wait for them or cancel them first",
            team.name
        )));
    }

    sqlx::query!(
        "DELETE FROM api_tokens
          WHERE id IN (SELECT token_id FROM api_token_teams WHERE team_id = ?1)
            AND NOT EXISTS (SELECT 1 FROM api_token_teams o
                             WHERE o.token_id = api_tokens.id AND o.team_id <> ?1)",
        team_id,
    )
    .execute(&mut *conn)
    .await?;

    let former_members = sqlx::query_scalar!(
        "SELECT user_id FROM team_memberships WHERE team_id = ?1 ORDER BY created_at, user_id",
        team_id,
    )
    .fetch_all(&mut *conn)
    .await?;
    let task_ids = sqlx::query_scalar!(
        "SELECT id FROM tasks WHERE team_id = ?1 ORDER BY created_at, id",
        team_id,
    )
    .fetch_all(&mut *conn)
    .await?;
    let repository_ids = sqlx::query_scalar!(
        "SELECT id FROM repositories WHERE team_id = ?1 ORDER BY created_at, id",
        team_id,
    )
    .fetch_all(&mut *conn)
    .await?;

    sqlx::query!(
        "DELETE FROM task_dependencies
          WHERE task_id IN (SELECT id FROM tasks WHERE team_id = ?1)
             OR depends_on_task_id IN (SELECT id FROM tasks WHERE team_id = ?1)",
        team_id,
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!("DELETE FROM tasks WHERE team_id = ?1", team_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM repositories WHERE team_id = ?1", team_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!("DELETE FROM teams WHERE id = ?1", team_id)
        .execute(&mut *conn)
        .await?;

    let span = tracing::Span::current();
    span.record("tasks", task_ids.len());
    span.record("repositories", repository_ids.len());
    span.record("members", former_members.len());
    Ok(Purged {
        team_id: team_id.to_string(),
        task_ids,
        repository_ids,
        former_members,
    })
}

/// What deleting an account changed: the users whose streams resolve again
/// (the deleted user and everyone else who was in their personal team).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedAccount {
    pub affected_users: Vec<UserId>,
}

/// Deletes the caller's own account, and nothing else: there is no id
/// argument, so no door can name someone else.
///
/// Refused, reading the caller's memberships in this transaction and never
/// through the scope, when the login is not typed exactly, when the caller is
/// the only owner of a team other than their personal team (that team would
/// be left with nobody to administer or delete it), or when one of their
/// runners holds a lease on a task outside their personal team, expired or
/// not: `runner_leases.runner_id` is `RESTRICT`, and an expired lease is not
/// deleted here because its task survives and its run would stay `running`.
/// 053's sweep releases it, after which the deletion goes through.
///
/// Then the personal team goes first, through [`purge`], because
/// `teams.personal_user_id` is `RESTRICT`; then the user, whose sessions,
/// tokens, pairing codes, runners, memberships, settings, acceptances and
/// trust rows cascade, and whose authorship columns become `NULL`, "a former
/// member". The links they made into their other teams are revoked first, as
/// a removal's are. Every other team's tasks that named them are announced,
/// per team.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str()))]
pub async fn delete_account(ctx: &ServiceContext, confirm_login: &str) -> Result<DeletedAccount> {
    let user_id = ctx.actor.as_str();
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;

    let login = sqlx::query_scalar!("SELECT login FROM users WHERE id = ?1", user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| Error::not_found("There is no such user."))?;
    if confirm_login != login {
        return Err(Error::invalid(TYPE_YOUR_LOGIN));
    }

    let sole_owner_of = sqlx::query_scalar!(
        r#"SELECT t.name FROM team_memberships m JOIN teams t ON t.id = m.team_id
            WHERE m.user_id = ?1 AND m.role = 'owner' AND t.personal_user_id IS NULL
              AND (SELECT count(*) FROM team_memberships o
                    WHERE o.team_id = t.id AND o.role = 'owner') = 1
            ORDER BY t.name, t.id"#,
        user_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    if !sole_owner_of.is_empty() {
        return Err(Error::invalid(format!(
            "you are the only owner of {}: make someone else an owner, or delete the team, first",
            sole_owner_of.join(", ")
        )));
    }

    let personal_team =
        sqlx::query_scalar!("SELECT id FROM teams WHERE personal_user_id = ?1", user_id,)
            .fetch_optional(&mut *tx)
            .await?;
    let holding = sqlx::query_scalar!(
        r#"SELECT r.label FROM runner_leases l
             JOIN runners r ON r.id = l.runner_id
             JOIN tasks t ON t.id = l.task_id
            WHERE r.user_id = ?1 AND (?2 IS NULL OR t.team_id <> ?2)
            ORDER BY r.label
            LIMIT 1"#,
        user_id,
        personal_team,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(label) = holding {
        return Err(Error::invalid(format!(
            "your runner {label} is still holding work: wait for it to be released"
        )));
    }

    let member_of = sqlx::query_scalar!(
        "SELECT team_id FROM team_memberships WHERE user_id = ?1 ORDER BY team_id",
        user_id,
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut affected: BTreeSet<UserId> = BTreeSet::from([user_id.to_string()]);
    let purged = match &personal_team {
        Some(team_id) => {
            let purged = purge(&mut tx, ctx, team_id).await?;
            affected.extend(purged.former_members.iter().cloned());
            Some(purged)
        }
        None => None,
    };

    let named = sqlx::query!(
        r#"SELECT team_id, id FROM tasks
            WHERE created_by = ?1 OR assignee_id = ?1 OR assigned_by = ?1
               OR plan_updated_by = ?1 OR review_instructions_updated_by = ?1
               OR strategy_requested_by = ?1
               OR pinned_runner_id IN (SELECT id FROM runners WHERE user_id = ?1)
            ORDER BY team_id, created_at, id"#,
        user_id,
    )
    .fetch_all(&mut *tx)
    .await?;
    // The deletion is a departure from every other team too, so the links
    // the account made there go the way a removal's do (D41 point 6). Its
    // `created_by` becomes `NULL` below, and a person who signs up again is a
    // new user, so nothing but this revocation stops them walking back into a
    // team with a link they kept.
    for team_id in member_of
        .iter()
        .filter(|team_id| Some(*team_id) != personal_team.as_ref())
    {
        invitations::revoke_links_made_by(&mut tx, ctx.clock.as_ref(), team_id, user_id).await?;
    }
    sqlx::query!("DELETE FROM users WHERE id = ?1", user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    if let Some(purged) = &purged {
        purged.announce(ctx);
    }
    for event in per_team(
        named.into_iter().map(|row| (row.team_id, row.id)),
        ChangeEvent::tasks,
    ) {
        ctx.publish(event);
    }
    for team_id in member_of
        .into_iter()
        .filter(|team_id| Some(team_id) != personal_team.as_ref())
    {
        ctx.publish(ChangeEvent::teams(team_id));
    }
    Ok(DeletedAccount {
        affected_users: affected.into_iter().collect(),
    })
}
