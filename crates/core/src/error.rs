//! The one error type.
//!
//! No `String` errors cross a boundary — not the Tauri IPC boundary, not the MCP
//! one. [`Error`] serializes to a stable `{ code, message }` payload so the
//! frontend can branch on [`ErrorCode`] without matching on prose, and can
//! always render *something* readable.
//!
//! `Serialize` is written by hand rather than derived: `sqlx::Error` and
//! `std::io::Error` are not serializable, and flattening them into a message is
//! the correct answer anyway — the UI has no use for a driver's error struct.

use serde::ser::{Serialize, SerializeStruct, Serializer};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{message}")]
    NotFound { message: String },

    #[error("{message}")]
    Invalid { message: String },

    /// The lease a report was made under is not the current one (ADR-0031
    /// point 3, seam-contract D8's 043 amendment). It means that and nothing
    /// else: a fenced report, never a lost race and never a refusal a person
    /// reads.
    #[error("{message}")]
    Conflict { message: String },

    /// The request carried no credential the server accepts, or one that
    /// cannot drive the surface it reached (seam-contract D32 point 7). Never
    /// "this exists, but not for you": that is `NotFound` (ADR-0029 point 5).
    #[error("{message}")]
    Unauthenticated { message: String },

    /// The client's protocol version is one this server no longer, or not
    /// yet, accepts a write from (ADR-0037 point 4, D32 point 7).
    #[error("{message}")]
    UpgradeRequired { message: String },

    /// Escape hatch for context-rich failures from `anyhow`-using internals.
    /// Not for anything the UI is expected to distinguish.
    #[error("{0}")]
    Internal(#[from] anyhow::Error),
}

/// The machine-readable half of the payload. Kept deliberately coarse: it exists
/// so the frontend can choose a presentation, not so it can reimplement backend
/// logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Database,
    Io,
    NotFound,
    Invalid,
    Conflict,
    Unauthenticated,
    UpgradeRequired,
    Internal,
}

impl ErrorCode {
    /// Every code, for a table that has to answer each one (the server's
    /// status map).
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::Database,
        ErrorCode::Io,
        ErrorCode::NotFound,
        ErrorCode::Invalid,
        ErrorCode::Conflict,
        ErrorCode::Unauthenticated,
        ErrorCode::UpgradeRequired,
        ErrorCode::Internal,
    ];
}

impl Error {
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Database(_) => ErrorCode::Database,
            Error::Io(_) => ErrorCode::Io,
            Error::NotFound { .. } => ErrorCode::NotFound,
            Error::Invalid { .. } => ErrorCode::Invalid,
            Error::Conflict { .. } => ErrorCode::Conflict,
            Error::Unauthenticated { .. } => ErrorCode::Unauthenticated,
            Error::UpgradeRequired { .. } => ErrorCode::UpgradeRequired,
            Error::Internal(_) => ErrorCode::Internal,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Error::NotFound {
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Error::Invalid {
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Error::Conflict {
            message: message.into(),
        }
    }

    pub fn unauthenticated(message: impl Into<String>) -> Self {
        Error::Unauthenticated {
            message: message.into(),
        }
    }

    pub fn upgrade_required(message: impl Into<String>) -> Self {
        Error::UpgradeRequired {
            message: message.into(),
        }
    }

    /// So callers can raise an internal failure without taking a dependency on
    /// `anyhow` themselves — the Tauri shell in particular.
    pub fn internal(message: impl Into<String>) -> Self {
        Error::Internal(anyhow::Error::msg(message.into()))
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut payload = serializer.serialize_struct("Error", 2)?;
        payload.serialize_field("code", &self.code())?;
        payload.serialize_field("message", &self.to_string())?;
        payload.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_json(error: Error) -> String {
        serde_json::to_string(&error).expect("error payload must always serialize")
    }

    #[test]
    fn invalid_serializes_to_code_and_message() {
        assert_eq!(
            as_json(Error::invalid("a task needs a repository")),
            r#"{"code":"invalid","message":"a task needs a repository"}"#
        );
    }

    #[test]
    fn not_found_serializes_to_code_and_message() {
        assert_eq!(
            as_json(Error::not_found("no task with that id")),
            r#"{"code":"not_found","message":"no task with that id"}"#
        );
    }

    #[test]
    fn conflict_serializes_to_code_and_message() {
        assert_eq!(
            as_json(Error::conflict("this lease is not the current one")),
            r#"{"code":"conflict","message":"this lease is not the current one"}"#
        );
    }

    #[test]
    fn unauthenticated_serializes_to_code_and_message() {
        assert_eq!(
            as_json(Error::unauthenticated("sign in to use this board")),
            r#"{"code":"unauthenticated","message":"sign in to use this board"}"#
        );
    }

    #[test]
    fn upgrade_required_serializes_to_code_and_message() {
        assert_eq!(
            as_json(Error::upgrade_required("update this client")),
            r#"{"code":"upgrade_required","message":"update this client"}"#
        );
    }

    #[test]
    fn every_code_is_listed_once_in_all() {
        // `code()` is exhaustive by the compiler; `ALL` is not, so a variant
        // added without a line there would leave the server's status table
        // untested for it.
        let codes = [
            Error::from(sqlx::Error::RowNotFound).code(),
            Error::from(std::io::Error::other("io")).code(),
            Error::not_found("").code(),
            Error::invalid("").code(),
            Error::conflict("").code(),
            Error::unauthenticated("").code(),
            Error::upgrade_required("").code(),
            Error::internal("").code(),
        ];
        assert_eq!(ErrorCode::ALL, codes);
    }

    #[test]
    fn io_error_flattens_instead_of_leaking_a_driver_struct() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        assert_eq!(
            as_json(Error::from(io)),
            r#"{"code":"io","message":"permission denied"}"#
        );
    }

    #[test]
    fn database_error_serializes_with_a_renderable_message() {
        assert_eq!(
            as_json(Error::from(sqlx::Error::RowNotFound)),
            r#"{"code":"database","message":"database error: no rows returned by a query that expected to return at least one row"}"#
        );
    }

    #[test]
    fn internal_error_keeps_the_anyhow_context() {
        assert_eq!(
            as_json(Error::from(anyhow::anyhow!("worktree root vanished"))),
            r#"{"code":"internal","message":"worktree root vanished"}"#
        );
    }
}
