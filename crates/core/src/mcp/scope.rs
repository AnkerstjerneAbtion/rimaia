//! Which door a tool call arrived through, and what that door may do
//! (ADR-0006's 2026-08-28 amendment, seam-contract D17.4).
//!
//! Until task 020 this server had exactly one caller: the operator's own
//! planning session, reaching `/mcp`, allowed every tool on ADR-0006's table.
//! A run is a second caller with a far narrower job — it has one task, and its
//! entire business with Rimaia is that task — so it gets a second route,
//! `/mcp/run/{token}`, minted per run and revoked when the run ends. `/mcp`
//! itself is untouched: it is the URL the user pasted into `claude mcp add`,
//! ADR-0006 fixes it, and re-scoping it would break every registered session.
//!
//! # The token is not a secret, and is not meant to be
//!
//! It travels in argv, inside `--mcp-config`, so `ps` shows it to the same
//! user. That is not a widening. ADR-0006's trust boundary is already "anything
//! on this machine that can reach loopback", and ADR-0012 hands the run
//! arbitrary bash besides — a secret a process could read out of its own
//! process table protects nothing from that process.
//!
//! **The token's job is to stop the confused deputy.** The realistic failure is
//! a run that has been prompt-injected by a file it read, or is simply mistaken
//! about which card it is working on, addressing a task that is not its own.
//! Before this module the only thing standing between that run and someone
//! else's board was a task id in a sentence in the prompt, which the model may
//! or may not still be attending to twenty turns later. After it, the task id
//! is on the server value and the check is a function call.
//!
//! # One decision point
//!
//! [`RunScope::authorize`] is every handler's first statement, and the only
//! place a tool's availability is decided. `tests/mcp_scope.rs` requires every
//! *registered* tool to have an entry in [`Tool`], so a tool added later cannot
//! reach the wire without someone having said what a run may do with it.
//!
//! `tools/list` is deliberately **not** filtered by scope, so a run is offered
//! tools it will be refused. That is the price of there being one decision
//! point: a filtered advertisement would be a second copy of the table, free to
//! disagree with this one, and the disagreement would be invisible — a tool
//! quietly missing from a list is much harder to notice than a call that comes
//! back with a sentence saying why. A run that tries anyway is told, in words,
//! that it is scoped to one task.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::db::new_id;
use crate::error::{Error, Result};
use crate::mcp::MCP_PATH;

/// Where the scoped route hangs, relative to [`MCP_PATH`].
///
/// One constant, so the path axum registers and the URL a run is handed cannot
/// drift apart — the failure mode being a token that resolves fine and a route
/// that never sees it.
pub(crate) const RUN_ROUTE_PREFIX: &str = "/run/";

/// The scope a [`RimaiaServer`](crate::mcp::RimaiaServer) was reached through.
///
/// It lives on the *server value*, not on the request. That is the whole reason
/// the token is a path segment: `StreamableHttpService`'s service factory is
/// `Fn() -> Result<S, io::Error>` with no access to the request, so a
/// header-carried token would have to be pulled out of request extensions
/// inside each handler — a second parameter on every one of them, every
/// direct-call test rewritten, and the scope living somewhere a newly added
/// tool can silently forget to read (seam-contract D17.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunScope {
    /// `/mcp` — the operator's own session. ADR-0006's table in full.
    Operator,
    /// `/mcp/run/{token}` — one unattended run, working on one task.
    Run { task_id: String },
}

/// Every tool this server registers, plus the one task 020's next commit wires.
///
/// An enum rather than a string, for the reason every other closed set in this
/// crate is one: a typo in `"remove_task_link"` inside an allow table is a hole
/// that compiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    AddTaskLink,
    CreateTask,
    GetBaseInstructions,
    GetTask,
    ListRepositories,
    ListTasks,
    MoveTask,
    RemoveTaskLink,
    SetTaskDependencies,
    /// Task 020's eleventh tool. Its decision is recorded here ahead of the
    /// handler because the decision is ADR-0006's to make, not the handler's.
    SetTaskStrategy,
    UpdateTask,
}

/// What a [`RunScope::Run`] may do with one tool — ADR-0006's amendment table,
/// as a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAccess {
    /// Allowed, and only against the task the token was minted for.
    OwnTaskOnly,
    /// Allowed. There is no task to scope it to.
    Unscoped,
    /// Refused outright.
    Refused,
}

