//! `runner.db`: the runner's own SQLite file (ADR-0028 point 3).
//!
//! A second file rather than more tables in `rimaia.db`, because the two
//! schemas version independently: a connected runner never has the board's
//! tables, and a headless runner has no board file at all.

use std::path::{Path, PathBuf};

use rimaia_core::db;
use rimaia_core::Result;
use sqlx::migrate::Migrator;
use sqlx::SqlitePool;

/// The runner set, compiled in from `crates/runner/migrations/`.
///
/// Spelled `./migrations` because sqlx 0.8.6's `resolve_path` refuses a bare
/// single-component path as "relative to the current file's directory"; with
/// the `./` it resolves against `CARGO_MANIFEST_DIR`, as `crates/core`'s
/// `../../src-tauri/migrations` does.
///
/// Private: [`RunnerStore::open`] is the only way to get a migrated store, so
/// nothing can hold a `runner.db` pool the runner set has not been applied
/// to. The same argument `rimaia_core::db`'s private migrator makes for the
/// board.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// An open, migrated `runner.db`.
///
/// A newtype and not a bare pool, because the shell holds two pools of the
/// same Rust type, and handing the board's where the runner's is expected must
/// not compile. It keeps the path it was opened from for the messages that
/// have to name the file: adoption's refusal and the shell's startup log line
/// (seam-contract D11).
#[derive(Debug, Clone)]
pub struct RunnerStore {
    pool: SqlitePool,
    path: PathBuf,
}

impl RunnerStore {
    /// Opens (creating if absent) the store at `path` and brings it up to the
    /// runner set's schema.
    ///
    /// Through `db::connect`, so the pragmas are the board's by construction
    /// (WAL, `synchronous = NORMAL`, `foreign_keys`, `busy_timeout`), and
    /// through `db::apply_migrations`, so the runner set is applied the way the
    /// board's is (seam-contract D28 part 1). Idempotent: a second launch
    /// applies nothing. The parent directory must already exist.
    pub async fn open(path: &Path) -> Result<Self> {
        let pool = db::connect(path).await?;
        db::apply_migrations(&MIGRATOR, &pool).await?;
        Ok(Self {
            pool,
            path: path.to_path_buf(),
        })
    }

    /// The file this store was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The store's pool, for this crate's queries and its tests. Every query
    /// through it names a runner table, never a board one (seam-contract D33
    /// point 2).
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D28 part 7's test, extended to the runner set. It lives here, beside
    /// the private migrator, because core's copy cannot reach it, and making
    /// the migrator public to let it would undo "`open` is the only way in".
    ///
    /// sqlx 0.8.6 ignores `-- no-transaction` on SQLite, so a file carrying it
    /// would still run in a transaction today and silently stop the day a
    /// driver honours it. sqlx checks only the start of the file, so a first
    /// line that merely mentions the marker opts the file out: each file's
    /// first line is its title instead.
    #[test]
    fn no_migration_opts_out_of_its_transaction() {
        let embedded: Vec<_> = MIGRATOR.iter().collect();

        assert!(!embedded.is_empty());
        for migration in embedded {
            let name = format!("{} {}", migration.version, migration.description);
            assert!(!migration.no_tx, "{name} opts out of its transaction");
            let title = migration.sql.lines().next().unwrap_or_default();
            assert!(
                title.starts_with("-- ") && !title.contains("no-transaction"),
                "{name} must open with a `--` title that does not name the marker: {title:?}"
            );
        }
    }
}
