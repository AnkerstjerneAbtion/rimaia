//! Every command, and whether it is a board command or a local one
//! (seam-contract D32 point 1, ADR-0034 point 1).
//!
//! **This table is the list.** Four things read it: the solo shell's invoke
//! path (`commands::board::route` sends a board name to [`dispatch`] and
//! anything else to its `generate_handler!` list), `rimaia-server`'s
//! `POST /api/v1/<name>` routes (one per board row), `src/lib/commands.ts`
//! (whose wrapper calls `board<T>` or `local<T>` to match) and
//! `scripts/check-command-wiring.sh`, which fails when any of them disagrees
//! with this file. A new command is not done until it has a row here; a new
//! board row is not done until it has a case in `testing/api.rs`.
//!
//! A row's kind says where the command is served **today** (D32 point 8). The
//! eight commands that still spawn, cancel or read on this machine are `local`
//! rows with a comment naming the task that flips them, and a flip is one
//! commit across this file, the handler list and `commands.ts`. The three
//! planning commands are local for good (D32's amendment on them).
//!
//! [`dispatch`]: super::dispatch

use std::fmt;
use std::future::Future;
use std::sync::LazyLock;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use super::board::{
    account, analytics, consent, repositories, review, runs, settings, strategy, tasks,
};
use super::{BoardRequest, BoxFuture};
use crate::error::{Error, Result};

pub struct Command {
    pub name: &'static str,
    pub kind: Kind,
}

pub enum Kind {
    /// Served by [`api::dispatch`](super::dispatch): over HTTP by
    /// `rimaia-server`, in process by the solo shell.
    Board {
        effect: Effect,
        handler: BoardHandler,
    },
    /// Served by the desktop shell's `generate_handler!` list, and by nothing
    /// else.
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Changes no stored state and publishes no `ChangeEvent`.
    Read,
    /// May do either, including recording a request for a runner to claim.
    Write,
}