impl Tool {
    /// Every tool with a recorded decision, so a test can walk the table.
    pub const ALL: [Tool; 11] = [
        Tool::AddTaskLink,
        Tool::CreateTask,
        Tool::GetBaseInstructions,
        Tool::GetTask,
        Tool::ListRepositories,
        Tool::ListTasks,
        Tool::MoveTask,
        Tool::RemoveTaskLink,
        Tool::SetTaskDependencies,
        Tool::SetTaskStrategy,
        Tool::UpdateTask,
    ];

    /// The wired name — what `tools/list` advertises and what the ADR table
    /// calls it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Tool::AddTaskLink => "add_task_link",
            Tool::CreateTask => "create_task",
            Tool::GetBaseInstructions => "get_base_instructions",
            Tool::GetTask => "get_task",
            Tool::ListRepositories => "list_repositories",
            Tool::ListTasks => "list_tasks",
            Tool::MoveTask => "move_task",
            Tool::RemoveTaskLink => "remove_task_link",
            Tool::SetTaskDependencies => "set_task_dependencies",
            Tool::SetTaskStrategy => "set_task_strategy",
            Tool::UpdateTask => "update_task",
        }
    }

    /// The decision for a name off the wire, or `None` when nobody has taken
    /// one.
    ///
    /// `tests/mcp_scope.rs` is where `None` costs something: a registered tool
    /// that lands here is a tool whose run-scope decision was never made.
    pub fn from_name(name: &str) -> Option<Self> {
        Tool::ALL.into_iter().find(|tool| tool.as_str() == name)
    }

    /// ADR-0006's amendment table, and the only copy of it in code.
    pub const fn run_access(self) -> RunAccess {
        match self {
            // A run may read and amend the card it was started for, and that
            // is the whole of its write surface.
            Tool::AddTaskLink
            | Tool::GetTask
            | Tool::RemoveTaskLink
            | Tool::SetTaskStrategy
            | Tool::UpdateTask => RunAccess::OwnTaskOnly,

            // Neither takes a task, and a run has a legitimate use for both:
            // the standing instructions it is working under, and the names of
            // the repositories it may be looking at.
            Tool::GetBaseInstructions | Tool::ListRepositories => RunAccess::Unscoped,

            // `move_task` because the runner owns where a card lands when a run
            // finishes, and a run moving its own card to `done` would be
            // marking its own homework. `list_tasks` because a run has no
            // business enumerating someone's board. `create_task` and
            // `set_task_dependencies` because a run spawning or reordering work
            // is orchestration, which ADR-0016 declines to build.
            Tool::CreateTask | Tool::ListTasks | Tool::MoveTask | Tool::SetTaskDependencies => {
                RunAccess::Refused
            }
        }
    }
}

impl RunScope {
    /// The single decision point. **Every handler's first statement.**
    ///
    /// `target_task_id` is the task the call would touch — `None` for the two
    /// tools that take none. A handler whose task id is not in the request
    /// resolves it first and authorizes against the answer;
    /// `remove_task_link` is the one that does.
    ///
    /// The refusal is a plain [`Error::Invalid`], so it reaches the caller as
    /// the same `{ code, message }` payload as every other refusal on either
    /// door (`mcp::error`). A scope check that invented its own shape would be
    /// the one refusal an agent could not handle like the rest.
    pub fn authorize(&self, tool: Tool, target_task_id: Option<&str>) -> Result<()> {
        let RunScope::Run { task_id } = self else {
            // The operator's door is ADR-0006's table in full. Task 020 takes
            // nothing away from it.
            return Ok(());
        };

        match tool.run_access() {
            RunAccess::Unscoped => Ok(()),

            RunAccess::Refused => Err(Error::invalid(format!(
                "{tool} is not available to a run: this handle is scoped to task {task_id}, and a \
                 run may only read and amend its own task.",
                tool = tool.as_str(),
            ))),

            RunAccess::OwnTaskOnly => match target_task_id {
                Some(target) if target == task_id => Ok(()),
                Some(target) => Err(Error::invalid(format!(
                    "this handle is scoped to task {task_id}, so {tool} cannot be called against \
                     task {target}.",
                    tool = tool.as_str(),
                ))),
                // Not reachable from a request: a tool that is scoped to a task
                // always has one to name by the time it authorizes. It is a
                // wiring mistake, so it reads as one rather than as a refusal
                // the agent could act on.
                None => Err(Error::internal(format!(
                    "{tool} was authorized with no task to scope it to",
                    tool = tool.as_str(),
                ))),
            },
        }
    }
}

