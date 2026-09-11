//! The layout of the application data directory.
//!
//! Core *derives* paths; it does not discover them. The platform data directory
//! is resolved by the shell through Tauri's path API and handed in, which keeps
//! the OS-specific lookup out of a crate that must not depend on `tauri`
//! (ADR-0015).
//!
//! Nothing Rimaia writes goes inside a user repository — worktrees least of all
//! (ADR-0005), so they cannot be accidentally staged.

use std::path::{Path, PathBuf};

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    data_dir: PathBuf,
}

impl AppPaths {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The single SQLite file (ADR-0003).
    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("rimaia.db")
    }

    /// Root of the per-task worktrees: `<data>/worktrees/<repo-slug>/<task-id>/`
    /// (ADR-0005).
    pub fn worktrees_dir(&self) -> PathBuf {
        self.data_dir.join("worktrees")
    }

    /// Root of the JSONL run transcripts: `<data>/runs/<task-id>/<run-id>.jsonl`
    /// (ADR-0013).
    pub fn runs_dir(&self) -> PathBuf {
        self.data_dir.join("runs")
    }

    /// Rolling application logs — Rimaia's own diagnostics, not run transcripts.
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Called once at startup, before anything tries to write. Idempotent.
    pub fn create_all(&self) -> Result<()> {
        for dir in [
            self.data_dir.clone(),
            self.worktrees_dir(),
            self.runs_dir(),
            self.logs_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
}

/// A canonical path the tools this app shells out to will actually accept.
///
/// `fs::canonicalize` returns a Windows **extended-length** path — the `\\?\`
/// prefix — and git for Windows cannot open one. It reports
/// `could not open '\\?\C:\…\HEAD' for writing: No such file or directory`,
/// which reads as a missing directory rather than as a path it declined to
/// parse, so the failure is both fatal and misleading. Every path this app
/// canonicalizes is eventually handed to `git`, so the prefix is stripped at
/// the point of canonicalization rather than at each call site.
///
/// The prefix buys support for paths longer than 260 characters. Nothing here
/// needs it: a repository the user picked in a folder dialog and a worktree
/// under the app data directory are both far short of that, and a path that
/// genuinely needed it would be one git could not open anyway.
///
/// A no-op on every other platform. Found by task 022's CI matrix, which is the
/// first time this project's tests ran on Windows at all.
pub fn git_safe(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        const EXTENDED_LENGTH_PREFIX: &str = r"\\?\";
        let text = path.to_string_lossy();
        if let Some(stripped) = text.strip_prefix(EXTENDED_LENGTH_PREFIX) {
            return PathBuf::from(stripped);
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_is_derived_from_the_data_dir() {
        let paths = AppPaths::new("/tmp/rimaia-test");
        assert_eq!(paths.db_file(), Path::new("/tmp/rimaia-test/rimaia.db"));
        assert_eq!(
            paths.worktrees_dir(),
            Path::new("/tmp/rimaia-test/worktrees")
        );
        assert_eq!(paths.runs_dir(), Path::new("/tmp/rimaia-test/runs"));
        assert_eq!(paths.logs_dir(), Path::new("/tmp/rimaia-test/logs"));
    }

    #[test]
    fn a_data_dir_containing_spaces_survives_intact() {
        // macOS puts this under "Application Support". Paths are joined, never
        // formatted into a string, for the same reason commands are argument
        // vectors and never `sh -c`.
        let paths = AppPaths::new("/Users/someone/Library/Application Support/com.rimaia.app");
        assert_eq!(
            paths.db_file(),
            Path::new("/Users/someone/Library/Application Support/com.rimaia.app/rimaia.db")
        );
    }

    #[test]
    fn create_all_is_idempotent() {
        let temp = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::new(temp.path().join("com.rimaia.app"));
        paths.create_all().expect("first create");
        paths.create_all().expect("second create must not fail");
        assert!(paths.logs_dir().is_dir());
    }
}
