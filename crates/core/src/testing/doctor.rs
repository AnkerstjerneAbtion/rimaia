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

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::doctor::Environment;
use crate::paths::AppPaths;
use crate::runner::process::RunnerConfig;

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

/// A stand-in `claude` that answers the two questions the doctor asks it, and
/// nothing else.
///
/// **`claude` is a prerequisite, not a dependency (ADR-0004): it is installed on
/// a developer's machine and absent from a CI runner.** Since task 018,
/// [`QueueHandle::start`](crate::scheduler::QueueHandle::start) runs the doctor,
/// which spawns that binary — so a test that starts a queue against
/// `RunnerConfig::default()`, whose `program` is a bare `claude` resolved on
/// `PATH`, passes locally and fails on CI. That is not a flake; it is the test
/// depending on something the project deliberately does not ship.
///
/// The answer is to *satisfy* the gate rather than switch it off, so what the
/// test proves stays the same: this writes a two-line script that reports a
/// version at the pinned minimum and a signed-in status.
///
/// `crates/core/tests/scheduler.rs` has a richer stand-in of its own — it also
/// replays recorded run fixtures, which is a job this one deliberately does not
/// do. Use that one for a queue that actually runs something; use this one when
/// all a test needs is to get past the preflight.
#[cfg(unix)]
pub fn standin_claude(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let program = dir.join("claude-standin");
    std::fs::write(
        &program,
        "#!/bin/sh\n\
         if [ \"$1\" = '--version' ]; then echo '2.1.234 (Claude Code)'; exit 0; fi\n\
         if [ \"$1\" = 'auth' ]; then echo '{\"loggedIn\": true}'; exit 0; fi\n\
         exit 0\n",
    )
    .expect("write the stand-in claude");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in claude executable");
    program
}

/// Everything `QueueHandle::start`'s preflight needs to pass without an
/// installed CLI: a real, writable app directory and a runner pointing at
/// [`standin_claude`].
///
/// The [`TempDir`] comes back so the caller can hold it — dropping it removes
/// both the directory the `data_directory` check reports on and the stand-in
/// the `claude` checks spawn.
#[cfg(unix)]
pub fn passing_queue_environment() -> (TempDir, AppPaths, RunnerConfig) {
    let root = TempDir::new().expect("a temporary directory for the queue");
    let paths = AppPaths::new(PathBuf::from(root.path()));
    paths.create_all().expect("the app directories");
    let runner = RunnerConfig {
        program: standin_claude(root.path()),
        ..RunnerConfig::default()
    };
    (root, paths, runner)
}

/// A planner access for a test that constructs an MCP server and never plans
/// anything.
///
/// [`RimaiaServer`](crate::mcp::server::RimaiaServer) takes a
/// [`PlannerAccess`](crate::runner::strategy::PlannerAccess) since task 023, for
/// the same reason it takes an [`Environment`]: `plan_task_strategy` and
/// `plan_tasks_strategy` spawn a real planner, and neither the data directory
/// nor the shared in-flight registry can be guessed from inside the server.
/// Almost no MCP test cares what is in it.
///
/// **A test that actually plans must not use this** — the `claude` here is a
/// bare name resolved on `PATH`, which is a prerequisite CI does not have
/// (ADR-0004). Build one from [`passing_queue_environment`] instead.
pub fn planner_access() -> crate::runner::strategy::PlannerAccess {
    crate::runner::strategy::PlannerAccess {
        paths: AppPaths::new(std::env::temp_dir().join("rimaia-placeholder-not-created")),
        runner: RunnerConfig::default(),
        in_flight: crate::scheduler::InFlight::new(),
    }
}