/// The live run-scoped endpoints: where the server is listening, and which
/// token means which task.
///
/// Cheap to clone; every clone mints, resolves and revokes against the same
/// table. The shell builds one before either subsystem and hands it to both,
/// which is what removes the ordering constraint between `scheduler::build` and
/// [`mcp::build`](crate::mcp::build) — neither has to exist before the other
/// for the runner to have somewhere to mint tokens.
#[derive(Clone, Default)]
pub struct RunHandles {
    shared: Arc<Mutex<Table>>,
}

#[derive(Default)]
struct Table {
    /// `http://127.0.0.1:4517` — origin only. The path is this module's
    /// business, and `None` means nothing is listening.
    endpoint: Option<String>,
    /// Token → task id. One entry per live [`RunGrant`].
    granted: HashMap<String, String>,
}

impl RunHandles {
    /// Records where the server is actually listening, or that it is not.
    ///
    /// Called by [`mcp::build`](crate::mcp::build) on **every** bind, including
    /// the rebind `set_mcp_port` performs at runtime. Shared mutable state
    /// rather than a URL copied once at startup precisely because of that
    /// rebind: a captured URL goes stale the moment the operator changes the
    /// port, and the next run would be handed an endpoint nothing answers.
    pub fn set_endpoint(&self, base_url: Option<String>) {
        self.lock().endpoint = base_url;
    }

    /// Where the server is listening, or `None` when nothing is.
    pub fn endpoint(&self) -> Option<String> {
        self.lock().endpoint.clone()
    }

    /// Mints a token for one run, valid until the returned grant is dropped.
    ///
    /// A hyphenated v4 UUID, like every other id in this app. Unguessable by
    /// accident rather than by an adversary — see this module's header on what
    /// the token is for.
    pub fn grant(&self, task_id: &str) -> RunGrant {
        let token = new_id();
        self.lock()
            .granted
            .insert(token.clone(), task_id.to_string());

        RunGrant {
            token,
            task_id: task_id.to_string(),
            handles: self.clone(),
        }
    }

    /// The scope a token names, or `None` when it is unknown or revoked.
    ///
    /// The two are deliberately indistinguishable: the route answers both with
    /// a bare 404, so this surface is not an oracle for which tokens exist.
    pub fn resolve(&self, token: &str) -> Option<RunScope> {
        self.lock().granted.get(token).map(|task_id| RunScope::Run {
            task_id: task_id.clone(),
        })
    }

    /// The `--mcp-config` argument for a run holding `grant`, or `None` when
    /// nothing is listening.
    ///
    /// An inline JSON string rather than a file (seam-contract D17.4):
    /// `runner::process` earns its tests by pinning argv byte for byte and a
    /// temp path changes every run, and there is nothing to create, clean up,
    /// or leave inside a worktree where the run could stage it.
    ///
    /// It takes the grant rather than a bare token so that a config can only be
    /// built by whoever holds the grant, which is the same thing as saying it
    /// cannot outlive the token it names. `None` has exactly one cause — no
    /// endpoint bound, the busy-port case seam-contract D16.7 makes non-fatal —
    /// so the caller can say "see Settings → MCP" without qualifying it.
    pub fn mcp_config_json(&self, grant: &RunGrant) -> Option<String> {
        let endpoint = self.endpoint()?;
        let url = format!(
            "{endpoint}{MCP_PATH}{RUN_ROUTE_PREFIX}{token}",
            token = grant.token
        );

        // `serde_json` rather than `format!`, so a URL is escaped as JSON
        // demands rather than as this line happens to assume. `Value`'s
        // `Display` is the compact form, which is what has to survive argv.
        Some(
            serde_json::json!({
                "mcpServers": { "rimaia": { "type": "http", "url": url } }
            })
            .to_string(),
        )
    }

    fn revoke(&self, token: &str) {
        self.lock().granted.remove(token);
    }

