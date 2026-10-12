//! What the runner's `machine_state` adoption step copies out of the board
//! (ADR-0028 point 5, seam-contract D28 "The runner set", task 041).
//!
//! Two halves, and only the first touches the board. [`read_board`] is the one
//! board query the step makes; [`machine_state`] is the pure mapping from
//! those rows to the machine store's, with the two cases that have no clone to
//! map skipped rather than guessed at. `rimaia-runner` writes the result into
//! `runner.db` in one transaction with its `adoptions` row, and decides
//! nothing about it.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::context::ServiceContext;
use crate::db::{OnArchive, Schedule, ScheduleMode};
use crate::error::Result;
use crate::repo;

use super::types::{Checkout, WorktreeRecord};

/// One board repository's per-machine columns, as the board still holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardRepository {
    pub id: String,
    pub name: String,
    /// `None` for a repository this board has no clone path for, which has no
    /// checkout to map.
    pub path: Option<String>,
    pub worktree_root: Option<String>,
    pub max_concurrency: i64,
    pub allow_unattended_runs: bool,
    pub on_archive: OnArchive,
    pub on_archive_script: Option<String>,
    pub credential_login: Option<String>,
    pub credential_label: Option<String>,
    pub credential_added_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// One task's worktree path, as `tasks.worktree_path` still holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardWorktree {
    pub task_id: String,
    pub repository_id: String,
    pub path: String,
}

/// Everything the step reads off the board.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardMachineState {
    pub repositories: Vec<BoardRepository>,
    pub worktrees: Vec<BoardWorktree>,
    pub schedules: Vec<Schedule>,
}

/// What the step writes into the machine store.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineState {
    pub checkouts: Vec<Checkout>,
    pub worktrees: Vec<WorktreeRecord>,
    pub schedules: Vec<Schedule>,
}

/// The board's copy of this machine's state: every repository's per-machine
/// columns, every task's worktree path, and every schedule.
///
/// Like `db::settings::runner_placed`, it takes the board context and ignores
/// its scope: machine state belongs to no team, and adoption copies all of it.
/// Task 065 deletes this function in the change that drops the columns it
/// names; once task 066 lands it is the only board query that still names a
/// retired column.
pub async fn read_board(ctx: &ServiceContext) -> Result<BoardMachineState> {
    let repositories = sqlx::query_as!(
        BoardRepository,
        r#"
        -- machine_state adoption (task 041; task 065 deletes this read)
        SELECT id, name, path, worktree_root, max_concurrency,
               allow_unattended_runs AS "allow_unattended_runs: bool",
               on_archive AS "on_archive: OnArchive", on_archive_script,
               credential_login, credential_label,
               credential_added_at AS "credential_added_at: DateTime<Utc>",
               created_at AS "created_at: DateTime<Utc>"
          FROM repositories
         ORDER BY id
        "#
    )
    .fetch_all(&ctx.pool)
    .await?;

    let worktrees = sqlx::query_as!(
        BoardWorktree,
        r#"
        -- machine_state adoption (task 041; task 065 deletes this read)
        SELECT id AS task_id, repository_id, worktree_path AS "path!"
          FROM tasks
         WHERE worktree_path IS NOT NULL
         ORDER BY id
        "#
    )
    .fetch_all(&ctx.pool)
    .await?;

    let schedules = sqlx::query_as!(
        Schedule,
        r#"
        -- machine_state adoption (task 041; task 065 deletes this read)
        SELECT id, name, mode AS "mode: ScheduleMode", cron,
               start_at AS "start_at: DateTime<Utc>", max_concurrency,
               enabled AS "enabled: bool", timezone, stop_at,
               last_fired_at AS "last_fired_at: DateTime<Utc>",
               armed_at AS "armed_at: DateTime<Utc>"
          FROM schedules
         ORDER BY id
        "#
    )
    .fetch_all(&ctx.pool)
    .await?;

    Ok(BoardMachineState {
        repositories,
        worktrees,
        schedules,
    })
}

/// The machine store's rows for `board`, column for column.
///
/// Two cases have no clone to map, and both are skipped rather than guessed
/// at: a repository whose `path` is `NULL`, and a worktree whose repository
/// was skipped. A repository with a path and no `worktree_root` gets the root
/// `repo::register` would have derived for it, under `worktrees_dir`.
pub fn machine_state(board: BoardMachineState, worktrees_dir: &Path) -> Result<MachineState> {
    let mut checkouts = Vec::with_capacity(board.repositories.len());
    for repository in board.repositories {
        let Some(path) = repository.path else {
            tracing::info!(
                repository_id = %repository.id,
                "the board holds no clone path for this repository; adopting no checkout for it",
            );
            continue;
        };
        let worktree_root = match repository.worktree_root {
            Some(root) => root,
            None => repo::default_worktree_root(worktrees_dir, &repository.name)?,
        };
        checkouts.push(Checkout {
            repository_id: repository.id,
            path,
            worktree_root,
            max_concurrency: repository.max_concurrency,
            unattended_consent: repository.allow_unattended_runs,
            on_archive: repository.on_archive,
            on_archive_script: repository.on_archive_script,
            credential_login: repository.credential_login,
            credential_label: repository.credential_label,
            credential_added_at: repository.credential_added_at,
            created_at: repository.created_at,
        });
    }

    let mapped: HashSet<&str> = checkouts
        .iter()
        .map(|checkout| checkout.repository_id.as_str())
        .collect();
    let worktrees = board
        .worktrees
        .into_iter()
        .filter(|worktree| mapped.contains(worktree.repository_id.as_str()))
        .map(|worktree| WorktreeRecord {
            task_id: worktree.task_id,
            repository_id: worktree.repository_id,
            path: worktree.path,
            fenced_at: None,
        })
        .collect();

    Ok(MachineState {
        checkouts,
        worktrees,
        schedules: board.schedules,
    })
}
