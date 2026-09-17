//! Ledger: a provider that does not exist, so the seam can be falsified
//! (ADR-0026, seam-contract D27.6).
//!
//! # Why it is fictional, and why that is the point
//!
//! Shipping a *real* second provider and stubbing what does not work would make
//! a claim: the next person and the UI would read it as "that provider is
//! supported". A deliberately invented one makes the same claim honestly — it is
//! **evidence about Rimaia's seam and nothing else** — which is the discipline
//! `SYNTHESIZED_UNOBSERVED` already enforces on the fixture corpus. It is not
//! named after any product for exactly that reason: a name would invite a later
//! agent to read its fixtures as evidence about that product.
//!
//! # What it is for
//!
//! It differs from Claude Code on **every axis the seam claims to abstract**,
//! because a double that agrees on an axis proves nothing about that axis:
//!
//! | Axis | Claude Code | Ledger |
//! | --- | --- | --- |
//! | Non-interactive | `-p` flag | `run` subcommand |
//! | Stream | `--output-format stream-json --verbose` | `--emit ndjson` |
//! | Session | pre-minted `--session-id` | self-minted, announced; resume is `--continue` |
//! | Posture | `--permission-mode bypassPermissions` | `--trust full` |
//! | Orchestrator facts | `--append-system-prompt <text>` | a file in the scratch directory |
//! | Rimaia handle | `--mcp-config <inline JSON>` | `LEDGER_HOME` with a `tools.toml` inside — no flag at all |
//! | Tool denial | `--disallowedTools <rules…>` | **none** |
//! | Turn budget | `--max-turns N` | `--step-budget N` |
//! | Discriminator | top-level `type` | top-level `kind`, payload under `body` |
//! | Terminal event | `result` + `terminal_reason` + `subtype` | `finished` + `why` |
//! | Usage | a separate event, absolute epoch | on every turn *and* the ending, **relative** |
//! | Cost | `total_cost_usd` | **absent** — tokens only |
//! | Identity env | prefix `CLAUDE` | prefixes `LEDGER` **and** `LGR_` |
//!
//! The last two rows are deliberate extra pressure: an absent cost forces
//! `RunOutcome::cost_usd` to mean seam-contract D18's "not recorded" all the way
//! to analytics, and two prefixes break any fix that assumes one string.
//!
//! # What it does not vary, and why
//!
//! Prompt delivery is stdin-then-close for both. That is the mechanism
//! `spike/FINDINGS.md` §7 measured as the thing that hangs a run, it is shared
//! machinery by definition, and varying it buys no evidence about the seam.

use std::path::Path;

use serde_json::Value;

use crate::error::Result;
use crate::runner::events::{
    AssistantEvent, ContentBlock, EndReason, InitEvent, OtherEvent, ResultEvent, RunEvent,
    TokenUsage, UsageState, UsageWindow, UserEvent, WindowReopen,
};
use crate::runner::provider::{
    parse_version, AgentProvider, AuthState, Capabilities, HandleInjection, IsolationSupport,
    OrchestratorChannel, PermissionMode, PostureEcho, ProbeOutput, ProviderId, ResumeStyle,
    RunIntent, SessionCapability, SpawnPlan, TurnBudget, Version, VersionReport,
};
use crate::strategy::{Catalogue, CatalogueEntry, PlannerBudget};

/// What the operator would type. Never resolved through a real `PATH`.
pub const LEDGER_CLI: &str = "ledger";

/// The name a doctor row or an error sentence would use, if this provider were
/// ever real. Deliberately not a product anyone could confuse for one.
pub const DISPLAY_NAME: &str = "Ledger";

/// A fictional minimum, chosen only to be obviously not Claude's.
pub const MINIMUM_VERSION: Version = (0, 3, 0);

/// Where this provider keeps its configuration, its conversations, and — when
/// Rimaia hands it one — its MCP servers.
///
/// One variable doing all three jobs is what makes handle injection and
/// isolation the *same* decision for this provider, where for Claude Code they
/// are two independent flags.
pub const LEDGER_HOME: &str = "LEDGER_HOME";

