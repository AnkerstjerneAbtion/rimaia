//! The `stream-json` event stream: parsed, persisted, and tailed live
//! (ADR-0004, ADR-0013, seam-contract D14).
//!
//! Four jobs, deliberately in one module because they all consume the same
//! line and would otherwise each have to re-read it:
//!
//! 1. **Parse** it into [`RunEvent`]. Tolerantly — a malformed line is skipped,
//!    an unmodelled event is kept whole as [`RunEvent::Other`], and neither is
//!    ever fatal. A Claude Code update must not break an overnight queue.
//! 2. **Persist** it verbatim to `<app-data>/runs/<task-id>/<run-id>.jsonl`
//!    ([`Transcript`]), and the child's stderr beside it ([`StderrLog`]).
//! 3. **Fold** it into a bounded [`RunProgress`] — the ring buffer a client
//!    reads to catch up when it starts watching mid-run.
//! 4. **Publish** a [`RunTail`] snapshot on the channel D14 gives it.
//!
//! # Parse the JSON. Do not match on substrings
//!
//! `spike/FINDINGS.md` §3: naive substring matching on `"type":"` mis-parses,
//! because an `assistant` event nests `"type":"message"` inside its payload and
//! `"type":"tool_use"` inside that. Dispatch on the *top-level* `type` of a
//! parsed document, and for a `system` event on its `subtype` — several of
//! which (`thinking_tokens`, `vcs_state_changed`, `hook_started`,
//! `hook_response`) appear in no `--help` output and were found only by running
//! the thing.
//!
//! # What is modelled, and what is not
//!
//! `system`/`init`, `assistant`, `user`, `result` and `rate_limit_event` are
//! typed, because something downstream reads a named field off each: the
//! permission mode and isolation actually applied (task 008 verifies them
//! against what it asked for), the live view's tool call and text, the
//! classifier's `terminal_reason`, the scheduler's usage-limit reset. Everything
//! else keeps its whole `serde_json::Value` and its position in the stream, so
//! tolerating an event never means discarding one.
//!
//! Every payload field is optional and every extractor is fallible-into-`None`.
//! An event whose shape changed under us degrades to missing fields, never to a
//! parse failure that would take the line — and with it the transcript's
//! evidence — down with it.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::error::Result;
use crate::events::RunId;
use crate::paths::AppPaths;
use crate::runner::provider::{AgentProvider, ClaudeProvider, PermissionMode};

/// How many unread [`RunTail`] snapshots the channel keeps for a receiver that
/// falls behind (seam-contract D14).
///
/// The same number ADR-0018 gives the change channel, on a channel that is
/// deliberately *not* that one. The reason for the separation is frequency: the
/// tail fires many times per turn and `ChangeEvent` once per committed
/// mutation, so sharing a buffer would let a chatty run lag a subscriber into
/// dropping change events — and a dropped change event costs a card that stops
/// refreshing. A dropped tail costs a line of scrollback that is already on disk
/// in the transcript, which is why D14 says to count it and move on.
pub const TAIL_CHANNEL_CAPACITY: usize = 256;

/// How many lines of recent activity [`RunProgress`] keeps for a client that
/// starts watching mid-run.
///
/// Bounded because this lives in a process that runs all night: an unbounded
/// "recent activity" list is the whole transcript in memory, and the transcript
/// is already on disk where task 015 can page it.
pub const RECENT_ACTIVITY_CAPACITY: usize = 64;

/// The longest assistant text one activity line keeps.
///
/// The tail is a view, not the record (D14 rule 2) — a client wanting the whole
/// message reads the JSONL. Capping here is what stops one verbose turn from
/// pinning several megabytes into the ring buffer and into every broadcast
/// clone of a snapshot.
const MAX_ACTIVITY_CHARS: usize = 2_000;

/// The longest [`ToolCall::detail`] rendered from a tool's input. Shorter than
/// [`MAX_ACTIVITY_CHARS`] because a `Write` call's input is an entire file.
const MAX_TOOL_DETAIL_CHARS: usize = 200;

/// Appended when [`clamp`] had to cut.
const TRUNCATION_MARKER: char = '…';

/// What the CLI writes into a tool result it refused for want of approval.
///
/// A phrase, reluctantly, and only ever behind a typed gate: see
/// [`reports_a_permission_denial`] for why there is no field to read instead
/// and what this is allowed to be used for.
const PERMISSION_DENIAL_MARKER: &str = "requires approval";

/// Whether `raw_line` is a tool result the CLI refused because nothing could
/// approve it — the shape of a run that spent an hour being told no.
///
/// The typed gate comes first: only a `user` event carrying a `tool_result`
/// block with `is_error` is considered at all. The phrase is what separates a
/// refusal from an ordinary tool failure, and there is no typed field that
/// does — [`ResultEvent::permission_denials`] carries them, but only on a run
/// that reached a `result`, and a run refused into giving up may never reach
/// one.
///
/// **Diagnostics and display only.** Nothing classifies on this: ADR-0004's
/// rule is that the terminal vocabulary decides an outcome, and a CLI update
/// rewording this sentence must cost a count in a summary, never a
/// misclassified run.
pub fn reports_a_permission_denial(event: &RunEvent, raw_line: &str) -> bool {
    let RunEvent::User(user) = event else {
        return false;
    };
    user.content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { is_error: true, .. }))
        && line_reports_a_denial(raw_line)
}

