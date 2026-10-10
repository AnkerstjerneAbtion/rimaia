//! [`MachineStore`] on `runner.db`: the production implementation, and the
//! only one (task 041, seam-contract D31's 2026-10-10 amendment).
//!
//! Storage and nothing else. What a key means, what a schedule may hold and
//! what is announced after a write are `rimaia-core`'s, in the function that
//! owns each fact; this crate answers what is stored. `machine_store_contract!`
//! holds it to the same behaviour as core's in-memory store, refusals included.
//!
//! The writes are functions over a bare connection as well as trait methods,
//! because adoption writes the same rows inside its own transaction.

use chrono::{DateTime, Utc};
use rimaia_core::board::LeasePurpose;
use rimaia_core::db::{OnArchive, Schedule, ScheduleMode};
use rimaia_core::machine::port::{
    checkout_in_use, duplicate_checkout, duplicate_schedule, unknown_checkout,
};
use rimaia_core::machine::{
    Checkout, CheckoutPatch, HeldLease, MachineFuture, MachineStore, WorktreeRecord,
};
use rimaia_core::Result;
use sqlx::SqliteConnection;

use crate::store::RunnerStore;

/// Inserts a checkout, refusing a second one for its repository.
pub(crate) async fn insert_checkout(
    conn: &mut SqliteConnection,
    checkout: &Checkout,
) -> Result<()> {
    let on_archive = checkout.on_archive.as_str();
    let inserted = sqlx::query!(
        r#"
        INSERT INTO checkouts
            (repository_id, path, worktree_root, max_concurrency, unattended_consent,
             on_archive, on_archive_script, credential_login, credential_label,
             credential_added_at, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        "#,
        checkout.repository_id,
        checkout.path,
        checkout.worktree_root,
        checkout.max_concurrency,
        checkout.unattended_consent,
        on_archive,
        checkout.on_archive_script,
        checkout.credential_login,
        checkout.credential_label,
        checkout.credential_added_at,
        checkout.created_at,
    )
    .execute(&mut *conn)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error) => {
            Err(duplicate_checkout(&checkout.repository_id))
        }
        Err(error) => Err(error.into()),
    }
}

/// Records a task's worktree, replacing an earlier record, and refusing one
/// whose checkout is not on this machine.
pub(crate) async fn record_worktree(
    conn: &mut SqliteConnection,
    record: &WorktreeRecord,
) -> Result<()> {
    let recorded = sqlx::query!(
        r#"
        INSERT INTO worktrees (task_id, repository_id, path, fenced_at)
        VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT (task_id) DO UPDATE
           SET repository_id = excluded.repository_id,
               path = excluded.path,
               fenced_at = excluded.fenced_at
        "#,
        record.task_id,
        record.repository_id,
        record.path,
        record.fenced_at,
    )
    .execute(&mut *conn)
    .await;
    match recorded {
        Ok(_) => Ok(()),
        Err(error) if is_foreign_key_violation(&error) => {
            Err(unknown_checkout(&record.repository_id))
        }
        Err(error) => Err(error.into()),
    }
}

/// Inserts a schedule, refusing a second one with its id.
pub(crate) async fn insert_schedule(
    conn: &mut SqliteConnection,
    schedule: &Schedule,
) -> Result<()> {
    let mode = schedule.mode.as_str();
    let inserted = sqlx::query!(
        r#"
        INSERT INTO schedules
            (id, name, mode, cron, start_at, max_concurrency, enabled, timezone, stop_at,
             last_fired_at, armed_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        "#,
        schedule.id,
        schedule.name,
        mode,
        schedule.cron,
        schedule.start_at,
        schedule.max_concurrency,
        schedule.enabled,
        schedule.timezone,
        schedule.stop_at,
        schedule.last_fired_at,
        schedule.armed_at,
    )
    .execute(&mut *conn)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error) => Err(duplicate_schedule(&schedule.id)),
        Err(error) => Err(error.into()),
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.is_unique_violation())
}