/// Set when Rimaia did **not** take the configuration home over, so the operator's
/// own configuration is still read.
pub const LEDGER_INHERIT: &str = "LEDGER_INHERIT";

/// Rimaia's scoped handle, as a file this provider reads out of its home.
pub const TOOLS_FILE: &str = "tools.toml";

/// The orchestrator facts, as a file beside the run rather than inside it.
pub const PREAMBLE_FILE: &str = "preamble.md";

/// Both of this provider's identity prefixes.
///
/// **Two**, deliberately. A stripping rule written as one string passes every
/// Claude test and leaks one of these.
const IDENTITY_PREFIXES: &[&str] = &["LEDGER", "LGR_"];

pub static CAPABILITIES: &Capabilities = &Capabilities {
    id: ProviderId::Ledger,
    session: SessionCapability::SelfMinted {
        resume: ResumeStyle::LastInHome,
    },
    orchestrator_channel: OrchestratorChannel::HandedFile {
        inside_workspace: false,
    },
    handle_injection: HandleInjection::ConfigHome { env: LEDGER_HOME },
    isolation: IsolationSupport::InheritExceptWhenHandleInjected,
    turn_budget: TurnBudget::ProviderEnforced {
        flag: "--step-budget",
    },
    posture_echo: PostureEcho::Silent,
    // **The row the whole design is validated against.** This provider has
    // sandbox modes and no deny list, so it cannot express any of ADR-0012 point
    // 3 — and an unattended run on it is therefore refused rather than degraded.
    enforceable: &[],
    identity_prefixes: IDENTITY_PREFIXES,
};

/// The same provider with its ability to continue a conversation taken away.
///
/// The only way to reach ADR-0026 point 6's other arm: a resume against a
/// provider that cannot resume must be sent the **composed** prompt, because a
/// one-line continuation delivered into a fresh session produces an agent with
/// no plan, no context and an empty diff.
pub static CAPABILITIES_WITHOUT_RESUME: &Capabilities = &Capabilities {
    session: SessionCapability::SelfMinted {
        resume: ResumeStyle::Unsupported,
    },
    ..*CAPABILITIES
};

/// Every capability table this provider contributes to the identity-prefix union
/// (seam-contract D27.5).
pub const ALL_CAPABILITIES: &[&Capabilities] = &[CAPABILITIES, CAPABILITIES_WITHOUT_RESUME];

/// The provider itself. Zero-sized, like every other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ledger;

/// [`Ledger`], minus `--continue`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LedgerWithoutResume;

impl AgentProvider for Ledger {
    fn id(&self) -> ProviderId {
        ProviderId::Ledger
    }

    fn default_program(&self) -> &'static str {
        LEDGER_CLI
    }

    fn capabilities(&self) -> &'static Capabilities {
        CAPABILITIES
    }

    fn plan_spawn(&self, intent: &RunIntent<'_>, scratch: &Path) -> Result<SpawnPlan> {
        plan_spawn(intent, scratch, true)
    }

    fn parse_line(&self, line: &str) -> std::result::Result<RunEvent, serde_json::Error> {
        serde_json::from_str(line).map(event_from_value)
    }

    fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    fn version_probe(&self, program: &Path) -> SpawnPlan {
        version_probe(program)
    }

    fn read_version(&self, output: &ProbeOutput) -> VersionReport {
        read_version(output)
    }

    fn auth_probe(&self, program: &Path) -> Option<SpawnPlan> {
        auth_probe(program)
    }

    fn read_auth(&self, output: &ProbeOutput) -> AuthState {
        read_auth(output)
    }

    fn minimum_version(&self) -> Version {
        MINIMUM_VERSION
    }

    fn tool_handle(&self, server: &str, tool: &str) -> String {
        tool_handle(server, tool)
    }

    fn fanout_noun(&self) -> &'static str {
        "helpers"
    }

    fn inherit_cost_usd(&self) -> Option<f64> {
        None
    }

    fn default_catalogue(&self) -> Catalogue {
        default_catalogue()
    }
}