/// The phrase half of [`reports_a_permission_denial`], on its own, for a
/// caller that has already applied its own typed gate.
///
/// `runs::transcript` is that caller: it reads finished transcripts with its
/// own parser (deliberately separate — see that module's header), and this is
/// what keeps the CLI's wording written down in exactly one place rather than
/// copied into a second module that would then drift.
pub fn line_reports_a_denial(raw_line: &str) -> bool {
    raw_line.contains(PERMISSION_DENIAL_MARKER)
}

// ---------------------------------------------------------------------------
// The event model
// ---------------------------------------------------------------------------

/// One event off the CLI's stdout.
///
/// [`Other`](RunEvent::Other) is not an error case. It is how ADR-0004's
/// tolerant-parsing rule is expressed in the type: an event this version of
/// Rimaia has no use for still arrives, still carries its whole JSON, and still
/// occupies its place in the stream.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Init(InitEvent),
    Assistant(AssistantEvent),
    User(UserEvent),
    Usage(UsageWindow),
    Result(ResultEvent),
    Other(OtherEvent),
}

/// The applied configuration, echoed back (ADR-0004).
///
/// The runner asks for a permission mode and an isolation posture; this is the
/// CLI reporting what it actually did with them. Verifying rather than assuming
/// is a cheap guard against a CLI change silently widening permissions, and the
/// `env-leak-*` fixtures are the two ends of the isolation measurement: 255
/// tools and 2 MCP servers inherited, 26 and 0 isolated.
///
/// Every field is optional because a missing one must not cost the event. What
/// an absent `permission_mode` *means* is the spawning code's call, not this
/// module's — it has the requested mode to compare against and this does not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InitEvent {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    /// The posture the provider says it applied, **typed**.
    ///
    /// Typed rather than a string so the provider normalises its own spelling
    /// and shared code compares two values (ADR-0026 point 3). `None` covers
    /// both "reported nothing" and "reported a word this provider does not
    /// know" — which are the same thing to a caller that can only say "it could
    /// not be verified".
    pub permission_mode: Option<PermissionMode>,
    /// `"none"` confirms subscription auth rather than a metered API key, which
    /// is ADR-0004's premise.
    pub api_key_source: Option<String>,
    /// The agent CLI's own version, as it announced it.
    pub agent_version: Option<String>,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<McpServer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub status: Option<String>,
}

/// `assistant` — one model message, or a fragment of one.
///
/// Several events share a `message_id`: the CLI emits a thinking block, then a
/// tool call, as separate events off the same message. That id is the only
/// thing tying them together, which is why it is kept.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantEvent {
    pub session_id: Option<String>,
    pub message_id: Option<String>,
    pub content: Vec<ContentBlock>,
}

/// `user` — tool results being fed back to the model. Not a human.
#[derive(Debug, Clone, PartialEq)]
pub struct UserEvent {
    pub session_id: Option<String>,
    pub content: Vec<ContentBlock>,
}

/// A block inside a message's `content` array.
///
/// The nesting spike §3 warns about lives here: these carry their own `type`,
/// and it is *not* the event's type. `tool_result` content is deliberately
/// dropped rather than modelled — a single `Read` result can be a whole file,
/// and the transcript already has it.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        is_error: bool,
    },
    /// `thinking`, and whatever a CLI update adds next.
    Other(Value),
}

/// What the provider last said about the usage window (ADR-0011's amendment,
/// ADR-0026 point 7 and point 8).
///
/// Arrives unprompted on a healthy run, not only on failure, which is what lets
/// the scheduler read limit state before committing to a long task. Do not grep
/// an error message for this.
///
/// # Three states, and why `Unknown` is not a wall
///
/// A provider that reports a percentage on every turn must not have "93% used"
/// read as a limit — that would raise ADR-0011's **global** pause on a perfectly
/// healthy run. So a window is a wall only when the provider says it is
/// [`Exhausted`](UsageState::Exhausted), and everything a provider says that
/// Rimaia cannot interpret is [`Unknown`](UsageState::Unknown), which is not a
/// wall either.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageWindow {
    pub state: UsageState,
    /// When the window reopens, as the line reported it.
    pub reopens: Option<WindowReopen>,
    /// What the provider calls this window, e.g. `five_hour`. Carried for the
    /// message a human reads, never for a decision.
    pub label: Option<String>,
    /// How much of the window is spent, where a provider says. **Never a
    /// classification input** — see this type's header.
    pub used_percent: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageState {
    /// The provider said this run may proceed.
    Allowed,
    /// The provider said it may not. ADR-0011's wall.
    Exhausted,
    /// The provider said something, and it was not either of the above.
    ///
    /// The default, and deliberately the *safe* one: a run whose window nobody
    /// could read is a run that carries on.
    #[default]
    Unknown,
}

