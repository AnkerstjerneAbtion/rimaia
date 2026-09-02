//! A doctor environment for tests that have to construct one but are not
//! testing the doctor.
//!
//! [`RimaiaServer`](crate::mcp::server::RimaiaServer) takes a
//! [`doctor::Environment`] because `run_doctor` needs to know where state lives
//! and which `claude` the runner would spawn — neither of which it could guess
//! without lying. Every MCP test therefore has to supply one, and almost none of
//! them care what is in it.
//!
//! **A test that actually runs the doctor must not use this.** Point it at its
//! own [`TempDir`](tempfile::TempDir) instead: [`environment`] deliberately
//! names a directory it does not create, so a check that reports on it would be
//! reporting on nothing, and the disk-space and writability rows would be about
//! a path the test never chose.

use std::path::PathBuf;

use tempfile::TempDir;

use crate::doctor::Environment;
use crate::paths::AppPaths;

/// A placeholder environment, for a test that constructs a server and never
/// asks it for a report.
pub fn environment() -> Environment {
    Environment::new(AppPaths::new(
        std::env::temp_dir().join("rimaia-placeholder-not-created"),
    ))
}

/// A real environment, rooted at a temporary directory that exists and is
/// writable — for a test that *does* run the doctor.
///
/// The [`TempDir`] comes back with it because dropping it removes the
/// directory, and a report about a directory that vanished mid-check is not a
/// scenario anyone is trying to test.
pub fn temp_environment() -> (TempDir, Environment) {
    let root = TempDir::new().expect("a temporary directory for the doctor");
    let paths = AppPaths::new(PathBuf::from(root.path()));
    paths.create_all().expect("the app directories");
    let environment = Environment::new(paths);
    (root, environment)
}