/// A board handler with its argument and output types erased, so one table
/// can hold every row.
pub type BoardHandler =
    Box<dyn Fn(BoardRequest, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

impl Command {
    /// The row's effect, or `None` for a local row, which has none.
    pub fn effect(&self) -> Option<Effect> {
        match self.kind {
            Kind::Board { effect, .. } => Some(effect),
            Kind::Local => None,
        }
    }

    pub fn is_board(&self) -> bool {
        matches!(self.kind, Kind::Board { .. })
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("name", &self.name)
            .field("effect", &self.effect())
            .finish()
    }
}

pub static COMMANDS: LazyLock<Vec<Command>> = LazyLock::new(|| {
    use Effect::{Read, Write};
    vec![
        // app
        local("get_app_info"),
        local("reveal_app_data_dir"),
        local("debug_provoke_error"),
        local("get_client_capabilities"),
        local("choose_folder"),
        // repositories
        board("list_repositories", Read, repositories::list_repositories),
        local("list_checkouts"),
        local("register_repository"), // local until task 054 flips it to board
        board("update_repository", Write, repositories::update_repository),
        local("set_repository_worktree_root"),
        local("set_repository_unattended_runs"),
        board(
            "set_repository_unattended_ceiling",
            Write,
            repositories::set_repository_unattended_ceiling,
        ),
        local("set_repository_on_archive"),
        local("set_repository_max_concurrency"),
        board("remove_repository", Write, repositories::remove_repository),
        local("get_repository_remote_info"),
        local("get_repository_credential_status"),
        local("set_repository_credential"),
        local("remove_repository_credential"),
        // tasks
        board("create_task", Write, tasks::create_task),
        board("get_task", Read, tasks::get_task),
        board("list_tasks", Read, tasks::list_tasks),
        board("update_task", Write, tasks::update_task),
        board("delete_task", Write, tasks::delete_task),
        board("archive_task", Write, tasks::archive_task),
        board("archive_tasks", Write, tasks::archive_tasks),
        board("unarchive_task", Write, tasks::unarchive_task),
        board("move_task", Write, tasks::move_task),
        board("set_task_run_state", Write, tasks::set_task_run_state),
        board("add_task_link", Write, tasks::add_task_link),
        board("update_task_link", Write, tasks::update_task_link),
        board("remove_task_link", Write, tasks::remove_task_link),
        board("reorder_task_link", Write, tasks::reorder_task_link),
        board("set_task_dependencies", Write, tasks::set_task_dependencies),
        board("assign_task", Write, tasks::assign_task),
        board("get_blocking_reason", Read, tasks::get_blocking_reason),
        // review
        board("approve_task", Write, review::approve_task),
        board("reject_task", Write, review::reject_task),
        board("request_task_changes", Write, review::request_task_changes),
        board("get_task_dependents", Read, review::get_task_dependents),
        board("get_review_digest", Read, review::get_review_digest),
        board(
            "mark_review_digest_seen",
            Write,
            review::mark_review_digest_seen,
        ),
        board("list_review_findings", Read, review::list_review_findings),
        board("get_review_history", Read, review::get_review_history),
        board("get_review_level", Read, review::get_review_level),
        board("get_review_settings", Read, review::get_review_settings),
        board("set_review_settings", Write, review::set_review_settings),
        board(
            "set_repository_review_config",
            Write,
            review::set_repository_review_config,
        ),
        board("set_task_review", Write, review::set_task_review),
        // consent
        board("accept_content", Write, consent::accept_content),
        board("get_task_consent", Read, consent::get_task_consent),
        // settings
        board(
            "get_base_instructions",
            Read,
            settings::get_base_instructions,
        ),
        board(
            "set_base_instructions",
            Write,
            settings::set_base_instructions,
        ),
        local("get_run_environment"),
        board("get_run_cost_summary", Read, settings::get_run_cost_summary),
        local("set_run_environment"),
        local("get_strategy_ceiling"),
        local("set_strategy_ceiling"),
        local("preview_composed_prompt"),
        // strategy
        board(
            "get_strategy_catalogue",
            Read,
            strategy::get_strategy_catalogue,
        ),
        board(
            "set_strategy_catalogue",
            Write,
            strategy::set_strategy_catalogue,
        ),
        board(
            "get_strategy_defaults",
            Read,
            strategy::get_strategy_defaults,
        ),
        board(
            "set_strategy_defaults",
            Write,
            strategy::set_strategy_defaults,
        ),
        board(
            "get_strategy_approval",
            Read,
            strategy::get_strategy_approval,
        ),
        board(
            "set_strategy_approval",
            Write,
            strategy::set_strategy_approval,
        ),
        board(
            "accept_task_strategy",
            Write,
            strategy::accept_task_strategy,
        ),
        board("clear_task_strategy", Write, strategy::clear_task_strategy),
        local("plan_task_strategy"),
        local("plan_tasks_strategy"),
        local("cancel_plan_pass"),
        // worktree
        local("get_worktree_status"),
        local("get_diff_summary"),
        local("reveal_task_worktree"),
        local("list_open_in_targets"),
        local("open_task_worktree_in"),
        local("get_worktree_inventory"),
        local("remove_task_worktree"),
        local("cleanup_done_worktrees"),
        local("cleanup_merged_worktrees"),
        local("get_worktree_auto_cleanup"),
        local("set_worktree_auto_cleanup"),
        local("list_local_worktrees"),
        // runs
        local("start_task_run"),  // local until task 052 flips it to board
        local("cancel_task_run"), // local until task 052 flips it to board
        local("retry_task_now"),  // local until task 052 flips it to board
        board("give_up_on_task", Write, runs::give_up_on_task),
        board("get_run_tail", Read, runs::get_run_tail),
        board("list_runs_for_task", Read, runs::list_runs_for_task),
        board("list_runs", Read, runs::list_runs),
        board("get_run", Read, runs::get_run),
        local("read_run_transcript_page"), // local until task 056 flips it to board
        local("search_run_transcript"),    // local until task 056 flips it to board
        local("summarize_run_transcript"), // local until task 056 flips it to board
        local("reveal_run_log"),
        local("get_run_log_path"),
        local("get_run_log_size"),
        local("prune_run_logs"),
        // queue
        local("start_queue"),
        local("pause_queue"),
        local("resume_queue"),
        local("stop_queue"),
        local("get_queue_status"),
        local("get_run_capacity"),
        local("set_schedule_mode"),
        local("set_max_concurrency"),
        // schedules
        local("list_schedules"),
        local("create_schedule"),
        local("update_schedule"),
        local("set_schedule_enabled"),
        local("delete_schedule"),
        local("preview_schedule_preflight"),
        local("list_timezones"),
        // mcp
        local("get_mcp_status"),
        local("set_mcp_port"),
        local("test_mcp_connection"),
        // analytics
        board("get_analytics", Read, analytics::get_analytics),
        board(
            "get_subscription_cost",
            Read,
            analytics::get_subscription_cost,
        ),
        board(
            "set_subscription_cost",
            Write,
            analytics::set_subscription_cost,
        ),
        // account (task 047): the caller's own user, sessions and tokens
        board("get_account", Read, account::get_account),
        board("list_sessions", Read, account::list_sessions),
        board("revoke_session", Write, account::revoke_session),
        board("list_api_tokens", Read, account::list_api_tokens),
        board(
            "create_personal_access_token",
            Write,
            account::create_personal_access_token,
        ),
        board("revoke_api_token", Write, account::revoke_api_token),
        board("create_pairing_code", Write, account::create_pairing_code),
        board("pair_own_runner", Write, account::pair_own_runner),
        // doctor
        local("run_doctor"),
        local("dismiss_onboarding"),
        local("dismiss_doctor_warning"),
        local("restore_doctor_warning"),
    ]
});

/// The row named `name`, or `None` for a name no row has.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.name == name)
}

