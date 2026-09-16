//! Claude Code, as a provider (ADR-0004, ADR-0012, ADR-0026).
//!
//! **Every Claude flag string in `rimaia-core` lives in this file.** That is the
//! falsifiable half of ADR-0026: a `--permission-mode` anywhere else under
//! `runner/` is a leak, and the seam tests look for exactly that.
//!
//! # The argument vector is a pure function, on purpose
//!
//! [`ClaudeProvider::plan_spawn`] takes an intent and touches nothing: it is
//! (session, posture, isolation, prompt, model, budget) in, argument vector out.
//! That is what makes ADR-0012's permission posture and ADR-0004's isolation
//! flags assertable as exact vectors, the same class of contract as prompt
//! composition — the flags this product is most dangerous to get wrong are the
//! ones a test can pin byte for byte without spawning anything.
//!
//! **Argument vectors, never `sh -c`.** A worktree path routinely contains a
//! space, and the composed system prompt contains newlines and quotes.
//!
//! # The blocklist is an expansion, not a list
//!
//! Rimaia asks for [`ForbiddenOperation`]s; this file is where each becomes
//! Claude permission-rule patterns, **including the documented incompleteness**
//! — `git push origin +main:main` forces via refspec with no `--force` token to
//! match, and a remote named anything other than `origin` is untouched by the
//! reset pattern. That incompleteness is true of Claude's rule language and is
//! therefore stated where it is true, rather than imposed on every future
//! provider as if it were a property of the operation.

use std::path::Path;

use crate::db::settings::RunEnvironment;
use crate::error::Result;
use crate::mcp::MCP_SERVER_NAME;
use crate::runner::events::RunEvent;

