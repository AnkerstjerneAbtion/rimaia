//! Forming, joining, leaving and deleting teams (task 051, ADR-0029 points 3,
//! 4 and 6, ADR-0037 point 6).
//!
//! # Roles are read in the write's own transaction
//!
//! [`require_owner`] is the one role check. It reads `team_memberships` for
//! `ctx.actor` inside the transaction that then writes, never `Caller.teams`,
//! which was read when the request began (D32 point 7): a demotion committed
//! in between must win. Every transaction that calls it, or applies the
//! last-owner rule, is `BEGIN IMMEDIATE`, for the reason 043 gives for its
//! lease transactions: under WAL a deferred transaction that reads and then
//! writes can fail with `SQLITE_BUSY_SNAPSHOT`, which would reach the person
//! as a `database` error instead of the sentence.
//!
//! The checks run in a fixed order. Scope first, so a non-member gets the
//! answer a never-issued team id gets (`not_found`, ADR-0029 point 5). Role
//! second, so a member gets `invalid` naming what only an owner can do.
//!
//! # A team always has an owner
//!
//! Changing a role, removing a member, leaving and deleting an account each
//! write first, then count the team's owners in the same transaction and roll
//! back when there are none. Writing first means two owners demoting each
//! other in concurrent requests cannot both succeed, whatever order SQLite
//! serializes them in.
//!
//! # A solo board has one team
//!
//! Every write here, and the invitation preview, is refused on a board with a
//! `solo_identity` row (ADR-0029 point 2, ADR-0030 point 7): a second team
//! would fall outside the shell's one-team scope and disappear.

use sqlx::SqliteConnection;

use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::identity::teams::{listing_of, TeamListing};
use crate::identity::{insert_team, Role};

pub mod copy;
pub mod delete;
pub mod invitations;
pub mod members;

/// A team as the person who just created or joined it lists it: the
/// switcher's entry, so it can select the team without a second read.
pub type Team = TeamListing;

/// What every team command a solo board refuses answers (task 051 Scope 10).
pub const SOLO_HAS_ONE_TEAM: &str = "a solo board has one team: teams need a Rimaia server";

/// What a team id outside the caller's scope is answered with, whether the
/// team exists or not: the sentence task 050's `Rimaia-Team` gives a missing
/// team.
pub const NO_SUCH_TEAM: &str = "There is no team with that id.";

/// The refusal a member gets for an owner-only action. `action` is a phrase
/// fixed per call site, such as `invite people`.
pub fn only_an_owner_can(action: &str) -> String {
    format!("only an owner of this team can {action}")
}

/// The last-owner rule's refusal.
pub fn needs_an_owner(team_name: &str) -> String {
    format!("{team_name} needs at least one owner: make someone else an owner first")
}

/// The refusal for removing, demoting or the leaving of a personal team's
/// own person.
pub const PERSONAL_TEAM_KEEPS_ITS_PERSON: &str =
    "a personal team always keeps the person it belongs to as an owner";

/// A team name: trimmed, 1 to 100 characters after trimming, as 047's labels
/// are. Not unique: refusing a taken name would tell the caller that some team
/// they cannot see already has it.
pub fn team_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if (1..=100).contains(&trimmed.chars().count()) {
        Ok(trimmed.to_string())
    } else {
        Err(Error::invalid("A team name is 1 to 100 characters."))
    }
}

/// Creates a team people share, with `ctx.actor` as its only owner, and its
/// base instructions seeded from the defaults by the helper a personal team
/// is made with.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str()))]
pub async fn create_team(ctx: &ServiceContext, name: &str) -> Result<Team> {
    let name = team_name(name)?;
    let mut tx = ctx.begin_immediate().await?;
    refuse_solo(&mut tx).await?;
    let team_id = insert_team(&mut tx, ctx.clock.as_ref(), &name, None, &ctx.actor).await?;
    let team = listing_of(&mut tx, &ctx.actor, &team_id).await?;
    tx.commit().await?;

    ctx.publish(ChangeEvent::teams(team_id));
    Ok(team)
}

