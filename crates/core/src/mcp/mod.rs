//! The embedded local MCP server (ADR-0006), so Claude Code sessions the user is
//! already running can read and write the board.
//!
//! It lives in core, not in the shell, precisely so its tool handlers are thin
//! adapters over the same services the Tauri commands call. A rule enforced in
//! only one of the two paths is a bug — the same invariant must produce the same
//! rejection whichever door it comes through.
//!
//! Task 010 fills it in: [`settings`] owns the port key, `requests` and
//! `responses` are the wire DTOs, `server` holds the ten tool handlers, and
//! `build` binds the listener.

pub mod error;
pub mod requests;
pub mod responses;
pub mod server;
pub mod settings;

pub use error::ToolError;
pub use server::RimaiaServer;
pub use settings::{configured_port, set_configured_port, MCP_PORT};

/// The port ADR-0006 fixes as the default, and the one every `claude mcp add`
/// line in the docs uses. Configurable for a collision; the *interface* is not
/// configurable and is hard-coded to loopback.
pub const DEFAULT_PORT: u16 = 4517;

/// The path the streamable-HTTP endpoint is mounted at, so
/// `http://127.0.0.1:4517/mcp` is what a user registers.
pub const MCP_PATH: &str = "/mcp";