/// When a closed window reopens, as one line reported it.
///
/// Two shapes because providers genuinely report two: an absolute instant, and a
/// duration relative to the moment it spoke. Resolving a relative one later —
/// at `finish_run`, say — would be wrong by however long the run then took to
/// die, in the direction that wastes a night.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowReopen {
    At(DateTime<Utc>),
    After(Duration),
}

/// One [`UsageWindow`] and the instant the clock said when its line was read.
///
/// The pairing is the whole of ADR-0026 point 7: the duration is what the
/// provider said, `observed_at` is when it said it, and
/// [`reopens_at`](Self::reopens_at) is the one place the two become an instant.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageReport {
    pub window: UsageWindow,
    pub observed_at: DateTime<Utc>,
}

impl UsageReport {
    /// The instant this window reopens, or `None` when the provider named none.
    ///
    /// The scheduler waits until this plus jitter (ADR-0011), so a reported
    /// instant outside the representable range reads as "no reset time known"
    /// rather than panicking a run that is otherwise fine — which is also what
    /// `usage_limit_without_reset_time_falls_back_to_fixed_poll` then does with
    /// it.
    pub fn reopens_at(&self) -> Option<DateTime<Utc>> {
        match self.window.reopens? {
            WindowReopen::At(instant) => Some(instant),
            WindowReopen::After(duration) => self.observed_at.checked_add_signed(duration),
        }
    }

    /// Whether the provider reported a wall. `Unknown` is not one (point 8).
    pub fn hit_a_wall(&self) -> bool {
        self.window.state == UsageState::Exhausted
    }
}

/// The terminal event, which arrives even when the run was killed.
///
/// `spike/FINDINGS.md` §5: a SIGTERM-killed run emits this and *then* exits 143.
/// The stream does not simply stop, so classification reads what the run *said*
/// and never the exit code alone. Everything the `runs` row needs is already
/// here — turns, cost, duration — with nothing to derive.
///
/// Every number is `Option` and stays so: a provider that reports no dollar
/// figure means seam-contract D18's "not recorded", never zero.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultEvent {
    /// Why the run stopped, in Rimaia's vocabulary (ADR-0026 point 3). The
    /// provider normalises its own terminal words into this; what each one is
    /// *worth* is [`classify`](crate::runner::outcome::classify)'s, and stays
    /// Rimaia's policy.
    pub end: EndReason,
    pub num_turns: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub duration_ms: Option<i64>,
    /// The id the *provider* announced for this conversation. Carried for a
    /// provider that resumes by id and has to recover it from a transcript;
    /// **never persisted** — `runs.session_id` is Rimaia's own (ADR-0026 point
    /// 5).
    pub session_id: Option<String>,
    pub stop_reason: Option<String>,
    /// The agent's own closing summary. Present on a clean finish, absent on the
    /// endings that carry [`errors`](Self::errors) instead.
    pub result: Option<String>,
    pub errors: Vec<String>,
    /// Kept whole: nothing reads its shape yet, and ADR-0012 makes a denial
    /// something a reviewer will want in full when it does.
    pub permission_denials: Vec<Value>,
    /// The four token counts ADR-0022 persists. Absent fields stay `None`.
    pub usage: TokenUsage,
    /// What this event said about the usage window, for a provider that reports
    /// it on the terminal event rather than on one of its own.
    pub usage_window: Option<UsageWindow>,
}

/// Why a run stopped, in the only vocabulary shared code is allowed to see.
///
/// Six values, chosen because each one is a *different decision* for
/// [`classify`](crate::runner::outcome::classify) and not because any provider
/// spells them this way. A provider maps its own terminal words onto these; a
/// word it has never seen is [`Unknown`](EndReason::Unknown), which ADR-0011
/// makes transient rather than fatal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EndReason {
    /// The agent finished the work and said so.
    Completed,
    /// The run was killed mid-stream — ADR-0011's "process died".
    Interrupted,
    /// The per-attempt turn budget ran out (ADR-0011's fatal row).
    TurnBudgetExhausted,
    /// The provider declined to do the work. Retrying spends the same tokens on
    /// the same refusal.
    Refused,
    /// It failed, and said something about it.
    Errored { detail: Option<String> },
    /// It ended in a way this provider could not describe. **The default**, and
    /// deliberately the one ADR-0011 retries.
    #[default]
    Unknown,
}

/// What one attempt spent, off the terminal event's `usage` object.
///
/// Four scalars out of a much larger object, chosen by ADR-0022 and no more:
/// the recorded corpus also carries `output_tokens_details`, `server_tool_use`,
/// `cache_creation`, `service_tier`, `iterations` and `speed`, none of which any
/// decision needs. `modelUsage` is deliberately *not* read either — it is a
/// per-model map, and ADR-0022 asked for four numbers on a row, not a document.
///
/// Every field is `Option`, and seam-contract D18 is the reason: a run that dies
/// before its `result` never learns these, and `None` must survive to the column
/// as NULL rather than being flattened to a zero that reads as a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    /// `cache_read_input_tokens`, renamed to the column ADR-0022 names.
    pub cache_read_tokens: Option<i64>,
    /// `cache_creation_input_tokens`, likewise.
    pub cache_creation_tokens: Option<i64>,
}

