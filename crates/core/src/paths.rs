//! The layout of the application data directory.
//!
//! Core *derives* paths; it does not discover them. The platform data directory
//! is resolved by the shell through Tauri's path API and handed in, which keeps
//! the OS-specific lookup out of a crate that must not depend on `tauri`
//! (ADR-0015).
//!
//! Nothing Rimaia writes goes inside a user repository — worktrees least of all
//! (ADR-0005), so they cannot be accidentally staged.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The variable that points one launch at a data directory of its own
/// (ADR-0023). Named here so the shell that reads it, the error that refuses a
/// bad value and the doctor row that reports it all spell it the same way.
pub const DATA_DIR_ENV: &str = "RIMAIA_DATA_DIR";

/// Where [`AppPaths`]' directory came from.
///
/// Carried rather than recomputed because by the time anything wants to say it
/// — the doctor's row, a startup log line — the environment has already been
/// read and the answer is not recoverable from the path alone. An override that
/// happens to equal the platform directory is still an override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataDirOrigin {
    /// Tauri's application data directory: what an installed Rimaia uses, and
    /// what ADR-0003 describes.
    Platform,
    /// [`DATA_DIR_ENV`] was set to a usable absolute path.
    Environment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    data_dir: PathBuf,
    origin: DataDirOrigin,
}

impl AppPaths {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            origin: DataDirOrigin::Platform,
        }
    }

    /// Choose between an operator's override and the platform directory
    /// (ADR-0023).
    ///
    /// Pure, and in core rather than in the shell, because every interesting
    /// case here is an edge: the shell reads the environment and asks Tauri for
    /// the fallback, and this decides between them under
    /// `cargo test -p rimaia-core` with no Tauri app in the picture (ADR-0015).
    ///
    /// Three values are refused rather than interpreted, and nothing is created
    /// before the refusal:
    ///
    /// * A **relative** path would mean one directory under `npm run tauri dev`
    ///   and another under a double-clicked bundle, so the symptom would be a
    ///   second empty database rather than a message.
    /// * A path beginning with **`~`** is a shell that did not expand it. The
    ///   alternative to refusing is a literal `~` directory in someone's home
    ///   folder, which is a worse outcome than the mistake being reported.
    /// * An **empty** value is the one deliberate exception: it reads as unset.
    ///   `RIMAIA_DATA_DIR=` is what a half-written shell script exports, an
    ///   empty string can never name a directory, and refusing to start over a
    ///   variable the operator plainly did not set is hostile where falling back
    ///   is not.
    pub fn resolve(override_value: Option<&OsStr>, fallback: PathBuf) -> Result<Self> {
        let Some(value) = override_value.filter(|value| !value.is_empty()) else {
            return Ok(Self::new(fallback));
        };

        let candidate = Path::new(value);
        if candidate.starts_with("~") {
            return Err(Error::invalid(format!(
                "{DATA_DIR_ENV} is {}, which starts with `~`. The shell expands tildes, Rimaia \
                 does not — give the expanded absolute path.",
                candidate.display()
            )));
        }
        if !candidate.is_absolute() {
            return Err(Error::invalid(format!(
                "{DATA_DIR_ENV} is {}, which is relative. It must be an absolute path, so that it \
                 means the same directory however Rimaia was launched.",
                candidate.display()
            )));
        }

        Ok(Self {
            data_dir: candidate.to_path_buf(),
            origin: DataDirOrigin::Environment,
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn origin(&self) -> DataDirOrigin {
        self.origin
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

    /// The platform fallback is what every installed Rimaia uses, so "unset
    /// changes nothing" is the criterion worth a test of its own rather than
    /// something read off the other cases.
    #[test]
    fn no_override_resolves_to_the_platform_directory() {
        let fallback = PathBuf::from("/Users/someone/Library/Application Support/com.rimaia.app");
        let paths = AppPaths::resolve(None, fallback.clone()).expect("no override");
        assert_eq!(paths.data_dir(), fallback);
        assert_eq!(paths.origin(), DataDirOrigin::Platform);
    }

    #[test]
    fn an_absolute_override_replaces_the_platform_directory() {
        let paths = AppPaths::resolve(
            Some(OsStr::new("/tmp/rimaia-scratch")),
            PathBuf::from("/platform"),
        )
        .expect("absolute override");
        assert_eq!(paths.data_dir(), Path::new("/tmp/rimaia-scratch"));
        assert_eq!(paths.origin(), DataDirOrigin::Environment);
        assert_eq!(paths.db_file(), Path::new("/tmp/rimaia-scratch/rimaia.db"));
        assert_eq!(paths.runs_dir(), Path::new("/tmp/rimaia-scratch/runs"));
    }

    #[test]
    fn an_override_containing_spaces_survives_intact() {
        let paths = AppPaths::resolve(
            Some(OsStr::new("/Users/someone/Scratch Dir/rimaia")),
            PathBuf::from("/platform"),
        )
        .expect("override with spaces");
        assert_eq!(
            paths.db_file(),
            Path::new("/Users/someone/Scratch Dir/rimaia/rimaia.db")
        );
    }

    #[test]
    fn a_relative_override_is_refused_naming_the_variable_and_the_value() {
        let error = AppPaths::resolve(Some(OsStr::new("scratch/data")), PathBuf::from("/platform"))
            .expect_err("a relative override must not be interpreted");
        let message = error.to_string();
        assert!(message.contains(DATA_DIR_ENV), "{message}");
        assert!(message.contains("scratch/data"), "{message}");
        assert!(message.contains("relative"), "{message}");
    }

    #[test]
    fn an_unexpanded_tilde_is_refused_as_a_tilde_not_as_a_relative_path() {
        // Both refusals are correct; only one of them tells the operator what
        // actually went wrong, which is that their shell did not expand it.
        let error = AppPaths::resolve(
            Some(OsStr::new("~/Library/rimaia-scratch")),
            PathBuf::from("/platform"),
        )
        .expect_err("an unexpanded tilde must not be interpreted");
        let message = error.to_string();
        assert!(message.contains(DATA_DIR_ENV), "{message}");
        assert!(message.contains('~'), "{message}");
    }

    /// `RIMAIA_DATA_DIR=` is what a half-written shell script exports. Reading
    /// it as unset is a decision, not an accident — see [`AppPaths::resolve`].
    #[test]
    fn an_empty_override_reads_as_unset() {
        let paths = AppPaths::resolve(Some(OsStr::new("")), PathBuf::from("/platform"))
            .expect("an empty value falls back rather than refusing");
        assert_eq!(paths.data_dir(), Path::new("/platform"));
        assert_eq!(paths.origin(), DataDirOrigin::Platform);
    }

    /// Resolving decides a path; it does not make one. The shell calls
    /// [`AppPaths::create_all`] afterwards, and only once the value was
    /// accepted — so a refusal cannot leave a plausible-looking empty data
    /// directory behind for the next launch to find and trust.
    #[test]
    fn resolving_creates_nothing() {
        let temp = tempfile::tempdir().expect("temp dir");
        let scratch = temp.path().join("not-yet");

        let paths = AppPaths::resolve(Some(scratch.as_os_str()), PathBuf::from("/platform"))
            .expect("absolute override");

        assert_eq!(paths.data_dir(), scratch);
        assert!(!scratch.exists());
    }
}