use super::intent::{ForbiddenKind, ForbiddenOperation, PermissionMode, RunIntent, SessionIntent};
use super::{
    AgentProvider, Capabilities, HandleInjection, IsolationSupport, OrchestratorChannel,
    PostureEcho, ProviderId, SessionCapability, SpawnPlan, TurnBudget,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The prerequisite, resolved through `PATH`. **Never bundled** (ADR-0004): it
/// is the same binary the operator already trusts interactively, carrying the
/// subscription auth Rimaia deliberately never handles.
pub const CLAUDE_CLI: &str = "claude";

/// What `strict_local` restricts settings discovery to (ADR-0004's amendment).
///
/// Deliberately **not** `--bare`: that would also switch off `CLAUDE.md`
/// discovery, and a repository's own `CLAUDE.md` is wanted in both modes — it is
/// the project's instructions, not the operator's configuration.
const SETTING_SOURCES: &str = "project,local";

/// The prefix that marks an environment variable as Claude Code's own process
/// identity.
///
/// **A prefix rule, not a list.** `spike/FINDINGS.md` §2b counted thirteen
/// `CLAUDE_*` / `CLAUDECODE` variables exported into children by Claude Code
/// 2.1.234; a list of thirteen names goes stale on the next release, and the
/// failure it produces — a child quietly believing it is a nested session of its
/// parent — is invisible until somebody reads a transcript and finds the wrong
/// session id. The prefix is `CLAUDE` rather than `CLAUDE_` because `CLAUDECODE`
/// carries no underscore.
const IDENTITY_PREFIX: &str = "CLAUDE";

/// `git push --force` and friends, both orderings.
///
/// `Bash(x:*)` is a **command-line prefix** match, so a pattern that only knows
/// the flag-first ordering (`git push --force origin main`) never matches the
/// equally ordinary remote-first one (`git push origin --force main`).
const REMOTE_HISTORY_REWRITE: [&str; 5] = [
    "Bash(git push --force:*)",
    "Bash(git push -f:*)",
    "Bash(git push --force-with-lease:*)",
    "Bash(git push origin --force:*)",
    "Bash(git push origin -f:*)",
];

/// `git push --delete` and friends. `Bash(git push origin :*)` additionally
/// covers the `git push origin :branch` delete shorthand, which carries no
/// `--delete` or `-d` token at all.
const REMOTE_BRANCH_DELETION: [&str; 5] = [
    "Bash(git push --delete:*)",
    "Bash(git push -d:*)",
    "Bash(git push origin --delete:*)",
    "Bash(git push origin -d:*)",
    "Bash(git push origin :*)",
];

/// `git reset --hard origin/…`. One pattern, and a remote named anything other
/// than `origin` is untouched by it — see this module's header.
const HARD_RESET_TO_REMOTE: [&str; 1] = ["Bash(git reset --hard origin/:*)"];

/// Everything a planner is denied on top of the git operations (ADR-0016).
const ANY_FILE_MUTATION: [&str; 3] = ["Write", "Edit", "NotebookEdit"];

/// Denying `Bash` is what makes "runs in a worktree it will not disturb" true
/// rather than merely intended — without it, an agent asked to understand a
/// repository reaches for the test suite.
const ANY_SHELL_COMMAND: [&str; 1] = ["Bash"];

/// What a run refuses to let the agent do when the operator has set no
/// blocklist: ADR-0012 point 3's three operations, spelled out.
///
/// This is the concatenation of [`REMOTE_HISTORY_REWRITE`],
/// [`REMOTE_BRANCH_DELETION`] and [`HARD_RESET_TO_REMOTE`], in that order —
/// which is also the order an intent carrying those three operations expands to,
/// and a unit test below holds the two together.
///
/// **This is not a sandbox and does not pretend to be one.** ADR-0012's own
/// Consequences say so: "the denied-tools list is a blocklist, and blocklists
/// are incomplete by construction. It reduces the common accidents". The
/// isolation that actually bounds a run is the worktree (ADR-0005); this stops
/// the specific mistakes that reach past it into a shared remote.
pub const DEFAULT_DISALLOWED_TOOLS: [&str; 11] = [
    "Bash(git push --force:*)",
    "Bash(git push -f:*)",
    "Bash(git push --force-with-lease:*)",
    "Bash(git push origin --force:*)",
    "Bash(git push origin -f:*)",
    "Bash(git push --delete:*)",
    "Bash(git push -d:*)",
    "Bash(git push origin --delete:*)",
    "Bash(git push origin -d:*)",
    "Bash(git push origin :*)",
    "Bash(git reset --hard origin/:*)",
];

/// Claude Code expresses every axis Rimaia negotiates on, which is why nothing
/// in the current product has ever been refused.
///
/// The seam is not proved by this table; it is proved by the one beside it that
/// says `&[]` on the same row.
pub static CAPABILITIES: &Capabilities = &Capabilities {
    id: ProviderId::ClaudeCode,
    // `--session-id` before the process exists, so `--resume` works even if the
    // child dies before emitting its `init` (ADR-0004).
    session: SessionCapability::PreMinted,
    // `--append-system-prompt`: a channel the agent reads and the task text does
    // not occupy (ADR-0012 point 4).
    orchestrator_channel: OrchestratorChannel::OutOfBand,
    handle_injection: HandleInjection::Argument {
        flag: "--mcp-config",
    },
    isolation: IsolationSupport::Both,
    turn_budget: TurnBudget::ProviderEnforced {
        flag: "--max-turns",
    },
    posture_echo: PostureEcho::Echoed,
    enforceable: &[
        ForbiddenKind::RemoteHistoryRewrite,
        ForbiddenKind::RemoteBranchDeletion,
        ForbiddenKind::HardResetToRemote,
        ForbiddenKind::AnyFileMutation,
        ForbiddenKind::AnyShellCommand,
        ForbiddenKind::RimaiaToolSurface,
        // An operator-authored rule is a `--disallowedTools` pattern, which is
        // exactly what this provider's rule language is made of.
        ForbiddenKind::ProviderRule,
    ],
    identity_prefixes: &[IDENTITY_PREFIX],
};

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// Claude Code. Zero-sized, so `Debug` on the config it hangs off can never leak
/// a token (seam-contract D27.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClaudeProvider;

impl AgentProvider for ClaudeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::ClaudeCode
    }

    fn default_program(&self) -> &'static str {
        CLAUDE_CLI
    }

    fn capabilities(&self) -> &'static Capabilities {
        CAPABILITIES
    }

    /// The argv, in task 008's documented order.
    ///
    /// Order is part of the contract rather than an accident: `--allowedTools`,
    /// `--disallowedTools` and `--mcp-config` are all variadic, so each has to be
    /// followed by a flag (or by nothing) for its list to terminate where this
    /// function intends. Anything non-flag appended after `--mcp-config` would be
    /// read as a second config.
    ///
    /// `scratch` is unused: every channel this provider has is an argument, so
    /// there is nothing to write down. A provider that needs a file is what that
    /// parameter exists for.
    fn plan_spawn(&self, intent: &RunIntent<'_>, _scratch: &Path) -> Result<SpawnPlan> {
        let mut args = vec![
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ];

        // `spike/FINDINGS.md` §6 is explicit about the pairing: "`--session-id`
        // on the first run and `--resume` on the retry is the right shape". They
        // are alternatives, not companions — the first opens an id, the second
        // reuses one. Both name Rimaia's conversation id, because this provider
        // takes the id it is given (ADR-0026 point 5).
        args.push(
            match intent.session {
                SessionIntent::Open { .. } => "--session-id",
                SessionIntent::Continue { .. } => "--resume",
            }
            .to_string(),
        );
        args.push(intent.session.conversation().to_string());

        args.push("--permission-mode".to_string());
        args.push(posture(intent.permission_mode).to_string());

        if intent.run_environment == RunEnvironment::StrictLocal {
            args.push("--strict-mcp-config".to_string());
            args.push("--setting-sources".to_string());
            args.push(SETTING_SOURCES.to_string());
        }

        args.push("--append-system-prompt".to_string());
        args.push(intent.system_append.clone());

        if let Some(model) = &intent.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = &intent.effort {
            args.push("--effort".to_string());
            args.push(effort.clone());
        }

        // Before `--disallowedTools` so the two variadic lists read in the order
        // a human reasons about them — what this run may do, then what it may
        // not. Each `--` token ends the previous list, so the pairing is safe in
        // either order; this one is just the legible one.
        if !intent.required_tools.is_empty() {
            args.push("--allowedTools".to_string());
            args.extend(intent.required_tools.iter().map(|tool| tool_handle(tool)));
        }

        let denied = spell_out(&intent.forbidden);
        if !denied.is_empty() {
            args.push("--disallowedTools".to_string());
            args.extend(denied);
        }

        if let Some(handle) = &intent.rimaia_handle {
            args.push("--mcp-config".to_string());
            args.push(mcp_config_json(handle));
        }

        if let Some(max_turns) = intent.max_turns {
            args.push("--max-turns".to_string());
            args.push(max_turns.to_string());
        }

        Ok(SpawnPlan {
            args,
            env_set: Vec::new(),
            env_remove: Vec::new(),
            stdin: intent.prompt.to_string(),
        })
    }

    fn parse_line(&self, line: &str) -> std::result::Result<RunEvent, serde_json::Error> {
        crate::runner::events::parse_line(line)
    }
}