/// An event this version does not model, kept whole.
#[derive(Debug, Clone, PartialEq)]
pub struct OtherEvent {
    /// The top-level `type`.
    pub event_type: String,
    /// The `subtype`, for the `system` events whose vocabulary is open-ended.
    pub subtype: Option<String>,
    pub raw: Value,
}

impl RunEvent {
    /// A neutral name for this event, for logging and for a test that wants to
    /// say what it saw.
    ///
    /// Rimaia's word, never the provider's wire type — except for
    /// [`Other`](RunEvent::Other), where the provider's own word is the whole of
    /// what is known about it.
    pub fn event_type(&self) -> &str {
        match self {
            Self::Init(_) => "init",
            Self::Assistant(_) => "assistant",
            Self::User(_) => "user",
            Self::Usage(_) => "usage",
            Self::Result(_) => "result",
            Self::Other(other) => &other.event_type,
        }
    }
}

// ---------------------------------------------------------------------------
// The files a run writes
// ---------------------------------------------------------------------------

/// `<app-data>/runs/<task-id>/<run-id>.jsonl` (ADR-0013).
pub fn transcript_path(paths: &AppPaths, task_id: &str, run_id: &str) -> PathBuf {
    run_dir(paths, task_id).join(format!("{run_id}.jsonl"))
}

/// The child's stderr, beside its transcript. A separate file rather than
/// interleaved lines, so the `.jsonl` stays valid JSONL — task 008's acceptance
/// criterion is about that file, and a stray stack trace in it would break every
/// reader for the sake of one.
pub fn stderr_path(paths: &AppPaths, task_id: &str, run_id: &str) -> PathBuf {
    run_dir(paths, task_id).join(format!("{run_id}.stderr.log"))
}

fn run_dir(paths: &AppPaths, task_id: &str) -> PathBuf {
    paths.runs_dir().join(task_id)
}

/// The append-only JSONL transcript.
///
/// # What "flushed as it arrives" buys, exactly
///
/// Each line is one `write_all` of the line plus its terminator, straight at the
/// `File` — no `BufWriter` anywhere, because a buffer is precisely the thing
/// that would hold the last few events at the moment they become interesting.
/// After `append` returns, the bytes are with the kernel: **any death of the
/// Rimaia process — a panic, a `kill -9`, a force-quit — leaves the transcript
/// complete up to the last line the parser saw**, which is ADR-0013's "a crash
/// mid-run still leaves a readable transcript" and task 008's acceptance
/// criterion.
///
/// What it does not buy is durability against the *machine* dying: without an
/// `fsync` per line the page cache can still be lost to a kernel panic or a
/// power cut. That trade is deliberate — an `fsync` on every event of a chatty
/// run costs far more than the case it protects, which is one where the user has
/// lost more than a transcript anyway. [`sync`](Self::sync) closes the window
/// once, at the end of a run.
///
/// Opened in append mode so a resumed attempt writing to an existing path
/// extends the record instead of truncating what the first attempt proved.
#[derive(Debug)]
pub struct Transcript {
    path: PathBuf,
    file: File,
}

