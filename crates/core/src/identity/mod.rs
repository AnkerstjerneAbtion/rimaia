//! Who owns the board: teams, the people in them, and the solo installation's
//! own identity (ADR-0029, ADR-0030, seam-contract D28 part 3).
//!
//! Solo is not a separate code path. It is one team, one user and one runner
//! with generated ids, recorded in `solo_identity`, and every service filters
//! by that team exactly as a server filters by a signed-in caller's (ADR-0029
//! point 2). Those rows come from one of two places:
//!
//! - **Adopted by migration**, when `20261003120000_team_mode_board.sql` finds
//!   a board with something a team must own. It has to be SQL, because the
//!   rebuild's `NOT NULL team_id` copy needs the team inside its transaction.
//! - **Created here**, by [`ensure_solo`], on a board that had nothing to
//!   adopt. It must not be SQL, because the same files build every server's
//!   board, and a server starts with no team at all.
//!
//! Both write the same placeholder values, and nothing in solo displays them.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::{SqliteConnection, SqlitePool};

use crate::clock::Clock;
use crate::db::new_id;
use crate::db::settings::{BASE_INSTRUCTIONS, DEFAULT_BASE_INSTRUCTIONS};
use crate::error::{Error, Result};
use crate::events::{RunnerId, TeamId, UserId};
use crate::runner::provider::ProviderId;

/// The solo user's login. A placeholder: solo shows no sign-in (ADR-0030
/// point 7), and task 038's adoption writes the same string.
pub const SOLO_LOGIN: &str = "solo";

/// The name every personal team is created with (ADR-0029 point 2).
pub const PERSONAL_TEAM_NAME: &str = "Personal";

/// The solo runner's label: the one machine a solo board has ever run on.
pub const SOLO_RUNNER_LABEL: &str = "This computer";

/// The installation's own team, user and runner, as `solo_identity` records
/// them. Read once at startup; the shell builds its context from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoloIdentity {
    pub team_id: TeamId,
    pub user_id: UserId,
    pub runner_id: RunnerId,
    pub created_at: DateTime<Utc>,
}

/// What a person may do in a team (ADR-0029 point 3), in the spelling of
/// `team_memberships.role`'s `CHECK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Owner,
    Member,
}

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Member => "member",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = Error;

    /// Strict, unlike a settings value: the column's `CHECK` admits only
    /// these two, so anything else did not come from the store.
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "owner" => Ok(Role::Owner),
            "member" => Ok(Role::Member),
            other => Err(Error::invalid(format!(
                "`{other}` is not a team role: a role is `owner` or `member`"
            ))),
        }
    }
}

/// The user and team [`create_personal_team`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalTeam {
    pub user_id: UserId,
    pub team_id: TeamId,
}

/// One user, their personal team, an owner membership, and the team's base
/// instructions seeded from [`DEFAULT_BASE_INSTRUCTIONS`].
///
/// This is the team-creation service D28 part 3 names: [`ensure_solo`] calls
/// it for the solo user, and a server's sign-up will (task 047). It writes
/// inside the caller's transaction rather than opening one, because each
/// caller adds rows of its own to the same one: `ensure_solo` a runner and the
/// `solo_identity` row, sign-up the user's identity.
pub async fn create_personal_team(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    login: &str,
) -> Result<PersonalTeam> {
    let user_id = new_id();
    let team_id = new_id();
    let now = clock.now();

    sqlx::query!(
        "INSERT INTO users (id, login, created_at) VALUES (?1, ?2, ?3)",
        user_id,
        login,
        now,
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO teams (id, name, personal_user_id, created_at) VALUES (?1, ?2, ?3, ?4)",
        team_id,
        PERSONAL_TEAM_NAME,
        user_id,
        now,
    )
    .execute(&mut *conn)
    .await?;
    let owner = Role::Owner.as_str();
    sqlx::query!(
        "INSERT INTO team_memberships (team_id, user_id, role, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        team_id,
        user_id,
        owner,
        now,
    )
    .execute(&mut *conn)
    .await?;
    // The seed the board's own `settings` row got from
    // 20260820120100_seed_settings.sql, for the same reason: a new team's
    // first run composes with the default instructions, and clearing them is
    // the user's choice to make.
    sqlx::query!(
        "INSERT INTO team_settings (team_id, key, value) VALUES (?1, ?2, ?3)",
        team_id,
        BASE_INSTRUCTIONS,
        DEFAULT_BASE_INSTRUCTIONS,
    )
    .execute(&mut *conn)
    .await?;

    Ok(PersonalTeam { user_id, team_id })
}