// ---------------------------------------------------------------------------
// The vocabulary
// ---------------------------------------------------------------------------

/// The CLI's own spelling of a posture, which is also what `init` echoes back —
/// one table, so the request and the verification cannot drift apart.
pub const fn posture(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::BypassPermissions => "bypassPermissions",
        PermissionMode::AcceptEdits => "acceptEdits",
    }
}

/// A posture the CLI reported, back in Rimaia's vocabulary. `None` for a word
/// this provider does not know, which the caller treats as "not verifiable"
/// rather than as a mismatch.
pub fn posture_from_str(applied: &str) -> Option<PermissionMode> {
    match applied {
        "bypassPermissions" => Some(PermissionMode::BypassPermissions),
        "acceptEdits" => Some(PermissionMode::AcceptEdits),
        _ => None,
    }
}

/// Whether a parent environment variable is this provider's own process
/// identity. Case-insensitive, so the rule means the same thing on a platform
/// whose environment is not case-sensitive.
pub fn is_process_identity(name: &str) -> bool {
    name.get(..IDENTITY_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(IDENTITY_PREFIX))
}

/// How this provider spells one of Rimaia's tool names.
fn tool_handle(tool: &str) -> String {
    format!("mcp__{MCP_SERVER_NAME}__{tool}")
}

/// Rimaia's whole MCP tool surface, as this provider denies it.
///
/// Derived from [`Tool::ALL`](crate::mcp::Tool::ALL) rather than spelled out, so
/// a tool added later is denied by existing. The bare server name denies the
/// whole server where Claude Code supports it; the per-tool entries are what
/// make the denial exact if it does not.
fn rimaia_tool_surface() -> Vec<String> {
    std::iter::once(format!("mcp__{MCP_SERVER_NAME}"))
        .chain(
            crate::mcp::Tool::ALL
                .iter()
                .map(|tool| tool_handle(tool.as_str())),
        )
        .collect()
}

/// The `--disallowedTools` list for a set of operations.
///
/// Order follows the operations as the intent lists them, and duplicates are
/// dropped keeping the first occurrence — an operator whose own rule already
/// spells an operation Rimaia also asked for should not see it twice.
pub fn spell_out(forbidden: &[ForbiddenOperation]) -> Vec<String> {
    let mut denied: Vec<String> = Vec::new();

    for operation in forbidden {
        let patterns: Vec<String> = match operation {
            ForbiddenOperation::RemoteHistoryRewrite => spelled(&REMOTE_HISTORY_REWRITE),
            ForbiddenOperation::RemoteBranchDeletion => spelled(&REMOTE_BRANCH_DELETION),
            ForbiddenOperation::HardResetToRemote => spelled(&HARD_RESET_TO_REMOTE),
            ForbiddenOperation::AnyFileMutation => spelled(&ANY_FILE_MUTATION),
            ForbiddenOperation::AnyShellCommand => spelled(&ANY_SHELL_COMMAND),
            ForbiddenOperation::RimaiaToolSurface => rimaia_tool_surface(),
            // Already in this provider's vocabulary. `negotiate` has already
            // dropped anything tagged for a different one, so reaching here
            // means the rule was written for Claude Code.
            ForbiddenOperation::ProviderRule { rule, .. } => vec![rule.clone()],
        };

        for pattern in patterns {
            if !denied.contains(&pattern) {
                denied.push(pattern);
            }
        }
    }

    denied
}