    /// `std::sync::Mutex` rather than tokio's, for the reason
    /// `scheduler::queue`'s `Shared` gives at its own: it is only ever held
    /// across a hash-map operation, never across an `await`. A poisoned lock is
    /// recovered rather than propagated — a panic somewhere else must not turn
    /// every later run's handle into a panic of its own.
    fn lock(&self) -> MutexGuard<'_, Table> {
        self.shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One run's live token. **Dropping it revokes the token**, so a cancelled or
/// panicking run cannot leave a live handle to a task behind.
///
/// Deliberately not `Clone`: with two owners, revocation would have to happen
/// on the first drop or the last, and neither is a rule worth having. One run,
/// one grant, and the compiler says so.
pub struct RunGrant {
    token: String,
    task_id: String,
    handles: RunHandles,
}

impl RunGrant {
    /// The minted token. Mostly for a test that wants to assert the URL a run
    /// was handed; the runner itself should ask for
    /// [`mcp_config_json`](RunHandles::mcp_config_json).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The task this grant is scoped to.
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
}

impl Drop for RunGrant {
    fn drop(&mut self) {
        self.handles.revoke(&self.token);
    }
}

impl std::fmt::Debug for RunGrant {
    /// Hand-written to keep the token out of a log line. It is not a secret,
    /// but it is also not something a `?` on a struct holding a grant should
    /// print by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunGrant")
            .field("task_id", &self.task_id)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RunHandles {
    /// Hand-written for [`RunGrant`]'s reason, one level up: `RunnerConfig`
    /// derives `Debug` and holds one of these, so a `?` on the runner's config
    /// would otherwise print every live token. The count is the part that is
    /// useful in a log line; the tokens are the part that is not.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let table = self.lock();
        f.debug_struct("RunHandles")
            .field("endpoint", &table.endpoint)
            .field("granted", &table.granted.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn bound() -> RunHandles {
        let handles = RunHandles::default();
        handles.set_endpoint(Some("http://127.0.0.1:4517".to_string()));
        handles
    }

    #[test]
    fn a_granted_token_resolves_to_its_own_task() {
        let handles = bound();

        let grant = handles.grant("task-1");

        assert_eq!(
            handles.resolve(grant.token()),
            Some(RunScope::Run {
                task_id: "task-1".to_string()
            })
        );
        assert_eq!(handles.resolve("not-a-token"), None);
    }

    #[test]
    fn dropping_a_grant_revokes_its_token() {
        // The RAII half of seam-contract D17.4: a run that is cancelled or
        // panics unwinds through this, so there is no path that leaves a live
        // handle to a task behind.
        let handles = bound();
        let token = {
            let grant = handles.grant("task-1");
            grant.token().to_string()
        };

        assert_eq!(handles.resolve(&token), None);
    }

    #[test]
    fn two_runs_get_two_tokens_for_the_same_task() {
        // Task 012's parallel queue is coming, and a token that collided would
        // be revoked by whichever run finished first.
        let handles = bound();

        let first = handles.grant("task-1");
        let second = handles.grant("task-1");

        assert_ne!(first.token(), second.token());
        assert!(handles.resolve(first.token()).is_some());
        assert!(handles.resolve(second.token()).is_some());
    }

    #[test]
    fn the_mcp_config_is_an_inline_json_object_naming_the_bound_port_and_the_token() {
        let handles = bound();
        let grant = handles.grant("task-1");

        let config = handles
            .mcp_config_json(&grant)
            .expect("an endpoint is bound");

        assert_eq!(
            config,
            format!(
                "{{\"mcpServers\":{{\"rimaia\":{{\"type\":\"http\",\
                 \"url\":\"http://127.0.0.1:4517/mcp/run/{token}\"}}}}}}",
                token = grant.token()
            )
        );
    }

    #[test]
    fn an_unbound_endpoint_yields_no_mcp_config_at_all() {
        // Seam-contract D16.7's busy port, reaching the runner: no
        // `--mcp-config` is passed, and the caller refuses to start a planner
        // rather than starting one that cannot answer.
        let handles = RunHandles::default();
        let grant = handles.grant("task-1");

        assert_eq!(handles.endpoint(), None);
        assert_eq!(handles.mcp_config_json(&grant), None);
    }

    #[test]
    fn rebinding_the_server_moves_the_endpoint_a_run_would_be_handed() {
        // Why this is shared mutable state and not a `String` copied at
        // startup: `commands::mcp::set_mcp_port` rebinds at runtime, and a
        // captured URL would send the next planner at a dead port.
        let handles = bound();
        let grant = handles.grant("task-1");

        handles.set_endpoint(Some("http://127.0.0.1:4600".to_string()));

        assert!(handles
            .mcp_config_json(&grant)
            .expect("an endpoint is bound")
            .contains("http://127.0.0.1:4600/mcp/run/"));
    }

    #[test]
    fn every_tool_name_round_trips_through_its_decision() {
        for tool in Tool::ALL {
            assert_eq!(Tool::from_name(tool.as_str()), Some(tool));
        }
        assert_eq!(Tool::from_name("delete_task"), None);
    }
}
