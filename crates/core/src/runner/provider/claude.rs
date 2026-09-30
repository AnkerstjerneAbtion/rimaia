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

use chrono::{TimeZone, Utc};
use serde_json::Value;

use crate::db::settings::RunEnvironment;
use crate::error::Result;
use crate::mcp::MCP_SERVER_NAME;
use crate::runner::events::{
    AssistantEvent, ContentBlock, EndReason, InitEvent, McpServer, OtherEvent, ResultEvent,
    RunEvent, TokenUsage, UsageState, UsageWindow, UserEvent, WindowReopen,
};

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

    /// Parses the JSON. **Never matches on substrings.**
    ///
    /// `spike/FINDINGS.md` §3: naive substring matching on `"type":"`
    /// mis-parses, because an `assistant` event nests `"type":"message"` inside
    /// its payload and `"type":"tool_use"` inside that. Dispatch on the
    /// *top-level* `type` of a parsed document, and for a `system` event on its
    /// `subtype` — several of which (`thinking_tokens`, `vcs_state_changed`,
    /// `hook_started`, `hook_response`) appear in no `--help` output and were
    /// found only by running the thing.
    ///
    /// The only failure is a line that is not JSON at all — the condition
    /// `malformed-line.jsonl` records, and `truncated-stream.jsonl` ends on. An
    /// event whose `type` is known but whose body changed shape is still an
    /// event, and dropping it would be a worse answer than keeping it opaque.
    fn parse_line(&self, line: &str) -> std::result::Result<RunEvent, serde_json::Error> {
        serde_json::from_str(line).map(event_from_value)
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

// ---------------------------------------------------------------------------
// The wire format
// ---------------------------------------------------------------------------
//
// One shape per extractor, all returning `Option`/empty rather than erroring. A
// field that changed type under us costs that field and nothing else.

fn event_from_value(raw: Value) -> RunEvent {
    let event_type = text(&raw, "type").unwrap_or_default();
    let subtype = text(&raw, "subtype");

    match (event_type.as_str(), subtype.as_deref()) {
        ("system", Some("init")) => RunEvent::Init(init_from_value(&raw)),
        ("assistant", _) => RunEvent::Assistant(assistant_from_value(&raw)),
        ("user", _) => RunEvent::User(user_from_value(&raw)),
        ("rate_limit_event", _) => RunEvent::Usage(usage_from_value(&raw)),
        ("result", _) => RunEvent::Result(result_from_value(&raw, subtype.as_deref())),
        _ => RunEvent::Other(OtherEvent {
            event_type,
            subtype,
            raw,
        }),
    }
}

fn init_from_value(raw: &Value) -> InitEvent {
    InitEvent {
        session_id: text(raw, "session_id"),
        cwd: text(raw, "cwd"),
        model: text(raw, "model"),
        // `permissionMode` — camelCase in the stream, unlike its neighbours, and
        // normalised here so shared code never matches on the spelling.
        permission_mode: text(raw, "permissionMode")
            .as_deref()
            .and_then(posture_from_str),
        api_key_source: text(raw, "apiKeySource"),
        agent_version: text(raw, "claude_code_version"),
        tools: strings(raw, "tools"),
        mcp_servers: array(raw, "mcp_servers")
            .iter()
            .filter_map(mcp_server_from_value)
            .collect(),
    }
}

/// `None` for an entry with no name — a server we cannot name is one nothing
/// downstream could assert against anyway.
fn mcp_server_from_value(raw: &Value) -> Option<McpServer> {
    Some(McpServer {
        name: text(raw, "name")?,
        status: text(raw, "status"),
    })
}

fn assistant_from_value(raw: &Value) -> AssistantEvent {
    let message = raw.get("message");
    AssistantEvent {
        session_id: text(raw, "session_id"),
        message_id: message.and_then(|message| text(message, "id")),
        content: content_blocks(message),
    }
}

fn user_from_value(raw: &Value) -> UserEvent {
    UserEvent {
        session_id: text(raw, "session_id"),
        content: content_blocks(raw.get("message")),
    }
}

/// `rate_limit_info`, in Rimaia's vocabulary.
///
/// # The `"not allowed"` predicate, and where it now lives
///
/// `spike/FINDINGS.md` §4 is explicit that the only `status` ever observed is
/// `"allowed"` — the spike never hit a real limit, and there is no recorded
/// `usage_limit` fixture. So the rule is "the status is not `allowed`", and no
/// vocabulary is invented for the payload nobody has seen. Failing *closed* —
/// treating an unrecognised word as "carry on" — would misread a real limit as a
/// hard failure at 2am, which is the precise mistake ADR-0011 names.
///
/// The guess is exactly as load-bearing as it always was. What changed is that
/// it is now one provider's guess about one wire format, instead of a shape
/// imposed on every future provider (ADR-0026).
fn usage_from_value(raw: &Value) -> UsageWindow {
    let info = raw.get("rate_limit_info").unwrap_or(&Value::Null);

    UsageWindow {
        state: match text(info, "status").as_deref() {
            None => UsageState::Unknown,
            Some(status) if status.eq_ignore_ascii_case(RATE_LIMIT_ALLOWED) => UsageState::Allowed,
            Some(_) => UsageState::Exhausted,
        },
        // Absolute, always: `resetsAt` is epoch seconds. An epoch outside the
        // representable range reads as "no reset time known" rather than
        // panicking a run that is otherwise fine.
        reopens: integer(info, "resetsAt")
            .and_then(|epoch| Utc.timestamp_opt(epoch, 0).single())
            .map(WindowReopen::At),
        label: text(info, "rateLimitType"),
        // Not reported by this provider. Seam-contract D18: absent is absent.
        used_percent: None,
    }
}

/// The one `rate_limit_info.status` any recording has ever carried.
const RATE_LIMIT_ALLOWED: &str = "allowed";

/// The terminal vocabulary `spike/FINDINGS.md` §5 measured against Claude Code
/// 2.1.234, mapped onto Rimaia's.
///
/// | Scenario | exit | `subtype` | `terminal_reason` |
/// | --- | --- | --- | --- |
/// | Success | 0 | `success` | `completed` |
/// | Killed (SIGTERM) | 143 | `error_during_execution` | `aborted_streaming` |
/// | Turn limit | 1 | `error_max_turns` | `max_turns` |
///
/// **A killed run still emits a `result` before exiting**, and it exits 143
/// rather than by signal — so nothing anywhere treats the stream stopping, or
/// the exit code, as evidence of how a run ended.
fn result_from_value(raw: &Value, subtype: Option<&str>) -> ResultEvent {
    let terminal_reason = text(raw, "terminal_reason");
    let is_error = flag(raw, "is_error").unwrap_or(false);

    ResultEvent {
        end: end_reason(terminal_reason.as_deref(), subtype, is_error),
        num_turns: integer(raw, "num_turns"),
        total_cost_usd: number(raw, "total_cost_usd"),
        duration_ms: integer(raw, "duration_ms"),
        session_id: text(raw, "session_id"),
        stop_reason: text(raw, "stop_reason"),
        result: text(raw, "result"),
        errors: strings(raw, "errors"),
        permission_denials: array(raw, "permission_denials").to_vec(),
        usage: token_usage(raw),
        // This provider reports the window on an event of its own, not here.
        usage_window: None,
    }
}

/// Which [`EndReason`] a `result` event describes.
///
/// The conjunction on success is deliberate and is carried over verbatim:
/// `is_error: false` alone would let a renamed `terminal_reason` read as
/// success, and the wrong direction to fail in is "declare victory" — a success
/// moves the task to `in_review` and stops, where the transient default resumes
/// the session and finds the work already done.
fn end_reason(terminal_reason: Option<&str>, subtype: Option<&str>, is_error: bool) -> EndReason {
    // Before the success test, because a turn limit is `Fatal` and must not be
    // reachable by any other route.
    if terminal_reason == Some(TERMINAL_MAX_TURNS) || subtype == Some(SUBTYPE_MAX_TURNS) {
        return EndReason::TurnBudgetExhausted;
    }
    if !is_error
        && (terminal_reason == Some(TERMINAL_COMPLETED) || subtype == Some(SUBTYPE_SUCCESS))
    {
        return EndReason::Completed;
    }
    if terminal_reason == Some(TERMINAL_ABORTED_STREAMING) {
        return EndReason::Interrupted;
    }
    if is_error {
        return EndReason::Errored {
            detail: describe_ending(terminal_reason, subtype),
        };
    }
    EndReason::Unknown
}

/// The terminal words the corpus proves. Anything outside them is an ending this
/// provider cannot describe, which ADR-0011 makes transient rather than fatal.
const TERMINAL_COMPLETED: &str = "completed";
const TERMINAL_ABORTED_STREAMING: &str = "aborted_streaming";
const TERMINAL_MAX_TURNS: &str = "max_turns";
const SUBTYPE_SUCCESS: &str = "success";
const SUBTYPE_MAX_TURNS: &str = "error_max_turns";

/// The provider's own words, for the sentence a human reads at 2am. `None` when
/// it named neither, because "unknown/unknown" tells a reader nothing the class
/// has not already said.
fn describe_ending(terminal_reason: Option<&str>, subtype: Option<&str>) -> Option<String> {
    if terminal_reason.is_none() && subtype.is_none() {
        return None;
    }
    Some(format!(
        "subtype \"{subtype}\" and terminal reason \"{reason}\"",
        subtype = subtype.unwrap_or("unknown"),
        reason = terminal_reason.unwrap_or("unknown"),
    ))
}

/// The four token counts ADR-0022 persists, off the terminal event's `usage`.
///
/// Four scalars out of a much larger object, chosen by ADR-0022 and no more: the
/// recorded corpus also carries `output_tokens_details`, `server_tool_use`,
/// `cache_creation`, `service_tier`, `iterations` and `speed`, none of which any
/// decision needs. `modelUsage` is deliberately not read either — it is a
/// per-model map, and ADR-0022 asked for four numbers on a row, not a document.
fn token_usage(raw: &Value) -> TokenUsage {
    let usage = raw.get("usage").unwrap_or(&Value::Null);
    TokenUsage {
        input_tokens: integer(usage, "input_tokens"),
        output_tokens: integer(usage, "output_tokens"),
        cache_read_tokens: integer(usage, "cache_read_input_tokens"),
        cache_creation_tokens: integer(usage, "cache_creation_input_tokens"),
    }
}

fn content_block_from_value(raw: &Value) -> ContentBlock {
    // `type` here is the *block's* type, one level below the event's. This is
    // the nesting that defeats substring matching (spike §3).
    match text(raw, "type").as_deref() {
        Some("text") => ContentBlock::Text(text(raw, "text").unwrap_or_default()),
        Some("tool_use") => match (text(raw, "id"), text(raw, "name")) {
            (Some(id), Some(name)) => ContentBlock::ToolUse {
                id,
                name,
                input: raw.get("input").cloned().unwrap_or(Value::Null),
            },
            // A tool call we cannot name or correlate is not one the live view
            // can show; keep it whole rather than half-modelled.
            _ => ContentBlock::Other(raw.clone()),
        },
        Some("tool_result") => match text(raw, "tool_use_id") {
            Some(tool_use_id) => ContentBlock::ToolResult {
                tool_use_id,
                is_error: flag(raw, "is_error").unwrap_or(false),
            },
            None => ContentBlock::Other(raw.clone()),
        },
        _ => ContentBlock::Other(raw.clone()),
    }
}

fn content_blocks(message: Option<&Value>) -> Vec<ContentBlock> {
    message
        .map(|message| array(message, "content"))
        .unwrap_or_default()
        .iter()
        .map(content_block_from_value)
        .collect()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn integer(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn number(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn flag(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// The string members of `key`, skipping anything that is not a string.
fn strings(value: &Value, key: &str) -> Vec<String> {
    array(value, key)
        .iter()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn parse(line: &str) -> RunEvent {
        ClaudeProvider.parse_line(line).expect("the line is JSON")
    }

    #[test]
    fn a_line_that_is_not_json_is_the_only_parse_failure() {
        assert!(ClaudeProvider
            .parse_line("{\"type\": \"result\", \"num_tur")
            .is_err());
        assert!(ClaudeProvider.parse_line("not json at all").is_err());
    }

    #[test]
    fn an_event_type_nobody_models_keeps_its_whole_document() {
        let raw = r#"{"type":"telemetry_ping","payload":{"heartbeat":true}}"#;

        let RunEvent::Other(other) = parse(raw) else {
            panic!("an unfamiliar type must not be forced into a modelled variant");
        };
        assert_eq!(other.event_type, "telemetry_ping");
        assert_eq!(other.subtype, None);
        assert_eq!(other.raw, serde_json::from_str::<Value>(raw).unwrap());
    }

    #[test]
    fn a_system_subtype_nobody_models_is_opaque_rather_than_an_error() {
        // Spike section 3: `system` carries many subtypes and several are
        // undocumented. Only `init` is modelled; the rest keep their JSON.
        let RunEvent::Other(other) =
            parse(r#"{"type":"system","subtype":"vcs_state_changed","branch":"rimaia/x"}"#)
        else {
            panic!("an unfamiliar subtype must not be forced into `init`");
        };

        assert_eq!(other.event_type, "system");
        assert_eq!(other.subtype.as_deref(), Some("vcs_state_changed"));
        assert_eq!(other.raw["branch"], json!("rimaia/x"));
    }

    #[test]
    fn an_event_whose_body_changed_shape_still_arrives_with_the_fields_that_did_not() {
        // The tolerance rule at field granularity: `tools` became objects and
        // `num_turns` a string, and neither costs the event or its neighbours.
        let RunEvent::Init(init) = parse(
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-5","tools":[{"name":"Read"}]}"#,
        ) else {
            panic!("expected an init event");
        };
        assert_eq!(init.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(init.tools, Vec::<String>::new());

        let RunEvent::Result(result) =
            parse(r#"{"type":"result","subtype":"success","num_turns":"five"}"#)
        else {
            panic!("expected a result event");
        };
        assert_eq!(result.end, EndReason::Completed);
        assert_eq!(result.num_turns, None);
    }

    #[test]
    fn an_mcp_server_entry_without_a_name_is_dropped_rather_than_named_empty() {
        let RunEvent::Init(init) = parse(
            r#"{"type":"system","subtype":"init","mcp_servers":[{"status":"connected"},{"name":"Brewale","status":"connected"}]}"#,
        ) else {
            panic!("expected an init event");
        };

        assert_eq!(
            init.mcp_servers,
            vec![McpServer {
                name: "Brewale".to_string(),
                status: Some("connected".to_string()),
            }]
        );
    }

    #[test]
    fn a_rate_limit_epoch_outside_the_representable_range_reads_as_no_reset_time() {
        // The scheduler waits until this instant plus jitter, so an epoch the
        // CLI reports nonsensically must read as "no reset time known" rather
        // than panic a run that is otherwise fine.
        let RunEvent::Usage(window) = parse(&format!(
            r#"{{"type":"rate_limit_event","rate_limit_info":{{"status":"allowed","resetsAt":{}}}}}"#,
            i64::MAX
        )) else {
            panic!("expected a usage event");
        };

        assert_eq!(window.reopens, None);
        assert_eq!(window.state, UsageState::Allowed);
    }

    #[test]
    fn any_status_but_allowed_is_a_wall_whatever_the_word_turns_out_to_be() {
        // `spike/FINDINGS.md` §4: the non-`allowed` payload was never observed,
        // so the rule is "not allowed" and the words below are chosen to be
        // obviously invented. Failing the other way — treating an unrecognised
        // word as "carry on" — misreads a real limit as a hard failure at 2am,
        // which is the precise mistake ADR-0011 names.
        for status in ["rejected", "limited", "blocked", "a word nobody has seen"] {
            let RunEvent::Usage(window) = parse(&format!(
                r#"{{"type":"rate_limit_event","rate_limit_info":{{"status":"{status}"}}}}"#
            )) else {
                panic!("expected a usage event");
            };
            assert_eq!(window.state, UsageState::Exhausted, "{status:?}");
        }

        // And the one word the corpus does prove still means "carry on".
        let RunEvent::Usage(allowed) =
            parse(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#)
        else {
            panic!("expected a usage event");
        };
        assert_eq!(allowed.state, UsageState::Allowed);

        // A window this provider could not read at all is `Unknown`, which is
        // not a wall (ADR-0026 point 8).
        let RunEvent::Usage(silent) = parse(r#"{"type":"rate_limit_event"}"#) else {
            panic!("expected a usage event");
        };
        assert_eq!(silent.state, UsageState::Unknown);
    }

    #[test]
    fn the_three_terminal_signatures_the_spike_measured_map_onto_three_endings() {
        // `spike/FINDINGS.md` §5's table, as the mapping it now is. A killed run
        // still emits a terminal event and exits 143, so none of this is read
        // off an exit code.
        for (line, expected) in [
            (
                r#"{"type":"result","subtype":"success","terminal_reason":"completed","is_error":false}"#,
                EndReason::Completed,
            ),
            (
                r#"{"type":"result","subtype":"error_during_execution","terminal_reason":"aborted_streaming","is_error":true}"#,
                EndReason::Interrupted,
            ),
            (
                r#"{"type":"result","subtype":"error_max_turns","terminal_reason":"max_turns","is_error":true}"#,
                EndReason::TurnBudgetExhausted,
            ),
        ] {
            let RunEvent::Result(result) = parse(line) else {
                panic!("expected a result event");
            };
            assert_eq!(result.end, expected, "in {line}");
        }
    }

    #[test]
    fn a_completed_run_that_also_reports_an_error_is_not_a_success() {
        // The conjunction that used to live in the classifier: `is_error: false`
        // alone would let a renamed terminal reason read as success, and the
        // wrong direction to fail in is "declare victory".
        let RunEvent::Result(result) =
            parse(r#"{"type":"result","terminal_reason":"completed","is_error":true}"#)
        else {
            panic!("expected a result event");
        };

        assert_eq!(
            result.end,
            EndReason::Errored {
                detail: Some("subtype \"unknown\" and terminal reason \"completed\"".to_string()),
            }
        );
    }

    #[test]
    fn a_terminal_word_this_provider_has_never_seen_is_unknown_rather_than_a_failure() {
        // ADR-0011 makes an ending nobody can describe transient, not fatal — so
        // a CLI update that renames a terminal reason costs a retry, never a
        // queue.
        let RunEvent::Result(result) =
            parse(r#"{"type":"result","terminal_reason":"vacated","is_error":false}"#)
        else {
            panic!("expected a result event");
        };

        assert_eq!(result.end, EndReason::Unknown);
    }

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