fn spelled(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(|p| (*p).to_string()).collect()
}

/// The three git operations, as the operations they are.
///
/// What an unset blocklist setting means: ADR-0012 point 3's three, and nothing
/// about how any provider spells them.
pub const DEFAULT_FORBIDDEN: [ForbiddenOperation; 3] = [
    ForbiddenOperation::RemoteHistoryRewrite,
    ForbiddenOperation::RemoteBranchDeletion,
    ForbiddenOperation::HardResetToRemote,
];

/// The scoped MCP handle as an inline JSON document (seam-contract D17.4).
///
/// Inline rather than a file: `runner::process` earns its tests by pinning argv
/// byte for byte and a temp path changes every run, and there is nothing to
/// create, clean up, or leave inside a worktree where the run could stage it.
///
/// `serde_json` rather than `format!`, so a URL is escaped as JSON demands rather
/// than as this line happens to assume.
fn mcp_config_json(handle: &crate::runner::provider::RimaiaHandle) -> String {
    let server = handle.server;
    let url = handle.url.as_str();
    serde_json::json!({
        "mcpServers": { server: { "type": "http", "url": url } }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn the_default_blocklist_is_exactly_what_the_three_operations_expand_to() {
        // The constant and the expansion are two spellings of one decision, and
        // this is what stops them drifting: a pattern added to an operation
        // below without being added to the constant fails here rather than
        // silently widening every run's argv.
        assert_eq!(
            spell_out(&DEFAULT_FORBIDDEN),
            DEFAULT_DISALLOWED_TOOLS
                .iter()
                .map(|pattern| (*pattern).to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_posture_round_trips_through_the_spelling_the_cli_echoes_back() {
        for mode in [
            PermissionMode::BypassPermissions,
            PermissionMode::AcceptEdits,
        ] {
            assert_eq!(posture_from_str(posture(mode)), Some(mode));
        }
        assert_eq!(
            posture(PermissionMode::BypassPermissions),
            "bypassPermissions"
        );
        assert_eq!(posture(PermissionMode::AcceptEdits), "acceptEdits");
        // A word this provider does not know is not a mismatch; the caller
        // treats it as a posture it could not verify (ADR-0004's tolerance).
        assert_eq!(posture_from_str("plan"), None);
    }

    #[test]
    fn an_operator_rule_that_repeats_an_operation_is_not_denied_twice() {
        let denied = spell_out(&[
            ForbiddenOperation::RemoteHistoryRewrite,
            ForbiddenOperation::ProviderRule {
                provider: ProviderId::ClaudeCode,
                rule: "Bash(git push -f:*)".to_string(),
            },
        ]);

        assert_eq!(denied, REMOTE_HISTORY_REWRITE.map(str::to_string).to_vec());
    }

    #[test]
    fn every_claude_variable_is_process_identity_including_the_one_with_no_underscore() {
        for name in [
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_ENTRYPOINT",
        ] {
            assert!(is_process_identity(name), "{name} must not reach a child");
        }
        // The reason the prefix is `CLAUDE` and not `CLAUDE_`.
        assert!(is_process_identity("CLAUDECODE"));
    }

    #[test]
    fn a_variable_that_merely_mentions_claude_elsewhere_is_kept() {
        for name in [
            "PATH",
            "HOME",
            "MY_CLAUDE_NOTES",
            "ANTHROPIC_API_KEY",
            "CLAUD",
        ] {
            assert!(
                !is_process_identity(name),
                "{name} was stripped unnecessarily"
            );
        }
    }

    #[test]
    fn a_variable_name_that_is_not_ascii_does_not_panic_on_a_byte_boundary() {
        // `get(..6)` returns None rather than slicing through a multi-byte
        // character, which is the whole reason it is used instead of indexing.
        assert!(!is_process_identity("CLAÜDE_CODE"));
        assert!(!is_process_identity("🚀"));
    }
}
