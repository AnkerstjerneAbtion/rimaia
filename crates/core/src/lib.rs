//! Rimaia orchestration logic.
//!
//! Everything that decides something lives here: prompt composition, outcome
//! classification, retry policy, ordering, dependency resolution, git and
//! process supervision. The Tauri shell in `src-tauri/` and the MCP server in
//! [`mcp`] are both thin adapters over these services, so a rule enforced in
//! only one of them is a bug (ADR-0006).
//!
//! This crate must not depend on `tauri` (ADR-0015). That is what lets
//! `cargo test -p rimaia-core` run with no WebKit or GTK installed, and what
//! stops business rules from drifting into a layer the MCP server cannot reach.

pub mod clock;
pub mod context;
pub mod db;
pub mod error;
pub mod events;
pub mod mcp;
pub mod paths;
pub mod repo;
pub mod runner;
pub mod scheduler;
pub mod startup;
pub mod tasks;
pub mod worktree;

/// Test scaffolding shared by this crate's unit tests, its `tests/` integration
/// tests, and the shell's. Behind a feature so a release build never links it.
#[cfg(feature = "testing")]
pub mod testing;

pub use clock::{Clock, SystemClock};
pub use context::ServiceContext;
pub use error::{Error, ErrorCode, Result};
pub use events::ChangeEvent;
pub use paths::AppPaths;
