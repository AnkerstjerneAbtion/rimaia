//! The embedded local MCP server (ADR-0006), so Claude Code sessions the user is
//! already running can read and write the board.
//!
//! It lives in core, not in the shell, precisely so its tool handlers are thin
//! adapters over the same services the Tauri commands call. A rule enforced in
//! only one of the two paths is a bug — the same invariant must produce the same
//! rejection whichever door it comes through.
//!
//! Filled in by task 010.
