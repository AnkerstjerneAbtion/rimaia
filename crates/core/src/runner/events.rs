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

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::clock::Clock;
use crate::context::ServiceContext;
use crate::error::Result;
use crate::events::RunId;
use crate::paths::AppPaths;

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
    RateLimit(RateLimitEvent),
    Result(ResultEvent),
    Other(OtherEvent),
}

/// `system`/`init` — the applied configuration, echoed back (ADR-0004).
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
    /// `permissionMode` — camelCase in the stream, unlike its neighbours.
    pub permission_mode: Option<String>,
    /// `apiKeySource`. `"none"` confirms subscription auth rather than a
    /// metered API key, which is ADR-0004's premise.
    pub api_key_source: Option<String>,
    pub claude_code_version: Option<String>,
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

/// `rate_limit_event` — the usage-limit signal, typed (ADR-0011's amendment).
///
/// Arrives early and unprompted on **every** run, not only on failure, which is
/// what lets task 014 read limit state before committing to a long task. Do not
/// grep an error message for this.
///
/// `status` is a `String` and not an enum on purpose. `spike/FINDINGS.md` §4 is
/// explicit that the only value ever observed is `"allowed"` — the spike never
/// hit a real limit, and there is no `usage_limit` fixture. Inventing variants
/// for the payload nobody has seen would manufacture a contract that the
/// classifier could then be written to "pass" against, in exactly the module
/// ADR-0011 calls the one most likely to break on a CLI update and the one whose
/// breakage is least visible. The three field *names* below are what the corpus
/// proves; their vocabulary is not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RateLimitEvent {
    pub status: Option<String>,
    /// `resetsAt`, epoch seconds.
    pub resets_at: Option<i64>,
    /// `rateLimitType`, e.g. `"five_hour"`.
    pub rate_limit_type: Option<String>,
}

impl RateLimitEvent {
    /// [`resets_at`](Self::resets_at) as an instant, or `None` when it is absent
    /// or outside the representable range.
    ///
    /// The scheduler waits until this plus jitter (ADR-0011), so an epoch the
    /// CLI reports nonsensically must read as "no reset time known" rather than
    /// panic a run that is otherwise fine.
    pub fn resets_at_utc(&self) -> Option<DateTime<Utc>> {
        self.resets_at
            .and_then(|epoch| Utc.timestamp_opt(epoch, 0).single())
    }
}

/// `result` — the terminal event, which arrives even when the run was killed.
///
/// `spike/FINDINGS.md` §5: a SIGTERM-killed run emits this and *then* exits 143.
/// The stream does not simply stop, so classification reads `terminal_reason`
/// with `subtype` and never the exit code alone. Everything the `runs` row needs
/// is already here — turns, cost, duration — with nothing to derive.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultEvent {
    pub subtype: Option<String>,
    /// The cleanest discriminator, and the one the original ADR did not have:
    /// `completed`, `aborted_streaming`, `max_turns`.
    pub terminal_reason: Option<String>,
    pub is_error: bool,
    pub num_turns: Option<i64>,
    pub total_cost_usd: Option<f64>,
    pub duration_ms: Option<i64>,
    pub session_id: Option<String>,
    pub stop_reason: Option<String>,
    /// The agent's own closing summary. Present on `success`, null on the error
    /// subtypes, which carry [`errors`](Self::errors) instead.
    pub result: Option<String>,
    pub errors: Vec<String>,
    /// Kept whole: nothing reads its shape yet, and ADR-0012 makes a denial
    /// something a reviewer will want in full when it does.
    pub permission_denials: Vec<Value>,
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
    /// Dispatches a parsed document onto a variant. Never fails.
    ///
    /// Falling back to [`Other`](RunEvent::Other) rather than erroring is the
    /// tolerance rule: an event whose `type` we know but whose body changed
    /// shape is still an event, and dropping it would be a worse answer than
    /// keeping it opaque.
    pub fn from_value(raw: Value) -> Self {
        let event_type = text(&raw, "type").unwrap_or_default();
        let subtype = text(&raw, "subtype");

        match (event_type.as_str(), subtype.as_deref()) {
            ("system", Some("init")) => Self::Init(InitEvent::from_value(&raw)),
            ("assistant", _) => Self::Assistant(AssistantEvent::from_value(&raw)),
            ("user", _) => Self::User(UserEvent::from_value(&raw)),
            ("rate_limit_event", _) => Self::RateLimit(RateLimitEvent::from_value(&raw)),
            ("result", _) => Self::Result(ResultEvent::from_value(&raw)),
            _ => Self::Other(OtherEvent {
                event_type,
                subtype,
                raw,
            }),
        }
    }

    /// The top-level `type`, for logging and for a test that wants to say what
    /// it saw. Never the nested `message.type`.
    pub fn event_type(&self) -> &str {
        match self {
            Self::Init(_) => "system",
            Self::Assistant(_) => "assistant",
            Self::User(_) => "user",
            Self::RateLimit(_) => "rate_limit_event",
            Self::Result(_) => "result",
            Self::Other(other) => &other.event_type,
        }
    }
}

impl InitEvent {
    fn from_value(raw: &Value) -> Self {
        Self {
            session_id: text(raw, "session_id"),
            cwd: text(raw, "cwd"),
            model: text(raw, "model"),
            permission_mode: text(raw, "permissionMode"),
            api_key_source: text(raw, "apiKeySource"),
            claude_code_version: text(raw, "claude_code_version"),
            tools: strings(raw, "tools"),
            mcp_servers: array(raw, "mcp_servers")
                .iter()
                .filter_map(McpServer::from_value)
                .collect(),
        }
    }
}

