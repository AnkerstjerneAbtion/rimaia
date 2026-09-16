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
use crate::runner::provider::ProviderId;

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
    /// Where a provider keeps one task's conversations
    /// (`<data>/providers/<provider>/<task-id>/`, ADR-0026 point 5).
    ///
    /// Created and owned by Rimaia, per provider and per task. A provider that
    /// takes a session id up front ignores it; a provider that resumes "the last
    /// conversation here" is made exact by it, because ADR-0005 already gives
    /// every task a worktree of its own.
    pub fn provider_home(&self, provider: ProviderId, task_id: &str) -> PathBuf {
        self.providers_dir().join(provider.as_str()).join(task_id)
    }

    pub fn providers_dir(&self) -> PathBuf {
        self.data_dir.join("providers")
    }

    /// Per-attempt working space for whatever a provider has to write down.
    ///
    /// Deliberately **not** under [`runs_dir`](Self::runs_dir): that directory is
    /// walked for disk accounting and pruned by task 016, and a scratch file
    /// there would be counted as a transcript.
    pub fn scratch_dir(&self) -> PathBuf {
        self.data_dir.join("scratch")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Called once at startup, before anything tries to write. Idempotent.
    pub fn create_all(&self) -> Result<()> {
        for dir in [
            self.data_dir.clone(),
            self.worktrees_dir(),
            self.runs_dir(),
            self.providers_dir(),
            self.scratch_dir(),
            self.logs_dir(),
        ] {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
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
        assert_eq!(
            paths.providers_dir(),
            Path::new("/tmp/rimaia-test/providers")
        );
        assert_eq!(paths.scratch_dir(), Path::new("/tmp/rimaia-test/scratch"));
        assert_eq!(
            paths.provider_home(ProviderId::ClaudeCode, "task-1"),
            Path::new("/tmp/rimaia-test/providers/claude-code/task-1"),
        );
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