impl Transcript {
    pub fn create(paths: &AppPaths, task_id: &str, run_id: &str) -> Result<Self> {
        let path = transcript_path(paths, task_id, run_id);
        std::fs::create_dir_all(run_dir(paths, task_id))?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `raw_line` verbatim, newline-terminated.
    ///
    /// Verbatim including a line that failed to parse: the transcript is
    /// evidence, not a projection of what Rimaia understood. One `write_all`
    /// rather than two, so a line and its terminator cannot be separated by
    /// anything short of the OS itself failing.
    pub fn append(&mut self, raw_line: &str) -> Result<()> {
        let mut framed = String::with_capacity(raw_line.len() + 1);
        framed.push_str(raw_line);
        framed.push('\n');
        self.file.write_all(framed.as_bytes())?;

        Ok(())
    }

    /// Pushes the file to disk. Called once when a run ends, not per line.
    pub fn sync(&self) -> Result<()> {
        self.file.sync_all()?;

        Ok(())
    }
}

/// The child's stderr, captured beside the transcript.
///
/// Created on the first line rather than up front: most runs write nothing here,
/// and an empty file per run is litter in a directory the user is invited to
/// inspect (ADR-0003's "any tool" argument applies to the run directory too).
#[derive(Debug)]
pub struct StderrLog {
    path: PathBuf,
    file: Option<File>,
}

impl StderrLog {
    pub fn new(paths: &AppPaths, task_id: &str, run_id: &str) -> Self {
        Self {
            path: stderr_path(paths, task_id, run_id),
            file: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether anything has been written — i.e. whether [`path`](Self::path)
    /// exists.
    pub fn is_empty(&self) -> bool {
        self.file.is_none()
    }

    pub fn append(&mut self, line: &str) -> Result<()> {
        if self.file.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }

        let file = self
            .file
            .as_mut()
            .expect("the stderr file was just opened or already was");
        let mut framed = String::with_capacity(line.len() + 1);
        framed.push_str(line);
        framed.push('\n');
        file.write_all(framed.as_bytes())?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The live view
// ---------------------------------------------------------------------------

/// A tool call the agent made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// The `tool_use_id`, so the matching `tool_result` can close it out.
    pub id: String,
    pub name: String,
    /// A bounded one-line rendering of the call's most identifying argument —
    /// the file being read, the command being run.
    ///
    /// A rendering rather than the input itself, because a `Write` call's input
    /// is an entire file and this value is cloned once per broadcast subscriber.
    /// The keys are tried in [`TOOL_DETAIL_KEYS`]' order and the result is cut at
    /// [`MAX_TOOL_DETAIL_CHARS`]; a tool whose input has none of them shows its
    /// name alone, which is what task 008's "shows the current tool call" asks
    /// for at minimum.
    pub detail: Option<String>,
}

/// The input keys worth showing, most identifying first. Ordinary Claude Code
/// tool inputs; an unrecognised tool simply has none of them.
const TOOL_DETAIL_KEYS: [&str; 8] = [
    "file_path",
    "command",
    "path",
    "pattern",
    "url",
    "query",
    "description",
    "prompt",
];

impl ToolCall {
    fn new(id: String, name: String, input: &Value) -> Self {
        let detail = TOOL_DETAIL_KEYS
            .iter()
            .find_map(|key| text(input, key))
            .map(|value| clamp(value.trim(), MAX_TOOL_DETAIL_CHARS))
            .filter(|value| !value.is_empty());

        Self { id, name, detail }
    }
}

/// One line of the live view's scrollback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Activity {
    /// Something the agent said, clamped to [`MAX_ACTIVITY_CHARS`].
    Assistant {
        text: String,
    },
    ToolCall(ToolCall),
}

/// What a watching client is shown while a run is in flight (seam-contract D14).
///
/// Carries a payload, unlike [`ChangeEvent`](crate::ChangeEvent), because it is
/// a *view* and not a fact about stored state — there is nothing to re-read, and
/// an id would tell a client to query the database once per turn. The five
/// fields are the ones D14 names.
///
/// **Never the source of truth for anything persisted.** The transcript file is
/// (ADR-0013) and the `runs` row is; if this and the row disagree, the row wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTail {
    pub run_id: RunId,
    pub elapsed_ms: i64,
    /// Approximate until the run ends — see [`RunProgress::turns`].
    pub turns: u32,
    pub current_tool: Option<ToolCall>,
    pub last_assistant_text: Option<String>,
}

/// The bounded in-memory picture of a run in flight.
///
/// A client that starts watching mid-run reads [`recent`](Self::recent) to catch
/// up; the tail channel is for what happens after that. Which is why the buffer
/// is bounded and the channel is lossy: between them they describe "the last
/// little while", and the complete answer is the JSONL file.
pub struct RunProgress {
    run_id: RunId,
    clock: Arc<dyn Clock>,
    started_at: DateTime<Utc>,
    recent: VecDeque<Activity>,
    turns: u32,
    last_message_id: Option<String>,
    current_tool: Option<ToolCall>,
    last_assistant_text: Option<String>,
}

impl RunProgress {
    pub fn new(run_id: impl Into<RunId>, clock: Arc<dyn Clock>) -> Self {
        let started_at = clock.now();
        Self {
            run_id: run_id.into(),
            clock,
            started_at,
            recent: VecDeque::with_capacity(RECENT_ACTIVITY_CAPACITY),
            turns: 0,
            last_message_id: None,
            current_tool: None,
            last_assistant_text: None,
        }
    }

    /// Folds one event in. Reports whether anything a watcher would notice
    /// changed, so a run that emits a hundred `thinking_tokens` events does not
    /// publish a hundred identical snapshots.
    pub fn observe(&mut self, event: &RunEvent) -> bool {
        match event {
            RunEvent::Assistant(assistant) => self.observe_assistant(assistant),
            RunEvent::User(user) => self.observe_user(user),
            RunEvent::Result(result) => {
                // The `result` event's own count supersedes the running
                // approximation below — D14 rule 2, applied to the one number
                // where the tail and the `runs` row could otherwise disagree in
                // front of the user. A count that will not fit leaves the
                // approximation standing rather than replacing it with nonsense.
                if let Some(num_turns) = result.num_turns {
                    self.turns = u32::try_from(num_turns).unwrap_or(self.turns);
                }
                self.current_tool = None;
                true
            }
            _ => false,
        }
    }