/// A foreign key refused the statement, whichever way SQLite spells it.
///
/// An insert naming a missing parent is `SQLITE_CONSTRAINT_FOREIGNKEY` (787),
/// which sqlx recognises. Deleting a parent an `ON DELETE RESTRICT` child
/// still names is reported as `SQLITE_CONSTRAINT_TRIGGER` (1811), because
/// SQLite enforces `RESTRICT` with an internal trigger, and sqlx does not
/// count that one.
fn is_foreign_key_violation(error: &sqlx::Error) -> bool {
    const SQLITE_CONSTRAINT_TRIGGER: &str = "1811";
    matches!(
        error,
        sqlx::Error::Database(database)
            if database.is_foreign_key_violation()
                || database.code().as_deref() == Some(SQLITE_CONSTRAINT_TRIGGER)
    )
}

/// One checkout, on a connection a caller may be holding a transaction on.
async fn fetch_checkout(
    conn: &mut SqliteConnection,
    repository_id: &str,
) -> Result<Option<Checkout>> {
    let checkout = sqlx::query_as!(
        Checkout,
        r#"
        SELECT repository_id, path, worktree_root, max_concurrency,
               unattended_consent AS "unattended_consent: bool",
               on_archive AS "on_archive: OnArchive", on_archive_script,
               credential_login, credential_label,
               credential_added_at AS "credential_added_at: DateTime<Utc>",
               created_at AS "created_at: DateTime<Utc>"
          FROM checkouts WHERE repository_id = ?1
        "#,
        repository_id,
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(checkout)
}

impl MachineStore for RunnerStore {
    fn get_setting<'a>(&'a self, key: &'a str) -> MachineFuture<'a, Option<String>> {
        Box::pin(async move {
            let value =
                sqlx::query_scalar!("SELECT value FROM runner_settings WHERE key = ?1", key)
                    .fetch_optional(self.pool())
                    .await?;
            Ok(value)
        })
    }

    fn set_setting<'a>(&'a self, key: &'a str, value: &'a str) -> MachineFuture<'a, ()> {
        Box::pin(async move {
            sqlx::query!(
                "INSERT INTO runner_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                key,
                value,
            )
            .execute(self.pool())
            .await?;
            Ok(())
        })
    }

    fn clear_setting<'a>(&'a self, key: &'a str) -> MachineFuture<'a, ()> {
        Box::pin(async move {
            sqlx::query!("DELETE FROM runner_settings WHERE key = ?1", key)
                .execute(self.pool())
                .await?;
            Ok(())
        })
    }

    fn list_checkouts(&self) -> MachineFuture<'_, Vec<Checkout>> {
        Box::pin(async move {
            let checkouts = sqlx::query_as!(
                Checkout,
                r#"
                SELECT repository_id, path, worktree_root, max_concurrency,
                       unattended_consent AS "unattended_consent: bool",
                       on_archive AS "on_archive: OnArchive", on_archive_script,
                       credential_login, credential_label,
                       credential_added_at AS "credential_added_at: DateTime<Utc>",
                       created_at AS "created_at: DateTime<Utc>"
                  FROM checkouts ORDER BY repository_id
                "#
            )
            .fetch_all(self.pool())
            .await?;
            Ok(checkouts)
        })
    }

    fn get_checkout<'a>(&'a self, repository_id: &'a str) -> MachineFuture<'a, Option<Checkout>> {
        Box::pin(async move {
            let mut conn = self.pool().acquire().await?;
            fetch_checkout(&mut conn, repository_id).await
        })
    }

    fn insert_checkout<'a>(&'a self, checkout: &'a Checkout) -> MachineFuture<'a, ()> {
        Box::pin(async move {
            let mut conn = self.pool().acquire().await?;
            insert_checkout(&mut conn, checkout).await
        })
    }

    fn patch_checkout<'a>(
        &'a self,
        repository_id: &'a str,
        patch: &'a CheckoutPatch,
    ) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            // Read, apply, write, in one transaction: the patch's reading of
            // "unset" is core's (`CheckoutPatch::applied_to`), so both stores
            // share it rather than this one spelling it in SQL.
            let mut tx = self.pool().begin().await?;
            let Some(current) = fetch_checkout(&mut tx, repository_id).await? else {
                return Ok(false);
            };

            let next = patch.applied_to(&current);
            let on_archive = next.on_archive.as_str();
            sqlx::query!(
                r#"
                UPDATE checkouts
                   SET path = ?2, worktree_root = ?3, max_concurrency = ?4,
                       unattended_consent = ?5, on_archive = ?6, on_archive_script = ?7,
                       credential_login = ?8, credential_label = ?9, credential_added_at = ?10
                 WHERE repository_id = ?1
                "#,
                repository_id,
                next.path,
                next.worktree_root,
                next.max_concurrency,
                next.unattended_consent,
                on_archive,
                next.on_archive_script,
                next.credential_login,
                next.credential_label,
                next.credential_added_at,
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(true)
        })
    }

    fn remove_checkout<'a>(&'a self, repository_id: &'a str) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            let removed = sqlx::query!(
                "DELETE FROM checkouts WHERE repository_id = ?1",
                repository_id
            )
            .execute(self.pool())
            .await;
            match removed {
                Ok(done) => Ok(done.rows_affected() > 0),
                Err(error) if is_foreign_key_violation(&error) => {
                    Err(checkout_in_use(repository_id))
                }
                Err(error) => Err(error.into()),
            }
        })
    }

    fn get_worktree<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, Option<WorktreeRecord>> {
        Box::pin(async move {
            let record = sqlx::query_as!(
                WorktreeRecord,
                r#"SELECT task_id, repository_id, path,
                          fenced_at AS "fenced_at: DateTime<Utc>"
                     FROM worktrees WHERE task_id = ?1"#,
                task_id,
            )
            .fetch_optional(self.pool())
            .await?;
            Ok(record)
        })
    }

    fn list_worktrees(&self) -> MachineFuture<'_, Vec<WorktreeRecord>> {
        Box::pin(async move {
            let records = sqlx::query_as!(
                WorktreeRecord,
                r#"SELECT task_id, repository_id, path,
                          fenced_at AS "fenced_at: DateTime<Utc>"
                     FROM worktrees ORDER BY task_id"#
            )
            .fetch_all(self.pool())
            .await?;
            Ok(records)
        })
    }

    fn record_worktree<'a>(&'a self, record: &'a WorktreeRecord) -> MachineFuture<'a, ()> {
        Box::pin(async move {
            let mut conn = self.pool().acquire().await?;
            record_worktree(&mut conn, record).await
        })
    }

    fn forget_worktree<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            let forgotten = sqlx::query!("DELETE FROM worktrees WHERE task_id = ?1", task_id)
                .execute(self.pool())
                .await?;
            Ok(forgotten.rows_affected() > 0)
        })
    }

    fn record_held_lease<'a>(&'a self, lease: &'a HeldLease) -> MachineFuture<'a, ()> {
        Box::pin(async move {
            // A replace, not a refusal: one row per task, and a row a missed
            // forget left behind must not refuse the next claim's record.
            sqlx::query!(
                r#"INSERT INTO held_leases
                    (task_id, team_id, purpose, run_id, generation, acquired_at)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                   ON CONFLICT (task_id) DO UPDATE SET
                    team_id = excluded.team_id, purpose = excluded.purpose,
                    run_id = excluded.run_id, generation = excluded.generation,
                    acquired_at = excluded.acquired_at"#,
                lease.task_id,
                lease.team_id,
                lease.purpose,
                lease.run_id,
                lease.generation,
                lease.acquired_at,
            )
            .execute(self.pool())
            .await?;
            Ok(())
        })
    }

    fn set_held_lease_run<'a>(
        &'a self,
        task_id: &'a str,
        run_id: Option<&'a str>,
        purpose: LeasePurpose,
    ) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            let updated = sqlx::query!(
                "UPDATE held_leases SET run_id = ?2, purpose = ?3 WHERE task_id = ?1",
                task_id,
                run_id,
                purpose,
            )
            .execute(self.pool())
            .await?;
            Ok(updated.rows_affected() > 0)
        })
    }

    fn forget_held_lease<'a>(&'a self, task_id: &'a str) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            let forgotten = sqlx::query!("DELETE FROM held_leases WHERE task_id = ?1", task_id)
                .execute(self.pool())
                .await?;
            Ok(forgotten.rows_affected() > 0)
        })
    }

    fn list_held_leases(&self) -> MachineFuture<'_, Vec<HeldLease>> {
        Box::pin(async move {
            let leases = sqlx::query_as!(
                HeldLease,
                r#"SELECT task_id, team_id, purpose AS "purpose: LeasePurpose", run_id,
                          generation, acquired_at AS "acquired_at: DateTime<Utc>"
                     FROM held_leases ORDER BY task_id"#
            )
            .fetch_all(self.pool())
            .await?;
            Ok(leases)
        })
    }

    fn list_schedules(&self) -> MachineFuture<'_, Vec<Schedule>> {
        Box::pin(async move {
            let schedules = sqlx::query_as!(
                Schedule,
                r#"
                SELECT id, name, mode AS "mode: ScheduleMode", cron,
                       start_at AS "start_at: DateTime<Utc>", max_concurrency,
                       enabled AS "enabled: bool", timezone, stop_at,
                       last_fired_at AS "last_fired_at: DateTime<Utc>",
                       armed_at AS "armed_at: DateTime<Utc>"
                  FROM schedules
                 ORDER BY name ASC, id ASC
                "#
            )
            .fetch_all(self.pool())
            .await?;
            Ok(schedules)
        })
    }

    fn get_schedule<'a>(&'a self, id: &'a str) -> MachineFuture<'a, Option<Schedule>> {
        Box::pin(async move {
            let schedule = sqlx::query_as!(
                Schedule,
                r#"
                SELECT id, name, mode AS "mode: ScheduleMode", cron,
                       start_at AS "start_at: DateTime<Utc>", max_concurrency,
                       enabled AS "enabled: bool", timezone, stop_at,
                       last_fired_at AS "last_fired_at: DateTime<Utc>",
                       armed_at AS "armed_at: DateTime<Utc>"
                  FROM schedules WHERE id = ?1
                "#,
                id,
            )
            .fetch_optional(self.pool())
            .await?;
            Ok(schedule)
        })
    }

    fn insert_schedule<'a>(&'a self, schedule: &'a Schedule) -> MachineFuture<'a, ()> {
        Box::pin(async move {
            let mut conn = self.pool().acquire().await?;
            insert_schedule(&mut conn, schedule).await
        })
    }

    fn update_schedule<'a>(&'a self, schedule: &'a Schedule) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            let mode = schedule.mode.as_str();
            let updated = sqlx::query!(
                r#"
                UPDATE schedules
                   SET name = ?2, mode = ?3, cron = ?4, start_at = ?5, max_concurrency = ?6,
                       enabled = ?7, timezone = ?8, stop_at = ?9, armed_at = ?10
                 WHERE id = ?1
                "#,
                schedule.id,
                schedule.name,
                mode,
                schedule.cron,
                schedule.start_at,
                schedule.max_concurrency,
                schedule.enabled,
                schedule.timezone,
                schedule.stop_at,
                schedule.armed_at,
            )
            .execute(self.pool())
            .await?;
            Ok(updated.rows_affected() > 0)
        })
    }

    fn set_schedule_enabled<'a>(
        &'a self,
        id: &'a str,
        enabled: bool,
        armed_at: Option<DateTime<Utc>>,
    ) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            let updated = sqlx::query!(
                "UPDATE schedules SET enabled = ?2, armed_at = ?3 WHERE id = ?1",
                id,
                enabled,
                armed_at,
            )
            .execute(self.pool())
            .await?;
            Ok(updated.rows_affected() > 0)
        })
    }

    fn record_schedule_fire<'a>(
        &'a self,
        id: &'a str,
        fired_at: DateTime<Utc>,
    ) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            let updated = sqlx::query!(
                "UPDATE schedules SET last_fired_at = ?2 WHERE id = ?1",
                id,
                fired_at,
            )
            .execute(self.pool())
            .await?;
            Ok(updated.rows_affected() > 0)
        })
    }

    fn delete_schedule<'a>(&'a self, id: &'a str) -> MachineFuture<'a, bool> {
        Box::pin(async move {
            let deleted = sqlx::query!("DELETE FROM schedules WHERE id = ?1", id)
                .execute(self.pool())
                .await?;
            Ok(deleted.rows_affected() > 0)
        })
    }
}
