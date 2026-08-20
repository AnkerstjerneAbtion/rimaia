//! Tauri command handlers.
//!
//! Deliberately thin: marshal arguments, call a `rimaia-core` service, return.
//! No branching on business rules, because the MCP server (ADR-0006) calls those
//! same services without passing through here — a rule enforced in only one of
//! the two paths is a bug.

pub mod app;
pub mod repositories;
pub mod settings;
pub mod tasks;
pub mod worktree;
