//! One command surface, whichever door a request came through (seam-contract
//! D32, ADR-0034).
//!
//! [`registry::COMMANDS`] says of every command whether it is a board command
//! or a local one. A board command runs through [`dispatch`] and nothing else:
//! the solo shell calls it in process for `invoke`, and `rimaia-server` calls it
//! for `POST /api/v1/<name>`. The in-process answer and the HTTP answer are the
//! same by construction, because below the transport they are one function.
//!
//! A handler receives a [`BoardRequest`]: a context already scoped to the
//! [`Caller`]'s teams and acting for them, the caller itself, the host, and,
//! only in solo, this machine.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tracing::Instrument;

use crate::context::ServiceContext;
use crate::error::{Error, Result};
use crate::machine::MachineContext;
use crate::paths::AppPaths;
use crate::runner::provider::ProviderProfile;

pub mod board;
pub mod caller;
pub mod protocol;
pub mod registry;

pub use caller::{Authenticate, Caller, Credential, Door, RefuseAll, TeamGrant};
pub use registry::{Command, Effect, Kind, COMMANDS};

/// A boxed, sendable future. A local alias rather than the `futures` crate's,
/// which nothing else here needs (D32 point 1).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The version of the board protocol this build speaks, sent by every client
/// in `Rimaia-Protocol` (ADR-0037 point 4). Its major matches the `/v1` path
/// segment. `src/lib/commands.ts` carries a copy, which the wiring script holds
/// equal to this one.
pub const PROTOCOL_VERSION: &str = "1.0";

/// What a host serving board commands holds for every request.
#[derive(Clone)]
pub struct BoardHost {
    /// The unscoped base. [`dispatch`] never hands it to a handler as it is:
    /// every request is re-scoped from its caller first.
    pub context: ServiceContext,
    /// The provider whose catalogue and cost the board reads, as data only.
    pub provider: ProviderProfile,
    /// Where `get_run` and `list_runs` look for a transcript to report
    /// `logAvailable` (task 066). Solo passes the app's paths; a server passes
    /// its data directory, where `runs/` stays empty until task 056 replaces
    /// this field with its transcript hold.
    pub paths: AppPaths,
}

/// Everything a board handler may read.
#[derive(Clone)]
pub struct BoardRequest {
    /// `host.context.for_caller(&caller)`: scope, actor and source already set.
    pub ctx: ServiceContext,
    pub caller: Caller,
    pub host: BoardHost,
    /// This machine, when the request came from a shell that has one (task
    /// 046 point 3a). Seven handlers pass it on, to the core functions that
    /// react on this machine in the same call: archive, archive many, move,
    /// approve, reject, remove a repository, and the review digest. A server
    /// never has one.
    pub machine: Option<MachineContext>,
}

/// Runs the board command `name` for `caller`, with no machine: what a server,
/// every contract test and a connected desktop call.
///
/// A name that is not a board row is `NotFound`, and `null` arguments read as
/// `{}`.
pub async fn dispatch(host: &BoardHost, caller: Caller, name: &str, args: Value) -> Result<Value> {
    run(host, caller, None, name, args).await
}

/// [`dispatch`], for the solo shell, which hands this machine on to the seven
/// handlers that react on it (task 046 point 3a). `commands::board::route` is
/// its only caller.
pub async fn dispatch_with_machine(
    host: &BoardHost,
    caller: Caller,
    machine: &MachineContext,
    name: &str,
    args: Value,
) -> Result<Value> {
    run(host, caller, Some(machine.clone()), name, args).await
}

/// The one path both entry points share: lookup, argument defaulting,
/// re-scoping and the span.
async fn run(
    host: &BoardHost,
    caller: Caller,
    machine: Option<MachineContext>,
    name: &str,
    args: Value,
) -> Result<Value> {
    let Some(handler) = registry::find(name).and_then(|command| match &command.kind {
        registry::Kind::Board { handler, .. } => Some(handler),
        registry::Kind::Local => None,
    }) else {
        return Err(Error::not_found(format!(
            "there is no board command `{name}`"
        )));
    };
    let args = match args {
        Value::Null => Value::Object(Default::default()),
        args => args,
    };
    let ctx = host.context.for_caller(&caller)?;
    // Never the arguments or the output: they carry plans (ADR-0037 point 6).
    let span = tracing::info_span!(
        "command",
        name,
        door = caller.door.name(),
        user_id = %caller.user_id,
    );
    let request = BoardRequest {
        ctx,
        caller,
        host: host.clone(),
        machine,
    };
    handler(request, args).instrument(span).await
}