impl AgentProvider for LedgerWithoutResume {
    fn id(&self) -> ProviderId {
        ProviderId::Ledger
    }

    fn default_program(&self) -> &'static str {
        LEDGER_CLI
    }

    fn capabilities(&self) -> &'static Capabilities {
        CAPABILITIES_WITHOUT_RESUME
    }

    fn plan_spawn(&self, intent: &RunIntent<'_>, scratch: &Path) -> Result<SpawnPlan> {
        plan_spawn(intent, scratch, false)
    }

    fn parse_line(&self, line: &str) -> std::result::Result<RunEvent, serde_json::Error> {
        serde_json::from_str(line).map(event_from_value)
    }

    fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    fn version_probe(&self, program: &Path) -> SpawnPlan {
        version_probe(program)
    }

    fn read_version(&self, output: &ProbeOutput) -> VersionReport {
        read_version(output)
    }

    fn auth_probe(&self, program: &Path) -> Option<SpawnPlan> {
        auth_probe(program)
    }

    fn read_auth(&self, output: &ProbeOutput) -> AuthState {
        read_auth(output)
    }

    fn minimum_version(&self) -> Version {
        MINIMUM_VERSION
    }

    fn tool_handle(&self, server: &str, tool: &str) -> String {
        tool_handle(server, tool)
    }

    fn fanout_noun(&self) -> &'static str {
        "helpers"
    }

    fn inherit_cost_usd(&self) -> Option<f64> {
        None
    }

    fn default_catalogue(&self) -> Catalogue {
        default_catalogue()
    }
}

/// `ledger --version`. Unlike every other axis this provider varies
/// deliberately (see this module's header table), the version probe shares
/// Claude's flag on purpose: `crates/core/tests/fixtures/cli.rs`'s stand-in
/// answers exactly one version flag for every provider it plays, and a
/// fictional provider inventing a second one would need to teach the harness
/// a subcommand no real CLI asked for. What proves the seam here is the
/// *string* — `"0.9.4 (ledger)"`, nothing like Claude's — parsed by the same
/// generic [`parse_version`], not the flag that requested it.
fn version_probe(_program: &Path) -> SpawnPlan {
    SpawnPlan {
        args: vec!["--version".to_string()],
        ..SpawnPlan::default()
    }
}

fn read_version(output: &ProbeOutput) -> VersionReport {
    let raw = output.stdout.trim().to_string();
    VersionReport {
        parsed: parse_version(&raw),
        raw,
    }
}

/// `None`: this fictional provider's sign-in genuinely cannot be checked out
/// of band, which is the row task 032 exists to let a doctor omit rather than
/// fake.
fn auth_probe(_program: &Path) -> Option<SpawnPlan> {
    None
}

/// Never reached in practice — [`auth_probe`] is always `None` — but the
/// trait still requires an answer.
fn read_auth(_output: &ProbeOutput) -> AuthState {
    AuthState::Undetermined {
        detail: "Ledger has no out-of-band sign-in check".to_string(),
    }
}

/// This provider's own tool-naming convention — a `.` rather than Claude's
/// `mcp__server__tool` double underscore, so a fix that assumes one provider's
/// punctuation fails here first.
fn tool_handle(server: &str, tool: &str) -> String {
    format!("{server}.{tool}")
}

/// Deliberately not Claude's models or effort words, so a fix that assumes
/// `opus`/`sonnet`/`haiku` fails here first.
fn default_catalogue() -> Catalogue {
    Catalogue {
        models: vec![
            CatalogueEntry {
                id: "steady".to_string(),
                label: "Steady".to_string(),
            },
            CatalogueEntry {
                id: "swift".to_string(),
                label: "Swift".to_string(),
            },
        ],
        efforts: vec![
            CatalogueEntry {
                id: "careful".to_string(),
                label: "Careful".to_string(),
            },
            CatalogueEntry {
                id: "quick".to_string(),
                label: "Quick".to_string(),
            },
        ],
        planner: PlannerBudget {
            model: Some("steady".to_string()),
            effort: Some("careful".to_string()),
            ..PlannerBudget::default()
        },
    }
}

