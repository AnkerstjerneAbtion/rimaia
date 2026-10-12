//! The one-time copies of this machine's state out of `rimaia.db` (ADR-0028
//! point 5, seam-contract D28 "The runner set").
//!
//! Two steps so far: `settings` (task 040) and `machine_state` (task 041).
//! Each copy is a step with a name. A step runs only while its `adoptions` row
//! is absent, and writes `runner.db` in one transaction that also inserts that
//! row, so a step that fails partway leaves nothing behind and the next launch
//! runs it again. No step writes the board, and none publishes a
//! `ChangeEvent`, because nothing on the board changes.
//!
//! The copies are Rust and not runner migrations, because a migration cannot
//! know where the board file is, and a headless runner has none. They read the
//! board only through `rimaia-core` functions that take the board's
//! [`ServiceContext`]: this crate holds no query that names a board table
//! (seam-contract D33 point 2).

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use rimaia_core::db::settings;
use rimaia_core::identity::SoloIdentity;
use rimaia_core::machine::adoption;
use rimaia_core::{AppPaths, Error, Result, ServiceContext};
use sqlx::SqliteConnection;

use crate::machine;
use crate::store::RunnerStore;

/// What every step is handed: the board to read, the identity the shell
/// already established, where this machine keeps worktrees, and the instant
/// this step stamps.
#[derive(Clone, Copy)]
struct StepInput<'a> {
    board: &'a ServiceContext,
    solo: &'a SoloIdentity,
    worktrees_dir: &'a Path,
    now: DateTime<Utc>,
}

type StepFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// A step reads the board through `input` and writes `runner.db` through
/// `tx`, the transaction [`adopt_board`] commits together with the step's
/// `adoptions` row.
type Step = for<'a> fn(StepInput<'a>, &'a mut SqliteConnection) -> StepFuture<'a>;

/// Every step, in the order a launch runs them. The name is the
/// `adoptions.step` string.
///
/// Data, not branches: task 041 appends `machine_state` and task 054
/// `credential_keys`, each as one more entry. An install that already ran the
/// earlier steps runs only the new one, with no code of its own.
const STEPS: &[(&str, Step)] = &[
    ("settings", settings_step),
    ("machine_state", machine_state_step),
];

/// Adopts whatever this machine has not yet copied out of the board.
///
/// Solo only, once per launch, after the shell has established `solo` with
/// `identity::ensure_solo`. Adoption never calls that itself: it creates rows
/// on a board that has none, and adoption never writes the board. `board`'s
/// clock stamps every row written here, and `paths` says where a checkout
/// whose board row names no worktree root keeps its worktrees.
///
/// Before any step, a store that names another runner is refused, and
/// neither file is written (D28, "A runner store from another board is
/// refused").
pub async fn adopt_board(
    board: &ServiceContext,
    store: &RunnerStore,
    solo: &SoloIdentity,
    paths: &AppPaths,
) -> Result<()> {
    let worktrees_dir = paths.worktrees_dir();
    refuse_another_boards_store(store, solo).await?;

    for (name, step) in STEPS {
        let mut tx = store.pool().begin().await?;
        let adopted = sqlx::query_scalar!("SELECT step FROM adoptions WHERE step = ?1", name)
            .fetch_optional(&mut *tx)
            .await?;
        if adopted.is_some() {
            continue;
        }

        let now = board.clock.now();
        let input = StepInput {
            board,
            solo,
            worktrees_dir: &worktrees_dir,
            now,
        };
        step(input, &mut tx).await?;
        sqlx::query!(
            "INSERT INTO adoptions (step, adopted_at) VALUES (?1, ?2)",
            name,
            now,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::info!(step = name, store = %store.path().display(), "adopted into the runner store");
    }
    Ok(())
}

/// D11's answer to a file that belongs to someone else, applied to the second
/// file. It happens when a user deletes `rimaia.db` to start over and keeps
/// `runner.db`, or copies one file between machines; re-adopting silently
/// would give one store two runners' histories once task 041 puts worktrees
/// in it.
async fn refuse_another_boards_store(store: &RunnerStore, solo: &SoloIdentity) -> Result<()> {
    let recorded = sqlx::query_scalar!("SELECT runner_id FROM runner_identity")
        .fetch_optional(store.pool())
        .await?;
    match recorded {
        Some(recorded) if recorded != solo.runner_id => {
            let path = store.path().display();
            Err(Error::invalid(format!(
                "{path} belongs to runner {recorded}, but this board's runner is {}. Move \
                 {path} aside to start this machine over, or restore the rimaia.db it was \
                 adopted against.",
                solo.runner_id
            )))
        }
        _ => Ok(()),
    }
}

/// The runner-placed settings, byte for byte, and which runner this store is.
///
/// A key the board does not hold stays absent: absent already means "the
/// default" to every accessor.
fn settings_step<'a>(input: StepInput<'a>, tx: &'a mut SqliteConnection) -> StepFuture<'a> {
    Box::pin(async move {
        for row in settings::runner_placed(input.board).await? {
            sqlx::query!(
                "INSERT INTO runner_settings (key, value) VALUES (?1, ?2)",
                row.key,
                row.value,
            )
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query!(
            "INSERT INTO runner_identity (singleton, runner_id, server_url, created_at)
             VALUES (1, ?1, NULL, ?2)",
            input.solo.runner_id,
            input.now,
        )
        .execute(&mut *tx)
        .await?;
        Ok(())
    })
}

/// Each repository's clone as a checkout, every recorded worktree path, and
/// every schedule (D28 "The runner set").
///
/// What maps to what, and the two cases skipped rather than guessed at, are
/// `rimaia-core`'s (`machine::adoption::machine_state`); this writes the
/// result. The rows go through the same statements the store's own writes
/// use, so a value adoption copies is a value the store would have accepted.
fn machine_state_step<'a>(input: StepInput<'a>, tx: &'a mut SqliteConnection) -> StepFuture<'a> {
    Box::pin(async move {
        let state = adoption::machine_state(
            adoption::read_board(input.board).await?,
            input.worktrees_dir,
        )?;
        for checkout in &state.checkouts {
            machine::insert_checkout(&mut *tx, checkout).await?;
        }
        for worktree in &state.worktrees {
            machine::record_worktree(&mut *tx, worktree).await?;
        }
        for schedule in &state.schedules {
            machine::insert_schedule(&mut *tx, schedule).await?;
        }
        Ok(())
    })
}