    fn observe_assistant(&mut self, assistant: &AssistantEvent) -> bool {
        // Several events share one `message_id` — a thinking block and the tool
        // call it produced arrive separately. Counting transitions rather than
        // events is the closest a live reader gets to a turn count; the CLI's
        // own `num_turns` counts something subtly different (the corpus has
        // 7 message ids against 9 turns), which is exactly why this is
        // documented as approximate and replaced when `result` lands.
        let mut changed = false;
        if assistant.message_id.is_some() && assistant.message_id != self.last_message_id {
            self.last_message_id = assistant.message_id.clone();
            self.turns += 1;
            changed = true;
        }

        for block in &assistant.content {
            match block {
                // `body`, not `text`, so the binding does not shadow the
                // extractor of that name a few lines further down the file.
                ContentBlock::Text(body) if !body.trim().is_empty() => {
                    let text = clamp(body.trim(), MAX_ACTIVITY_CHARS);
                    self.last_assistant_text = Some(text.clone());
                    self.push(Activity::Assistant { text });
                    changed = true;
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let call = ToolCall::new(id.clone(), name.clone(), input);
                    self.current_tool = Some(call.clone());
                    self.push(Activity::ToolCall(call));
                    changed = true;
                }
                _ => {}
            }
        }

        changed
    }

    fn observe_user(&mut self, user: &UserEvent) -> bool {
        let mut changed = false;
        for block in &user.content {
            // Only the result that closes the call being shown clears it. A
            // result for some other call — parallel tool use — leaves the
            // displayed one alone rather than blanking the view.
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                if self
                    .current_tool
                    .as_ref()
                    .is_some_and(|call| &call.id == tool_use_id)
                {
                    self.current_tool = None;
                    changed = true;
                }
            }
        }

        changed
    }

    fn push(&mut self, activity: Activity) {
        if self.recent.len() == RECENT_ACTIVITY_CAPACITY {
            self.recent.pop_front();
        }
        self.recent.push_back(activity);
    }

    /// The catch-up buffer, oldest first. Never longer than
    /// [`RECENT_ACTIVITY_CAPACITY`].
    pub fn recent(&self) -> impl ExactSizeIterator<Item = &Activity> {
        self.recent.iter()
    }

    /// How many turns have been seen.
    ///
    /// A live approximation — distinct assistant message ids — until the `result`
    /// event arrives with the CLI's own `num_turns`, which replaces it.
    pub fn turns(&self) -> u32 {
        self.turns
    }

    pub fn current_tool(&self) -> Option<&ToolCall> {
        self.current_tool.as_ref()
    }

    pub fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }

    /// Milliseconds since the run started, read off the injected clock.
    ///
    /// Clamped at zero: a clock that steps backwards mid-run (an NTP correction,
    /// a laptop waking up) is not a reason to show a negative elapsed time.
    pub fn elapsed_ms(&self) -> i64 {
        (self.clock.now() - self.started_at)
            .num_milliseconds()
            .max(0)
    }

    /// The snapshot published on the tail channel.
    pub fn tail(&self) -> RunTail {
        RunTail {
            run_id: self.run_id.clone(),
            elapsed_ms: self.elapsed_ms(),
            turns: self.turns,
            current_tool: self.current_tool.clone(),
            last_assistant_text: self.last_assistant_text.clone(),
        }
    }
}

impl std::fmt::Debug for RunProgress {
    /// Hand-written because [`Clock`] does not require `Debug`, for the reason
    /// [`ServiceContext`]'s own impl gives.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunProgress")
            .field("run_id", &self.run_id)
            .field("turns", &self.turns)
            .field("recent", &self.recent.len())
            .field("current_tool", &self.current_tool)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------

/// One run's stdout, from the outside: hand it lines, it does the rest.
///
/// Deliberately not a reader. The process module owns the child and its pipes;
/// this owns what a line *means*, which is what makes every scenario in the
/// fixture corpus replayable without spawning anything or spending a token
/// (ADR-0015).
pub struct EventStream {
    context: ServiceContext,
    /// Who decides what a line means (ADR-0026). Defaulted rather than required,
    /// so no existing call site had to learn that a provider exists — see
    /// [`driven_by`](Self::driven_by).
    provider: Arc<dyn AgentProvider>,
    transcript: Transcript,
    stderr: StderrLog,
    progress: RunProgress,
    init: Option<InitEvent>,
    usage: Option<UsageReport>,
    result: Option<ResultEvent>,
    malformed_lines: u64,
    denied_tool_calls: u64,
}

impl EventStream {
    /// Opens the transcript and starts the clock.
    ///
    /// The run id is the one already on the `runs` row: the row and its
    /// transcript path are created together (ADR-0013), before the first line
    /// exists.
    pub fn create(
        context: &ServiceContext,
        paths: &AppPaths,
        task_id: &str,
        run_id: &str,
    ) -> Result<Self> {
        Ok(Self {
            context: context.clone(),
            provider: Arc::new(ClaudeProvider),
            transcript: Transcript::create(paths, task_id, run_id)?,
            stderr: StderrLog::new(paths, task_id, run_id),
            progress: RunProgress::new(run_id, context.clock.clone()),
            init: None,
            usage: None,
            result: None,
            malformed_lines: 0,
            denied_tool_calls: 0,
        })
    }

    /// Reads this stream as `provider` speaks it.
    ///
    /// A builder rather than a parameter on [`create`](Self::create), in the
    /// style of the rest of this type: every existing caller replays the Claude
    /// corpus and says so by not saying anything, and the one caller that knows
    /// which provider it spawned says which.
    #[must_use]
    pub fn driven_by(mut self, provider: Arc<dyn AgentProvider>) -> Self {
        self.provider = provider;
        self
    }