impl McpServer {
    /// `None` for an entry with no name — a server we cannot name is one nothing
    /// downstream could assert against anyway.
    fn from_value(raw: &Value) -> Option<Self> {
        Some(Self {
            name: text(raw, "name")?,
            status: text(raw, "status"),
        })
    }
}

impl AssistantEvent {
    fn from_value(raw: &Value) -> Self {
        let message = raw.get("message");
        Self {
            session_id: text(raw, "session_id"),
            message_id: message.and_then(|message| text(message, "id")),
            content: content_blocks(message),
        }
    }
}

impl UserEvent {
    fn from_value(raw: &Value) -> Self {
        Self {
            session_id: text(raw, "session_id"),
            content: content_blocks(raw.get("message")),
        }
    }
}

impl RateLimitEvent {
    fn from_value(raw: &Value) -> Self {
        let info = raw.get("rate_limit_info").unwrap_or(&Value::Null);
        Self {
            status: text(info, "status"),
            resets_at: integer(info, "resetsAt"),
            rate_limit_type: text(info, "rateLimitType"),
        }
    }
}

impl ResultEvent {
    fn from_value(raw: &Value) -> Self {
        Self {
            subtype: text(raw, "subtype"),
            terminal_reason: text(raw, "terminal_reason"),
            is_error: flag(raw, "is_error").unwrap_or(false),
            num_turns: integer(raw, "num_turns"),
            total_cost_usd: number(raw, "total_cost_usd"),
            duration_ms: integer(raw, "duration_ms"),
            session_id: text(raw, "session_id"),
            stop_reason: text(raw, "stop_reason"),
            result: text(raw, "result"),
            errors: strings(raw, "errors"),
            permission_denials: array(raw, "permission_denials").to_vec(),
        }
    }
}

impl ContentBlock {
    fn from_value(raw: &Value) -> Self {
        // `type` here is the *block's* type, one level below the event's. This
        // is the nesting that defeats substring matching (spike section 3).
        match text(raw, "type").as_deref() {
            Some("text") => Self::Text(text(raw, "text").unwrap_or_default()),
            Some("tool_use") => match (text(raw, "id"), text(raw, "name")) {
                (Some(id), Some(name)) => Self::ToolUse {
                    id,
                    name,
                    input: raw.get("input").cloned().unwrap_or(Value::Null),
                },
                // A tool call we cannot name or correlate is not one the live
                // view can show; keep it whole rather than half-modelled.
                _ => Self::Other(raw.clone()),
            },
            Some("tool_result") => match text(raw, "tool_use_id") {
                Some(tool_use_id) => Self::ToolResult {
                    tool_use_id,
                    is_error: flag(raw, "is_error").unwrap_or(false),
                },
                None => Self::Other(raw.clone()),
            },
            _ => Self::Other(raw.clone()),
        }
    }
}

fn content_blocks(message: Option<&Value>) -> Vec<ContentBlock> {
    message
        .map(|message| array(message, "content"))
        .unwrap_or_default()
        .iter()
        .map(ContentBlock::from_value)
        .collect()
}

// ---------------------------------------------------------------------------
// Parsing one line
// ---------------------------------------------------------------------------

/// Parses one line of the stream.
///
/// The only failure is a line that is not JSON at all — the condition
/// `malformed-line.jsonl` records, and `truncated-stream.jsonl` ends on. The
/// caller logs it, keeps the raw line in the transcript, and reads the next one;
/// it is never fatal, because the events on either side of a bad line are still
/// the run's evidence.
pub fn parse_line(line: &str) -> std::result::Result<RunEvent, serde_json::Error> {
    serde_json::from_str(line).map(RunEvent::from_value)
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
    transcript: Transcript,
    stderr: StderrLog,
    progress: RunProgress,
    init: Option<InitEvent>,
    rate_limit: Option<RateLimitEvent>,
    result: Option<ResultEvent>,
    malformed_lines: u64,
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
            transcript: Transcript::create(paths, task_id, run_id)?,
            stderr: StderrLog::new(paths, task_id, run_id),
            progress: RunProgress::new(run_id, context.clock.clone()),
            init: None,
            rate_limit: None,
            result: None,
            malformed_lines: 0,
        })
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

        let event = match parse_line(raw_line) {
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

        match &event {
            RunEvent::Init(init) => self.init = Some(init.clone()),
            RunEvent::RateLimit(rate_limit) => self.rate_limit = Some(rate_limit.clone()),
            RunEvent::Result(result) => self.result = Some(result.clone()),
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

    /// The most recent usage-limit report. Present on every real run.
    pub fn rate_limit(&self) -> Option<&RateLimitEvent> {
        self.rate_limit.as_ref()
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
// One shape each, all returning `Option`/empty rather than erroring. A field
// that changed type under us costs that field and nothing else.

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
        parse_line(line).expect("the line is JSON")
    }

    #[test]
    fn a_line_that_is_not_json_is_the_only_parse_failure() {
        assert!(parse_line("{\"type\": \"result\", \"num_tur").is_err());
        assert!(parse_line("not json at all").is_err());
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
        assert_eq!(result.subtype.as_deref(), Some("success"));
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
        let event = RateLimitEvent {
            resets_at: Some(i64::MAX),
            ..RateLimitEvent::default()
        };

        assert_eq!(event.resets_at_utc(), None);
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
