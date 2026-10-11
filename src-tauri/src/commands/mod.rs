//! Tauri command handlers: the local commands, and [`board::route`], the door
//! the board commands reach `rimaia_core::api` through (seam-contract D32).
//!
//! Deliberately thin: marshal arguments, call a `rimaia-core` service, return.
//! No branching on business rules, because the MCP server (ADR-0006) calls those
//! same services without passing through here — a rule enforced in only one of
//! the two paths is a bug. Every command is a row in
//! `crates/core/src/api/registry.rs`; a board row's handler lives in core, and
//! only a local row's lives here.

pub mod app;
pub mod board;
pub mod doctor;
pub mod mcp;
pub mod queue;
pub mod repositories;
pub mod runs;
pub mod schedules;
pub mod settings;
pub mod strategy;
pub mod worktree;