/// The installation's solo identity: the one already recorded, or a new one.
///
/// D28 part 3's three cases:
///
/// - a `solo_identity` row exists, whether the migration adopted it or an
///   earlier launch created it: it is returned and nothing is written;
/// - there is none and `teams` is empty: the board had nothing to adopt, so
///   the five rows are created here, in one transaction;
/// - there is none but `teams` is not empty: the file belongs to a server, and
///   solo refuses it rather than inventing a second owner beside the real ones.
///
/// Takes a pool rather than a [`ServiceContext`](crate::ServiceContext),
/// because it runs before any context can exist: the context's scope is what
/// it returns.
pub async fn ensure_solo(pool: &SqlitePool, clock: &dyn Clock) -> Result<SoloIdentity> {
    // `IMMEDIATE`, so two processes opening one file cannot both see no row
    // and both create one.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;

    if let Some(recorded) = read_solo_identity(&mut tx).await? {
        tx.commit().await?;
        return Ok(recorded);
    }

    let teams: i64 = sqlx::query_scalar!("SELECT count(*) FROM teams")
        .fetch_one(&mut *tx)
        .await?;
    if teams > 0 {
        return Err(Error::invalid(
            "this database belongs to a Rimaia server: it has teams but no solo identity, \
             so it cannot be opened as a solo board",
        ));
    }

    let personal = create_personal_team(&mut tx, clock, SOLO_LOGIN).await?;
    let runner_id = insert_runner(&mut tx, clock, &personal.user_id, SOLO_RUNNER_LABEL).await?;
    let created_at = clock.now();
    sqlx::query!(
        "INSERT INTO solo_identity (singleton, team_id, user_id, runner_id, created_at)
         VALUES (1, ?1, ?2, ?3, ?4)",
        personal.team_id,
        personal.user_id,
        runner_id,
        created_at,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(SoloIdentity {
        team_id: personal.team_id,
        user_id: personal.user_id,
        runner_id,
        created_at,
    })
}

/// A `runners` row for `user_id`, running the default provider.
///
/// Private: [`ensure_solo`] is the only thing that creates a runner. Pairing
/// one is tasks 047 and 052's; the test harness has a fixture of its own.
async fn insert_runner(
    conn: &mut SqliteConnection,
    clock: &dyn Clock,
    user_id: &str,
    label: &str,
) -> Result<RunnerId> {
    let runner_id = new_id();
    let provider = ProviderId::ClaudeCode.as_str();
    let paired_at = clock.now();
    sqlx::query!(
        "INSERT INTO runners (id, user_id, label, provider, paired_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        runner_id,
        user_id,
        label,
        provider,
        paired_at,
    )
    .execute(&mut *conn)
    .await?;
    Ok(runner_id)
}

async fn read_solo_identity(conn: &mut SqliteConnection) -> Result<Option<SoloIdentity>> {
    let recorded = sqlx::query_as!(
        SoloIdentity,
        r#"SELECT team_id, user_id, runner_id, created_at AS "created_at: DateTime<Utc>"
             FROM solo_identity
            WHERE singleton = 1"#
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(recorded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_epoch, test_pool, TestClock};
    use pretty_assertions::assert_eq;

    #[test]
    fn a_role_round_trips_through_the_spelling_the_column_checks() {
        for role in [Role::Owner, Role::Member] {
            assert_eq!(role.as_str().parse::<Role>().expect("parse"), role);
        }
        assert_eq!(
            "admin".parse::<Role>().expect_err("not a role").code(),
            crate::ErrorCode::Invalid
        );
    }

    #[tokio::test]
    async fn a_second_launch_finds_the_same_solo_identity_and_creates_nothing() {
        let pool = test_pool().await;
        let clock = TestClock::new(test_epoch());

        let first = ensure_solo(&pool, &clock).await.expect("first launch");
        clock.advance(chrono::Duration::days(1));
        let second = ensure_solo(&pool, &clock).await.expect("second launch");

        assert_eq!(second, first);
        let counts: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM users), (SELECT count(*) FROM teams),
                    (SELECT count(*) FROM runners), (SELECT count(*) FROM team_memberships)",
        )
        .fetch_one(&pool)
        .await
        .expect("count the rows");
        assert_eq!(counts, (1, 1, 1, 1));
    }

    #[tokio::test]
    async fn a_board_that_belongs_to_a_server_refuses_to_open_as_solo() {
        // The state `ensure_solo` never produces on its own: a team with no
        // solo identity, which is what every server's board looks like.
        let pool = test_pool().await;
        let clock = TestClock::new(test_epoch());
        sqlx::query(
            "INSERT INTO teams (id, name, created_at)
             VALUES ('3f2b1c00-0000-4000-8000-00000000aaaa', 'Acme', '2026-08-20T02:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .expect("a server's team");

        let error = ensure_solo(&pool, &clock)
            .await
            .expect_err("a server's board is not a solo board");

        assert_eq!(error.code(), crate::ErrorCode::Invalid);
        let written: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM users), (SELECT count(*) FROM teams),
                    (SELECT count(*) FROM runners), (SELECT count(*) FROM solo_identity),
                    (SELECT count(*) FROM team_settings)",
        )
        .fetch_one(&pool)
        .await
        .expect("count the rows");
        assert_eq!(written, (0, 1, 0, 0, 0), "the refusal wrote nothing");
    }

    #[tokio::test]
    async fn a_personal_team_has_its_owner_as_its_only_member() {
        let pool = test_pool().await;
        let clock = TestClock::new(test_epoch());
        let mut conn = pool.acquire().await.expect("a connection");

        let personal = create_personal_team(&mut conn, &clock, "ada")
            .await
            .expect("create the team");

        let members: Vec<(String, String)> =
            sqlx::query_as("SELECT user_id, role FROM team_memberships WHERE team_id = ?1")
                .bind(&personal.team_id)
                .fetch_all(&mut *conn)
                .await
                .expect("read the memberships");
        assert_eq!(
            members,
            vec![(personal.user_id.clone(), "owner".to_string())]
        );
        let seeded: String = sqlx::query_scalar(
            "SELECT value FROM team_settings WHERE team_id = ?1 AND key = 'base_instructions'",
        )
        .bind(&personal.team_id)
        .fetch_one(&mut *conn)
        .await
        .expect("the seeded instructions");
        assert_eq!(seeded, DEFAULT_BASE_INSTRUCTIONS);
    }
}