    /// Persists one raw stdout line, parses it, folds it in, and publishes a
    /// tail snapshot if a watcher would notice the difference.
    ///
    /// `Ok(None)` is a line that was persisted but yielded no event: a blank
    /// line, or one the parser had to skip. `Err` is only ever the transcript
    /// failing to write — surfaced rather than swallowed, because whether a run
    /// that can no longer record what it is doing should continue is the
    /// spawning code's decision, not this module's.
    pub fn observe(&mut self, raw_line: &str) -> Result<Option<RunEvent>> {
        if raw_line.trim().is_empty() {
            return Ok(None);
        }

        // Before parsing, so a line we cannot read is still evidence.
        self.transcript.append(raw_line)?;

        let event = match self.provider.parse_line(raw_line) {
            Ok(event) => event,
            Err(error) => {
                self.malformed_lines += 1;
                tracing::warn!(
                    run_id = %self.progress.run_id,
                    %error,
                    "skipping an unparseable line; it is in the transcript verbatim"
                );
                return Ok(None);
            }
        };

        if reports_a_permission_denial(&event, raw_line) {
            self.denied_tool_calls += 1;
        }

        match &event {
            RunEvent::Init(init) => self.init = Some(init.clone()),
            RunEvent::Usage(window) => self.observe_usage(window.clone()),
            RunEvent::Result(result) => {
                // A provider that reports the window on its terminal event
                // rather than on one of its own is folded in on the same terms,
                // latching included.
                if let Some(window) = &result.usage_window {
                    self.observe_usage(window.clone());
                }
                self.result = Some(result.clone());
            }
            RunEvent::Other(other) => tracing::debug!(
                run_id = %self.progress.run_id,
                event_type = %other.event_type,
                subtype = other.subtype.as_deref().unwrap_or("-"),
                "keeping an unmodelled event opaque"
            ),
            _ => {}
        }

        if self.progress.observe(&event) {
            self.context.publish_tail(self.progress.tail());
        }

        Ok(Some(event))
    }

    /// Captures one line of the child's stderr.
    pub fn observe_stderr(&mut self, raw_line: &str) -> Result<()> {
        self.stderr.append(raw_line)
    }

    /// The applied configuration, once `init` has arrived. Task 008 compares
    /// this against what it asked for rather than assuming it was honoured.
    pub fn init(&self) -> Option<&InitEvent> {
        self.init.as_ref()
    }

    /// What the provider last said about the usage window, and when it said it.
    ///
    /// **`Exhausted` latches** — see [`observe_usage`](Self::observe_usage).
    pub fn usage(&self) -> Option<&UsageReport> {
        self.usage.as_ref()
    }

    /// Folds one usage report in, stamping it with the instant its line was
    /// read.
    ///
    /// # Why a refusal latches
    ///
    /// A provider that reports the window on **every turn** will happily report
    /// `allowed` again after the turn that hit the wall — a heartbeat, not a
    /// retraction. Letting that clear the refusal would classify the run as
    /// `transient` instead of `usage_limit`, so a walled task backs off
    /// 1m/5m/15m into a window that is still closed and is abandoned by
    /// morning. That is ADR-0011's named nightmare reached by a route that only
    /// exists once a second provider's shape is taken seriously (ADR-0026 point
    /// 8).
    ///
    /// The clock is the injected one, so a test drives this with no `sleep`
    /// anywhere.
    fn observe_usage(&mut self, window: UsageWindow) {
        let report = UsageReport {
            window,
            observed_at: self.context.clock.now(),
        };

        let latched = self
            .usage
            .as_ref()
            .is_some_and(|held| held.hit_a_wall() && !report.hit_a_wall());
        if !latched {
            self.usage = Some(report);
        }
    }

    /// The terminal event, or `None` for a stream that stopped without one —
    /// which is a different condition from a run that ended badly and said so.
    pub fn result(&self) -> Option<&ResultEvent> {
        self.result.as_ref()
    }

    pub fn progress(&self) -> &RunProgress {
        &self.progress
    }

    /// How many lines had to be skipped. Non-zero is worth reporting; it is not
    /// by itself a failed run.
    pub fn malformed_lines(&self) -> u64 {
        self.malformed_lines
    }

    /// How many tool results came back refused for want of approval.
    ///
    /// A run that spends its whole hour being told no still ends looking
    /// unremarkable — the CLI exits, the classifier sees an ending, and
    /// nothing anywhere says the agent was never allowed to do the work. This
    /// is what gives the log line and the run detail something to say about
    /// that. See [`reports_a_permission_denial`] for what it is and is not
    /// allowed to decide.
    pub fn denied_tool_calls(&self) -> u64 {
        self.denied_tool_calls
    }

    pub fn transcript_path(&self) -> &Path {
        self.transcript.path()
    }

    pub fn stderr_path(&self) -> &Path {
        self.stderr.path()
    }

    /// Whether the child wrote anything to stderr.
    pub fn stderr_is_empty(&self) -> bool {
        self.stderr.is_empty()
    }

