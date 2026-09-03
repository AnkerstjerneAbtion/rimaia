//! Tauri commands for the MCP server (task 010; ADR-0006, seam-contract D16).
//!
//! Thin over `rimaia_core::mcp`, exactly like every other command module. The
//! one thing that happens here and nowhere else is the *handle swap*: a
//! listening socket cannot be rebound to a different port, so moving the port
//! means building a second server and replacing the one `AppState` holds. That
//! is a shell concern — core owns the port setting and the bind, the shell owns
//! the handle for the life of the process.

use std::mem;

use rimaia_core::doctor;
use rimaia_core::mcp::{self, McpProbe, McpState, McpStatus};
use rimaia_core::{Error, Result};
use tauri::State;

use crate::state::AppState;

/// What Settings → MCP renders: listening or not, on which address, and the
/// operating system's own words if the port was taken.
///
/// Synchronous: it reads a cached snapshot of the bind, which is the thing that
/// fails. Whether the server is *still* answering is [`test_mcp_connection`]'s
/// question, and the panel presents that as the only live check.
#[tauri::command]
pub fn get_mcp_status(state: State<'_, AppState>) -> Result<McpStatus> {
    Ok(state
        .mcp
        .lock()
        .map_err(|_| Error::internal("the mcp handle mutex is poisoned"))?
        .status())
}

/// Stores the port and restarts the server on it.
///
/// **A no-op when the port has not changed.** Rebinding a socket that is
/// currently listening races itself — the old listener is still holding the
/// port at the moment the new one asks for it, and `SO_REUSEADDR` does not
/// cover that case — so the user would be told their own port is in use.
///
/// Otherwise it swaps **unconditionally**, even onto a port that turns out to
/// be taken. Leaving the previous listener up after the setting changed is the
/// stored-versus-running divergence this codebase refuses everywhere else: the
/// panel would show a URL for a port the settings no longer name, and the next
/// launch would disagree with the running process.
#[tauri::command]
pub async fn set_mcp_port(state: State<'_, AppState>, port: u16) -> Result<McpStatus> {
    // Core's own guard on the range, and the message the panel renders.
    mcp::set_configured_port(&state.context, port).await?;

    let current = get_mcp_status(state.clone())?;
    if current.state == McpState::Listening && current.configured_port == port {
        return Ok(current);
    }

    // The same `RunHandles` the first bind was given, so the rebind records
    // the new address into the value the runner already holds (seam-contract
    // D17.4). Handing this one a fresh set would leave every later run minting
    // tokens against a table nothing routes.
    let (handle, task) = mcp::build(
        state.context.clone(),
        port,
        state.run_handles.clone(),
        // Rebuilt from the same two shell values the first bind used, for the
        // same reason the `RunHandles` above are reused: a rebind must not
        // quietly narrow what `run_doctor` can see.
        doctor::Environment::for_runner(state.paths.clone(), &state.runner),
    )
    .await;
    tauri::async_runtime::spawn(task.run());

    let previous = {
        let mut held = state
            .mcp
            .lock()
            .map_err(|_| Error::internal("the mcp handle mutex is poisoned"))?;
        mem::replace(&mut *held, handle)
    };
    // After the swap and outside the lock: shutting the old one down is what
    // frees its port, and nothing should be holding a mutex while it happens.
    previous.shutdown();

    get_mcp_status(state)
}

/// One real `initialize` + `tools/list` round trip against the address the
/// server actually bound — the way a client would, rather than a
/// "something is listening" check that would pass against any open socket.
#[tauri::command]
pub async fn test_mcp_connection(state: State<'_, AppState>) -> Result<McpProbe> {
    let status = get_mcp_status(state)?;
    let Some(address) = status.bound_address else {
        return Err(Error::invalid(
            "the MCP server is not listening, so there is nothing to test — set a free port first",
        ));
    };

    mcp::probe(&address).await
}