/// Refuses `action` unless `ctx.actor` owns `team_id`, read on `conn`, which
/// must be the `BEGIN IMMEDIATE` transaction that then writes.
///
/// A team outside the scope, or one the actor no longer belongs to, is
/// [`NO_SUCH_TEAM`]; a member is [`only_an_owner_can`]`(action)`.
pub(crate) async fn require_owner(
    conn: &mut SqliteConnection,
    ctx: &ServiceContext,
    team_id: &str,
    action: &str,
) -> Result<()> {
    if !ctx.scope.contains(team_id) {
        return Err(Error::not_found(NO_SUCH_TEAM));
    }
    match role_of(conn, team_id, &ctx.actor).await? {
        Some(Role::Owner) => Ok(()),
        Some(Role::Member) => Err(Error::invalid(only_an_owner_can(action))),
        None => Err(Error::not_found(NO_SUCH_TEAM)),
    }
}

/// `user_id`'s role in `team_id`, or `None` when they are not a member.
async fn role_of(
    conn: &mut SqliteConnection,
    team_id: &str,
    user_id: &str,
) -> Result<Option<Role>> {
    sqlx::query_scalar!(
        "SELECT role FROM team_memberships WHERE team_id = ?1 AND user_id = ?2",
        team_id,
        user_id,
    )
    .fetch_optional(&mut *conn)
    .await?
    .map(|role| role.parse())
    .transpose()
}

/// [`SOLO_HAS_ONE_TEAM`] on a board with a `solo_identity` row, D28's own
/// discriminator between a solo board and a server's.
async fn refuse_solo(conn: &mut SqliteConnection) -> Result<()> {
    let solo =
        sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM solo_identity) AS "solo!: bool""#)
            .fetch_one(&mut *conn)
            .await?;
    if solo {
        return Err(Error::invalid(SOLO_HAS_ONE_TEAM));
    }
    Ok(())
}

/// The facts about a team the rules below read.
struct TeamRow {
    name: String,
    personal_user_id: Option<String>,
}

async fn team_row(conn: &mut SqliteConnection, team_id: &str) -> Result<TeamRow> {
    sqlx::query_as!(
        TeamRow,
        "SELECT name, personal_user_id FROM teams WHERE id = ?1",
        team_id,
    )
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| Error::not_found(NO_SUCH_TEAM))
}

/// The last-owner rule, after the write: [`needs_an_owner`] when `team_id` has
/// no owner left. The caller's transaction rolls back when it drops.
async fn ensure_an_owner_remains(conn: &mut SqliteConnection, team_id: &str) -> Result<()> {
    let owners = sqlx::query_scalar!(
        r#"SELECT count(*) AS "owners!: i64" FROM team_memberships
            WHERE team_id = ?1 AND role = 'owner'"#,
        team_id,
    )
    .fetch_one(&mut *conn)
    .await?;
    if owners == 0 {
        let team = team_row(conn, team_id).await?;
        return Err(Error::invalid(needs_an_owner(&team.name)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_team_name_is_trimmed_and_held_to_one_to_a_hundred_characters() {
        assert_eq!(team_name("  Acme  ").expect("valid"), "Acme");
        assert_eq!(
            team_name(&"é".repeat(100))
                .expect("100 characters")
                .chars()
                .count(),
            100
        );
        for refused in ["", "   ", &"x".repeat(101)] {
            assert_eq!(
                team_name(refused).expect_err("out of range").to_string(),
                "A team name is 1 to 100 characters."
            );
        }
    }

    #[test]
    fn every_refusal_reads_exactly() {
        assert_eq!(
            only_an_owner_can("invite people"),
            "only an owner of this team can invite people"
        );
        assert_eq!(
            needs_an_owner("Acme"),
            "Acme needs at least one owner: make someone else an owner first"
        );
    }
}