/// A board row over a typed handler.
///
/// The erased handler deserializes the argument object into `A` (a failure
/// is `Invalid`, naming the command), awaits `f`, and serializes `O`.
fn board<A, O, F, Fut>(name: &'static str, effect: Effect, f: F) -> Command
where
    A: DeserializeOwned + Send + 'static,
    O: Serialize + 'static,
    F: Fn(BoardRequest, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<O>> + Send + 'static,
{
    let handler: BoardHandler = Box::new(move |request, args| {
        let call = serde_json::from_value::<A>(args)
            .map(|args| f(request, args))
            .map_err(|error| {
                Error::invalid(format!("`{name}` could not read its arguments: {error}"))
            });
        Box::pin(async move {
            let output = call?.await?;
            serde_json::to_value(output).map_err(|error| {
                Error::internal(format!("`{name}` could not serialize its answer: {error}"))
            })
        })
    });
    Command {
        name,
        kind: Kind::Board { effect, handler },
    }
}

/// A local row: the name, and nothing else.
fn local(name: &'static str) -> Command {
    Command {
        name,
        kind: Kind::Local,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn every_command_is_registered_once() {
        let mut seen = BTreeSet::new();
        let duplicates: Vec<&str> = COMMANDS
            .iter()
            .map(|command| command.name)
            .filter(|name| !seen.insert(*name))
            .collect();

        assert_eq!(duplicates, Vec::<&str>::new());
    }

    #[test]
    fn find_answers_a_row_by_name_and_none_for_an_unknown_one() {
        assert_eq!(
            find("get_task").map(Command::effect),
            Some(Some(Effect::Read))
        );
        assert_eq!(find("get_app_info").map(Command::effect), Some(None));
        assert!(find("no_such_command").is_none());
    }
}