/// A subcommand, a file and an environment variable — not one flag Claude Code
/// would recognise.
///
/// `required_tools` reaches the child through the `tools.toml` written below
/// rather than through an allow-list argument: this provider has no such flag,
/// which is exactly why the intent names Rimaia's tools rather than one
/// provider's spelling of them.
fn plan_spawn(intent: &RunIntent<'_>, scratch: &Path, can_continue: bool) -> Result<SpawnPlan> {
    let mut args = vec![
        "run".to_string(),
        "--emit".to_string(),
        "ndjson".to_string(),
    ];

    if can_continue && intent.session.is_continuation() {
        // No id anywhere: this provider minted its own and knows which one was
        // last in its home (ADR-0026 point 5).
        args.push("--continue".to_string());
    }

    args.push("--trust".to_string());
    args.push(
        match intent.permission_mode {
            PermissionMode::BypassPermissions => "full",
            PermissionMode::AcceptEdits => "edits-only",
        }
        .to_string(),
    );

    // Out of the workspace, so the agent cannot rewrite the constraints it is
    // being run under — the difference between `HandedFile { inside_workspace }`
    // being allowed with a warning and being refused outright.
    let preamble = scratch.join(PREAMBLE_FILE);
    std::fs::write(&preamble, &intent.system_append)?;
    args.push("--preamble-file".to_string());
    args.push(preamble.display().to_string());

    if let Some(model) = &intent.model {
        args.push("--engine".to_string());
        args.push(model.clone());
    }
    if let Some(effort) = &intent.effort {
        args.push("--diligence".to_string());
        args.push(effort.clone());
    }
    if let Some(max_turns) = intent.max_turns {
        args.push("--step-budget".to_string());
        args.push(max_turns.to_string());
    }

    let home = intent.session.home();
    std::fs::create_dir_all(home)?;
    let mut env_set = vec![(LEDGER_HOME.to_string(), home.display().to_string())];

    match &intent.rimaia_handle {
        // Taking the configuration home over is what injects the handle *and*
        // what isolates the run. One decision, not two.
        Some(handle) => {
            let tools = format!(
                "[servers.{server}]\nkind = \"http\"\nurl = \"{url}\"\ntools = [{allowed}]\n",
                server = handle.server,
                url = handle.url,
                allowed = intent
                    .required_tools
                    .iter()
                    .map(|tool| format!("\"{tool}\""))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            std::fs::write(home.join(TOOLS_FILE), tools)?;
        }
        None => env_set.push((LEDGER_INHERIT.to_string(), "1".to_string())),
    }

    Ok(SpawnPlan {
        args,
        env_set,
        env_remove: Vec::new(),
        stdin: intent.prompt.to_string(),
    })
}

// ---------------------------------------------------------------------------
// The wire format
// ---------------------------------------------------------------------------
//
// `kind` at the top level, everything else nested under `body`. Sharing no
// vocabulary with the other corpus is the point: a shared-code path that reads
// `type` or `terminal_reason` produces `Other` here and fails a test rather than
// quietly working.

fn event_from_value(raw: Value) -> RunEvent {
    let kind = raw
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let body = raw.get("body").cloned().unwrap_or(Value::Null);

    match kind.as_str() {
        "conversation.opened" => RunEvent::Init(InitEvent {
            session_id: string(&body, "conversation"),
            cwd: string(&body, "cwd"),
            model: string(&body, "engine"),
            // `PostureEcho::Silent`: this provider never says what it applied,
            // which is a fact recorded on the run rather than a reason to refuse
            // it.
            permission_mode: None,
            api_key_source: None,
            agent_version: string(&body, "version"),
            tools: Vec::new(),
            mcp_servers: Vec::new(),
        }),
        "agent.said" => RunEvent::Assistant(AssistantEvent {
            session_id: string(&body, "conversation"),
            message_id: string(&body, "turn"),
            content: vec![ContentBlock::Text(
                string(&body, "text").unwrap_or_default(),
            )],
        }),
        "agent.tool" => RunEvent::Assistant(AssistantEvent {
            session_id: string(&body, "conversation"),
            message_id: string(&body, "turn"),
            content: vec![match (string(&body, "call"), string(&body, "tool")) {
                (Some(id), Some(name)) => ContentBlock::ToolUse {
                    id,
                    name,
                    input: body.get("args").cloned().unwrap_or(Value::Null),
                },
                _ => ContentBlock::Other(body.clone()),
            }],
        }),
        "tool.returned" => RunEvent::User(UserEvent {
            session_id: string(&body, "conversation"),
            content: vec![match string(&body, "call") {
                Some(tool_use_id) => ContentBlock::ToolResult {
                    tool_use_id,
                    is_error: body.get("failed").and_then(Value::as_bool).unwrap_or(false),
                },
                None => ContentBlock::Other(body.clone()),
            }],
        }),
        "window" => RunEvent::Usage(window_from_value(&body)),
        "finished" => RunEvent::Result(ResultEvent {
            end: match string(&body, "why").as_deref() {
                Some("ok") => EndReason::Completed,
                Some("stopped") => EndReason::Interrupted,
                Some("budget") => EndReason::TurnBudgetExhausted,
                Some("declined") => EndReason::Refused,
                Some("window_closed") => EndReason::Errored {
                    detail: Some("the usage window closed mid-run".to_string()),
                },
                // A word this provider has not been taught. ADR-0004's
                // tolerance: transient, never fatal.
                _ => EndReason::Unknown,
            },
            num_turns: body.get("turns").and_then(Value::as_i64),
            // **Absent by design.** This provider reports tokens and never a
            // dollar figure, so seam-contract D18's "not recorded" has to survive
            // all the way to the analytics column.
            total_cost_usd: None,
            duration_ms: body.get("ms").and_then(Value::as_i64),
            session_id: string(&body, "conversation"),
            stop_reason: string(&body, "why"),
            result: string(&body, "summary"),
            errors: body
                .get("errors")
                .and_then(Value::as_array)
                .map(|errors| {
                    errors
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            permission_denials: Vec::new(),
            usage: TokenUsage {
                input_tokens: body.pointer("/tokens/in").and_then(Value::as_i64),
                output_tokens: body.pointer("/tokens/out").and_then(Value::as_i64),
                cache_read_tokens: body.pointer("/tokens/cached").and_then(Value::as_i64),
                cache_creation_tokens: None,
            },
            // The window rides on the ending too, which is the shape that makes
            // latching matter: a `finished` reporting `open` must not clear a
            // wall an earlier turn reported.
            usage_window: body.get("window").map(window_from_value),
        }),
        _ => RunEvent::Other(OtherEvent {
            event_type: kind,
            subtype: string(&body, "why"),
            raw,
        }),
    }
}

/// `{"used_pct":93.5,"window":"5h","reopens_in_s":3600,"state":"closed"}`.
///
/// **Relative**, which is the half of ADR-0026 point 7 the other corpus cannot
/// exercise: `reopens_in_s` means "from the moment you read this line", so the
/// instant it names depends on when it was read and on nothing else.
fn window_from_value(body: &Value) -> UsageWindow {
    UsageWindow {
        state: match string(body, "state").as_deref() {
            Some("open") => UsageState::Allowed,
            Some("closed") => UsageState::Exhausted,
            // A percentage and no state is not a wall (ADR-0026 point 8).
            _ => UsageState::Unknown,
        },
        reopens: body
            .get("reopens_in_s")
            .and_then(Value::as_i64)
            .map(|seconds| WindowReopen::After(chrono::Duration::seconds(seconds))),
        label: string(body, "window"),
        used_percent: body.get("used_pct").and_then(Value::as_f64),
    }
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}
