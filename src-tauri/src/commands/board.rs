//! The solo shell's door onto the board commands (seam-contract D32 point 3).
//!
//! Board commands are not registered with Tauri. [`route`] wraps the
//! `generate_handler!` closure: a name `api::registry::find` reports as a board
//! row goes to [`api::dispatch_with_machine`] with the solo caller and this
//! machine, and every other name goes to the closure, which answers it or
//! reports it unknown as it always has. Invoke names and payloads are what they
//! were, so no frontend call changes.

use rimaia_core::api::{self, registry};
use rimaia_core::Error;
use serde_json::Value;
use tauri::ipc::{Invoke, InvokeBody, InvokeError};
use tauri::{Manager, Runtime};

use crate::state::{AppState, SoloBoard};

/// The composite invoke handler: board names to the registry, the rest to
/// `local`, the `generate_handler!` list of local rows.
pub fn route<R: Runtime>(
    local: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke: Invoke<R>| {
        let is_board = registry::find(invoke.message.command()).is_some_and(|row| row.is_board());
        if !is_board {
            return local(invoke);
        }
        let name = invoke.message.command().to_string();
        let args = match invoke.message.payload() {
            InvokeBody::Json(args) => Ok(args.clone()),
            InvokeBody::Raw(_) => Err(Error::invalid(format!(
                "`{name}` takes a JSON argument object, not raw bytes"
            ))),
        };
        let board = invoke
            .message
            .webview()
            .try_state::<AppState>()
            .map(|state| state.board.clone());
        invoke.resolver.respond_async(async move {
            answer(board, &name, args)
                .await
                // The same `{ code, message }` a `#[tauri::command]` returning
                // `Result<_, Error>` rejects with.
                .map_err(InvokeError::from)
        });
        true
    }
}

/// `state` is `None` before `setup()` has managed `AppState`, and its board is
/// `None` on a connected desktop.
async fn answer(
    state: Option<Option<SoloBoard>>,
    name: &str,
    args: rimaia_core::Result<Value>,
) -> rimaia_core::Result<Value> {
    let Some(board) = state else {
        return Err(Error::internal(format!(
            "`{name}` arrived before the app finished starting"
        )));
    };
    // Connected (task 059), board commands go to the server; one that arrives
    // here is a transport bug, refused on the first click rather than written
    // to a board nobody reads.
    let Some(board) = board else {
        return Err(Error::invalid(format!(
            "`{name}` is a board command, and this desktop sends board commands to its server"
        )));
    };
    api::dispatch_with_machine(&board.host, board.caller, &board.machine, name, args?).await
}