    /// Pushes the transcript to disk. Once, when the run is over.
    pub fn finish(&self) -> Result<()> {
        self.transcript.sync()
    }
}

impl std::fmt::Debug for EventStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream")
            .field("transcript", &self.transcript.path())
            .field("malformed_lines", &self.malformed_lines)
            .field("progress", &self.progress)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Extractors
// ---------------------------------------------------------------------------
//
// What is left here after ADR-0026 reads the *live view's* own values off a tool
// input, which is a provider-shaped document this module deliberately keeps
// opaque — the wire extractors that read an event's fields moved into the
// provider with the format they were reading.

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// At most `max_chars` characters, with [`TRUNCATION_MARKER`] when it had to
/// cut. Counts characters rather than bytes so a multi-byte boundary cannot
/// panic the run that is otherwise going fine.
fn clamp(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        None => text.to_owned(),
        Some((byte, _)) => {
            let mut clamped = String::with_capacity(byte + TRUNCATION_MARKER.len_utf8());
            clamped.push_str(&text[..byte]);
            clamped.push(TRUNCATION_MARKER);
            clamped
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn parse(line: &str) -> RunEvent {
        ClaudeProvider.parse_line(line).expect("the line is JSON")
    }

    /// The typed gate first, the phrase only to tell a refusal from an
    /// ordinary tool failure. Both halves are load-bearing: without the gate
    /// an assistant *discussing* approval would count, and without the phrase
    /// every failing test run would.
    #[test]
    fn a_refused_tool_call_is_told_apart_from_a_tool_that_merely_failed() {
        let refused = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"This Bash command contains multiple operations. The following part requires approval: git remote get-url origin"}]}}"#;
        let failed = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"error: could not compile `rimaia-core`"}]}}"#;
        let succeeded = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":false,"content":"ok"}]}}"#;
        let discussed = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"That command requires approval, so I will not run it."}]}}"#;

        assert!(reports_a_permission_denial(&parse(refused), refused));
        assert!(!reports_a_permission_denial(&parse(failed), failed));
        assert!(!reports_a_permission_denial(&parse(succeeded), succeeded));
        assert!(
            !reports_a_permission_denial(&parse(discussed), discussed),
            "an assistant talking about approval is not a tool being refused",
        );
    }

    #[test]
    fn a_tool_calls_detail_is_the_first_recognised_input_key() {
        let call = ToolCall::new(
            "toolu_1".to_string(),
            "Bash".to_string(),
            &json!({ "description": "run the tests", "command": "cargo test" }),
        );

        // `command` outranks `description` regardless of the input's own key
        // order, because the ordering is TOOL_DETAIL_KEYS' and not the JSON's.
        assert_eq!(call.detail.as_deref(), Some("cargo test"));
    }

    #[test]
    fn a_tool_call_whose_input_says_nothing_recognisable_shows_its_name_alone() {
        let call = ToolCall::new(
            "toolu_1".to_string(),
            "SomeNewTool".to_string(),
            &json!({ "unfamiliar": 7 }),
        );

        assert_eq!(call.detail, None);
    }

    #[test]
    fn a_tail_snapshot_serializes_to_the_shape_the_shell_forwards() {
        // The shell re-emits this verbatim as `runs:tail` (seam-contract D14),
        // and `src/lib/events.ts` types the payload once. camelCase keys, so the
        // two spellings are the same one.
        let tail = RunTail {
            run_id: "run-1".to_string(),
            elapsed_ms: 4_000,
            turns: 2,
            current_tool: Some(ToolCall {
                id: "toolu_1".to_string(),
                name: "Bash".to_string(),
                detail: Some("cargo test".to_string()),
            }),
            last_assistant_text: Some("Running the tests.".to_string()),
        };

        assert_eq!(
            serde_json::to_string(&tail).expect("a tail always serializes"),
            r#"{"runId":"run-1","elapsedMs":4000,"turns":2,"currentTool":{"id":"toolu_1","name":"Bash","detail":"cargo test"},"lastAssistantText":"Running the tests."}"#
        );
    }

    #[test]
    fn an_activity_line_serializes_with_the_kind_that_tells_them_apart() {
        let assistant = Activity::Assistant {
            text: "Reading the file.".to_string(),
        };
        let call = Activity::ToolCall(ToolCall {
            id: "toolu_1".to_string(),
            name: "Read".to_string(),
            detail: None,
        });

        assert_eq!(
            serde_json::to_string(&assistant).expect("an activity always serializes"),
            r#"{"kind":"assistant","text":"Reading the file."}"#
        );
        assert_eq!(
            serde_json::to_string(&call).expect("an activity always serializes"),
            r#"{"kind":"toolCall","id":"toolu_1","name":"Read","detail":null}"#
        );
    }

    #[test]
    fn clamping_cuts_on_a_character_boundary_and_says_that_it_cut() {
        assert_eq!(clamp("abc", 8), "abc");
        assert_eq!(clamp("abcdefgh", 8), "abcdefgh");
        // Four-byte characters: a byte-wise cut here would panic.
        assert_eq!(clamp("🚀🚀🚀", 2), "🚀🚀…");
    }
}
