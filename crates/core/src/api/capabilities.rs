//! What the calling client can do, which only the client knows (ADR-0034
//! point 5, task 049).
//!
//! `get_client_capabilities` is a local command: the desktop shell answers it
//! from this module, and a browser answers it in `src/lib/commands.ts` without
//! a request. The server never does, because it cannot say what the page that
//! called it can do. Lives in core rather than the shell because CI runs
//! `cargo test -p rimaia-core` and nothing runs tests in `src-tauri`; it takes
//! plain values because core must not depend on `rimaia-runner`.

use serde::Serialize;

/// How this client reaches its board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientMode {
    /// The desktop, over its own board file.
    Solo,
    /// The desktop, with a server for the board and a local shell for the
    /// machine.
    Connected,
    /// A page served by a server.
    Browser,
}

/// Mirrored by hand in `src/types.ts` as `ClientCapabilities`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    pub mode: ClientMode,
    /// The runner on this machine, `None` where there is none to name.
    pub local_runner_id: Option<String>,
    pub can_open_in_editor: bool,
    pub can_reveal_files: bool,
    pub can_choose_folders: bool,
}

impl ClientCapabilities {
    /// A desktop: it can do every local action. `server_url` is the
    /// `runner_identity` row's, `None` in solo.
    pub fn desktop(local_runner_id: String, server_url: Option<&str>) -> Self {
        Self {
            mode: if server_url.is_some() {
                ClientMode::Connected
            } else {
                ClientMode::Solo
            },
            local_runner_id: Some(local_runner_id),
            can_open_in_editor: true,
            can_reveal_files: true,
            can_choose_folders: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn a_desktop_without_a_server_is_solo_and_can_do_every_local_action() {
        let capabilities = ClientCapabilities::desktop("runner-1".to_string(), None);

        assert_eq!(capabilities.mode, ClientMode::Solo);
        assert!(capabilities.can_open_in_editor);
        assert!(capabilities.can_reveal_files);
        assert!(capabilities.can_choose_folders);
    }

    #[test]
    fn a_desktop_with_a_server_url_is_connected_and_names_its_local_runner() {
        let capabilities =
            ClientCapabilities::desktop("runner-2".to_string(), Some("https://board.example"));

        assert_eq!(capabilities.mode, ClientMode::Connected);
        assert_eq!(capabilities.local_runner_id.as_deref(), Some("runner-2"));
        assert!(capabilities.can_choose_folders);
    }

    /// `src/types.ts` mirrors these keys by hand, and a misspelt one would
    /// typecheck on both sides and read `undefined` in the page.
    #[test]
    fn capabilities_serialize_with_the_keys_the_frontend_reads() {
        let capabilities = ClientCapabilities::desktop("runner-1".to_string(), None);

        assert_eq!(
            serde_json::to_value(capabilities).expect("serializes"),
            json!({
                "mode": "solo",
                "localRunnerId": "runner-1",
                "canOpenInEditor": true,
                "canRevealFiles": true,
                "canChooseFolders": true,
            })
        );
    }
}
