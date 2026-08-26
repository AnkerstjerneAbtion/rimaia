//! How a refusal reaches the calling agent (ADR-0006, seam-contract D8).
//!
//! MCP draws a line rmcp enforces in its types. `Err(ErrorData)` is a
//! *protocol* error, which clients render opaquely — "tool result missing due
//! to internal error" — and the server's message never reaches the caller.
//! `Ok(CallToolResult::error(...))` is a *tool-level* error whose content is
//! handed to the model.
//!
//! Task 010 requires "a specific, actionable error to the calling agent", so
//! **every** [`Error`] becomes tool-level here — including `Internal` and
//! `Database`, against rmcp's general advice. On a loopback server whose caller
//! is the user's own Claude Code session, an opaque failure costs a whole
//! planning session; a message that says what went wrong costs nothing. The
//! internal ones are logged at `error` on the way out, because those the
//! operator does need to see.
//!
//! The structured payload is byte for byte what [`Error`]'s own `Serialize`
//! produces at the Tauri boundary, so the two doors report the same refusal
//! identically rather than merely both failing — which is the assertion
//! `tests/mcp_tools.rs` makes about every shared invariant.

use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock, ErrorData};

use crate::error::Error;

/// A [`rimaia_core::Error`](Error) on its way to a tool caller.
///
/// A newtype rather than an `impl` on `Error` itself, because
/// `IntoCallToolResult` is rmcp's trait and `Error` is this crate's — and
/// because it keeps the `rmcp` dependency out of `error.rs`, which every module
/// in the crate uses.
#[derive(Debug)]
pub struct ToolError(pub Error);

impl From<Error> for ToolError {
    fn from(error: Error) -> Self {
        ToolError(error)
    }
}

impl IntoCallToolResult for ToolError {
    fn into_call_tool_result(self) -> Result<CallToolResponse, ErrorData> {
        if matches!(self.0, Error::Internal(_) | Error::Database(_)) {
            // The one class the caller cannot act on. It still reaches them —
            // an agent that is told "database is locked" can wait and retry —
            // but the operator is the one who needs the log line.
            tracing::error!(error = %self.0, "an MCP tool call failed internally");
        }

        let mut result = CallToolResult::error(vec![ContentBlock::text(self.0.to_string())]);
        // `structured_content` is assigned rather than passed to a constructor:
        // `CallToolResult` is `#[non_exhaustive]`, which forbids a struct
        // literal, not a field write.
        result.structured_content = serde_json::to_value(&self.0).ok();
        result.into_call_tool_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn as_result(error: Error) -> CallToolResult {
        match ToolError(error)
            .into_call_tool_result()
            .expect("a tool error is never a protocol error")
        {
            CallToolResponse::Complete(result) => result,
            other => panic!("expected a completed result, got {other:?}"),
        }
    }

    #[test]
    fn an_invalid_error_becomes_a_tool_error_carrying_code_and_message() {
        let result = as_result(Error::invalid("column must be one of not_ready, ready"));

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content,
            Some(json!({
                "code": "invalid",
                "message": "column must be one of not_ready, ready",
            }))
        );
    }

    #[test]
    fn the_structured_payload_matches_what_the_tauri_boundary_sends() {
        // ADR-0006's argument in one assertion: the same refusal, the same
        // payload, whichever door asked. If `Error`'s `Serialize` ever grows a
        // field, this is what stops the MCP surface quietly disagreeing.
        let error = Error::not_found("no task with id abc");
        let over_tauri = serde_json::to_value(&error).expect("the command boundary's payload");

        let result = as_result(Error::not_found("no task with id abc"));

        assert_eq!(result.structured_content, Some(over_tauri));
    }

    #[test]
    fn an_internal_failure_still_reaches_the_agent_as_content() {
        // Deliberately against rmcp's general advice — see the module doc.
        let result = as_result(Error::internal("the worktree root vanished"));

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .content
                .first()
                .and_then(|block| block.as_text())
                .map(|text| text.text.as_str()),
            Some("the worktree root vanished"),
            "an opaque protocol error would cost the caller its whole session"
        );
        assert_eq!(
            result.structured_content,
            Some(json!({ "code": "internal", "message": "the worktree root vanished" }))
        );
    }

    #[test]
    fn every_error_code_survives_the_crossing() {
        for error in [
            Error::invalid("bad"),
            Error::not_found("missing"),
            Error::internal("broken"),
            Error::from(sqlx::Error::RowNotFound),
            Error::from(std::io::Error::other("io")),
        ] {
            let expected = serde_json::to_value(&error).expect("payload");
            let code = error.code();
            let result = as_result(error);

            assert_eq!(
                result.structured_content,
                Some(expected),
                "{code:?} must cross intact"
            );
        }
    }
}
