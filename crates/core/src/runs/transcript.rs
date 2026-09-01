//! Paginated, readable rendering of one run's JSONL transcript, and text
//! search across it (task 015, ADR-0013).
//!
//! # Pagination, not virtualization — and why
//!
//! Task 015's hard requirement is "a 50MB transcript opens without freezing
//! the UI." Virtualization (rendering only the DOM rows currently in view)
//! still requires *something* to hold the whole parsed transcript in memory
//! on one side of the IPC boundary before it can decide which rows are in
//! view; for a 50MB file of assistant messages and tool payloads, that
//! something is tens of megabytes of JavaScript objects serialized across
//! `invoke` in one call, which is exactly the freeze the requirement rules
//! out. Pagination instead bounds the *backend* read to `limit` lines
//! ([`DEFAULT_PAGE_SIZE`] by default) and the *wire payload* to one page's
//! worth of parsed entries — the file itself is read with
//! [`tokio::fs::File`] and [`tokio::io::BufReader`], line by line, so a
//! 50MB file costs one bounded buffer and a sequential scan, never a single
//! multi-megabyte allocation.
//!
//! [`read_page`] still scans from the start of the file on every call, past
//! the requested window, to report [`TranscriptPage::total_lines`] — an
//! O(file size) cost, but one that never parses or retains a line outside
//! the window, and disk cache makes a repeated scan of the same file fast in
//! practice. Building a persistent line-offset index would trade that for a
//! second thing to keep in sync with a transcript that is still being
//! appended to while a run is in flight (ADR-0013's own "flushed
//! continuously"); this module only ever reads a *finished* run's file
//! (task 015's Scope — the live view is task 008's `RunTail`), so the
//! simpler re-scan was chosen over an index with nothing to invalidate it.
//!
//! # What is kept that `runner::events` deliberately drops
//!
//! `runner::events::ContentBlock::ToolResult` carries no content — that
//! module's own doc explains why: a live run's
//! [`RunProgress`](crate::runner::events::RunProgress) is a bounded,
//! memory-resident ring buffer, and a single `Read` result can be a whole
//! file. This module reads one page from disk on demand instead of holding
//! anything resident, so the same constraint does not apply, and a tool
//! result's content is exactly what "tool results collapsed by default,
//! expandable" (task 015's Scope) needs to expand *to*. The two parsers are
//! intentionally separate rather than one extended to serve both call sites.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::{Error, Result};

/// How many entries [`read_page`] returns when the caller does not ask for a
/// specific count.
pub const DEFAULT_PAGE_SIZE: usize = 200;

/// How many matches [`search`] collects before it stops reading the file.
/// Bounded for the same reason every other buffer in this codebase is
/// (`RECENT_ACTIVITY_CAPACITY`, `TAIL_CHANNEL_CAPACITY`): a pathological
/// transcript with the search term on every line must not turn a search box
/// into an unbounded read.
pub const MAX_SEARCH_HITS: usize = 200;

/// The longest [`SearchHit::snippet`] returned, in characters.
const MAX_SNIPPET_CHARS: usize = 320;

/// The longest preview kept of a line that would not parse.
///
/// Larger than a snippet because this one is meant to be *read*: the line it
/// most often belongs to is the closing message of a run whose stream was cut,
/// and a sentence-and-a-half of it explains nothing. Still bounded, because a
/// single unparseable line can be a whole file's worth of tool output.
const MAX_MALFORMED_PREVIEW_CHARS: usize = 4_000;

// ---------------------------------------------------------------------------
// The display model — one entry per non-blank JSONL line
// ---------------------------------------------------------------------------

/// One line of the transcript, as the viewer renders it.
///
/// `line` is the 1-based line number in the file *including* blank lines —
/// what a human editor would show — even though [`TranscriptPage`]'s
/// `offset`/count only ever advance over non-blank ones. The two numbers
/// answering different questions is deliberate: `line` is where to point a
/// "open in editor" link, and the page window is how many *events* have been
/// shown.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEntry {
    pub line: usize,
    pub kind: TranscriptEntryKind,
}

/// What one transcript line was — dispatched on the same top-level `type`
/// [`crate::runner::events::RunEvent`] reads, and for the same reason that
/// module gives: never a substring match, only the parsed top-level field.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TranscriptEntryKind {
    Assistant {
        blocks: Vec<TranscriptBlock>,
    },
    /// Tool results being fed back to the model — not a human, exactly as
    /// `runner::events::UserEvent`'s own doc notes.
    User {
        blocks: Vec<TranscriptBlock>,
    },
    // The enum-level `rename_all` cases only the variant tag, never a struct
    // variant's own field names (unlike on a plain struct, where it renames
    // every field) — so `is_error` and `event_type` below each need their
    // own `rename_all` to reach the wire as `isError`/`eventType` instead of
    // breaking this boundary's "camelCase keys" rule.
    #[serde(rename_all = "camelCase")]
    Result {
        summary: Option<String>,
        errors: Vec<String>,
        is_error: bool,
    },
    /// `system`/`init` and anything else this viewer does not render its own
    /// way — ADR-0004's tolerant-parsing rule applies to display just as much
    /// as to the classifier: an event type this version does not know still
    /// occupies its place in the page rather than vanishing from it.
    ///
    /// The `subtype` rides along because without it this arm is a lie by
    /// omission: a real transcript is mostly `system`, and nine different
    /// events — the run's own `init`, a hook's exit code, a subagent
    /// starting, a token counter — all rendered as the single word "system".
    #[serde(rename_all = "camelCase")]
    Other {
        event_type: String,
        subtype: Option<String>,
    },
    /// A line that was not valid JSON at all — kept in its place rather than
    /// skipped silently, so a page's entry count still matches what a reader
    /// counts by eye in the raw file.
    ///
    /// `raw` is a bounded prefix of the line itself. A truncated final line is
    /// the shape of a stream that was cut mid-write, and what it was cut in
    /// the middle of is usually the agent's closing message — the one thing a
    /// reviewer most wants when a run ended without saying why. Rendering the
    /// bytes we have beats telling them a line exists that they may not read.
    Malformed {
        raw: String,
    },
}

/// A block inside one entry's `content` array — the same nesting
/// `runner::events::ContentBlock` documents, with `tool_result` content kept
/// rather than dropped (see this module's own doc for why that is safe here).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TranscriptBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    // See `TranscriptEntryKind::Result`'s identical comment on why this
    // variant needs its own `rename_all`.
    #[serde(rename_all = "camelCase")]
    ToolResult {
        tool_use_id: String,
        is_error: bool,
        /// `None` when the result carried no renderable text — kept absent
        /// rather than an empty string, so "collapsed, expand for nothing"
        /// is not a state the viewer has to invent copy for.
        content: Option<String>,
    },
    /// `thinking`, or whatever a CLI update adds next.
    Other,
}

/// One page of a transcript, oldest line first.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPage {
    pub entries: Vec<TranscriptEntry>,
    /// How many non-blank lines precede this page — `0` for the first page.
    pub offset: usize,
    /// How many non-blank lines the whole file holds, so the viewer can
    /// render "page 3 of 40" and disable "next" on the last page.
    pub total_lines: usize,
}

/// Reads `[offset, offset + limit)` of `log_path`'s non-blank lines, parsed
/// for display, plus the file's total line count.
///
/// Streams the file with a bounded buffer rather than reading it whole —
/// see this module's own doc for why that, and not virtualization, is what
/// makes a 50MB transcript safe to open.
pub async fn read_page(log_path: &Path, offset: usize, limit: usize) -> Result<TranscriptPage> {
    let file = open_transcript(log_path).await?;
    let mut lines = BufReader::new(file).lines();

    let mut entries = Vec::new();
    let mut total_lines = 0usize;
    let mut file_line = 0usize;

    while let Some(raw) = lines.next_line().await? {
        file_line += 1;
        if raw.trim().is_empty() {
            continue;
        }

        let index = total_lines;
        total_lines += 1;
        if index >= offset && entries.len() < limit {
            entries.push(parse_entry(file_line, &raw));
        }
    }

    Ok(TranscriptPage {
        entries,
        offset,
        total_lines,
    })
}

async fn open_transcript(log_path: &Path) -> Result<tokio::fs::File> {
    tokio::fs::File::open(log_path).await.map_err(|error| {
        Error::not_found(format!(
            "could not open the transcript at {}: {error}",
            log_path.display()
        ))
    })
}

fn parse_entry(line: usize, raw: &str) -> TranscriptEntry {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return TranscriptEntry {
            line,
            kind: TranscriptEntryKind::Malformed {
                raw: preview(raw, MAX_MALFORMED_PREVIEW_CHARS),
            },
        };
    };

    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let kind = match event_type {
        "assistant" => TranscriptEntryKind::Assistant {
            blocks: content_blocks(&value),
        },
        "user" => TranscriptEntryKind::User {
            blocks: content_blocks(&value),
        },
        "result" => TranscriptEntryKind::Result {
            summary: value
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_string),
            errors: value
                .get("errors")
                .and_then(Value::as_array)
                .map(|errors| {
                    errors
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            is_error: value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        other => TranscriptEntryKind::Other {
            event_type: other.to_string(),
            subtype: value
                .get("subtype")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
    };

    TranscriptEntry { line, kind }
}

/// The first `limit` *characters* of `raw`, marked when it had to cut.
///
/// Characters, never bytes, for the reason [`matching_snippet`] gives: a cut
/// mid-codepoint is a panic, and a truncated line is exactly where invalid
/// UTF-8 boundaries turn up.
fn preview(raw: &str, limit: usize) -> String {
    let mut preview: String = raw.chars().take(limit).collect();
    if raw.chars().nth(limit).is_some() {
        preview.push('…');
    }
    preview
}

fn content_blocks(event: &Value) -> Vec<TranscriptBlock> {
    event
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .map(|blocks| blocks.iter().map(parse_block).collect())
        .unwrap_or_default()
}

fn parse_block(block: &Value) -> TranscriptBlock {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => TranscriptBlock::Text {
            text: block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        Some("tool_use") => match (
            block.get("id").and_then(Value::as_str),
            block.get("name").and_then(Value::as_str),
        ) {
            (Some(id), Some(name)) => TranscriptBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            },
            // A tool call this module cannot name or correlate is not one the
            // viewer can render as a call — `runner::events::ContentBlock`
            // makes the identical choice for the identical reason.
            _ => TranscriptBlock::Other,
        },
        Some("tool_result") => match block.get("tool_use_id").and_then(Value::as_str) {
            Some(tool_use_id) => TranscriptBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                is_error: block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                content: tool_result_content(block),
            },
            None => TranscriptBlock::Other,
        },
        _ => TranscriptBlock::Other,
    }
}

/// `tool_result.content` is either a plain string or a list of blocks (a
/// text block being the only realistic member); either shape renders to one
/// string. Kept tolerant of both because neither this module nor
/// `runner::events` has ever observed the field directly — ADR-0004's rule
/// extends to a shape nobody has seen yet.
fn tool_result_content(block: &Value) -> Option<String> {
    match block.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => {
            let text = items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The summary — what a reader needs before reading 1800 lines
// ---------------------------------------------------------------------------

/// The handful of facts that explain a run's shape, read in one pass.
///
/// Every field here is already *in* the transcript, and every one of them was
/// unreadable in practice: the permission mode is inside a `system` event
/// among a thousand others, the refusals are spread across a hundred tool
/// results, and "there is no `result` line" is a fact about the whole file
/// that no page of it can show. A reviewer opening a failed run should not
/// have to page through a 4MB file to learn that the agent was never allowed
/// to run `git commit`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSummary {
    /// What the CLI reported it was actually running under, from `init` —
    /// not what Rimaia asked for. ADR-0012's distinction, and `process.rs`
    /// verifies the two agree; this is the one the transcript can prove.
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    /// Tool results refused for want of approval. See
    /// [`crate::runner::events::reports_a_permission_denial`] — display only.
    pub denied_tool_calls: usize,
    /// Whether the stream reached its terminal event. `false` means whatever
    /// the run's outcome says, it was inferred from an ending the CLI never
    /// described.
    pub ended_with_result: bool,
    /// Whether the last entry in the file is one that would not parse — the
    /// signature of a stream cut mid-write, as opposed to a malformed line in
    /// the middle of an otherwise complete run.
    pub ends_mid_line: bool,
    pub malformed_lines: usize,
}

/// Reads `log_path` once and answers [`TranscriptSummary`].
///
/// A whole-file scan, like [`read_page`]'s own total-count pass and for the
/// same reasons (see this module's header): bounded memory, one sequential
/// read, nothing retained. It parses each line only far enough to ask what it
/// is — the summary never holds a line it has finished with.
pub async fn summarize(log_path: &Path) -> Result<TranscriptSummary> {
    let file = open_transcript(log_path).await?;
    let mut lines = BufReader::new(file).lines();

    let mut summary = TranscriptSummary::default();
    while let Some(raw) = lines.next_line().await? {
        if raw.trim().is_empty() {
            continue;
        }

        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            summary.malformed_lines += 1;
            summary.ends_mid_line = true;
            continue;
        };
        // Only a *trailing* unparseable line means the stream was cut; one in
        // the middle is a line the CLI wrote badly and then carried on past.
        summary.ends_mid_line = false;

        match value.get("type").and_then(Value::as_str) {
            Some("result") => summary.ended_with_result = true,
            Some("system") if value.get("subtype").and_then(Value::as_str) == Some("init") => {
                summary.permission_mode = string_field(&value, "permissionMode");
                summary.model = string_field(&value, "model");
            }
            Some("user") if refused_tool_result(&value, &raw) => summary.denied_tool_calls += 1,
            _ => {}
        }
    }

    Ok(summary)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The display-side reading of the same condition
/// [`crate::runner::events::reports_a_permission_denial`] counts live, against
/// the raw line rather than the live event model — the two agree on the gate
/// (an errored `tool_result`) and on the phrase, which is defined once, there.
fn refused_tool_result(value: &Value, raw: &str) -> bool {
    let errored = value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_result")
                    && block.get("is_error").and_then(Value::as_bool) == Some(true)
            })
        });

    errored && crate::runner::events::line_reports_a_denial(raw)
}

// ---------------------------------------------------------------------------
// Search — inside tool inputs as well as assistant text
// ---------------------------------------------------------------------------

/// One line matching a [`search`] query.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// The same 1-based file line [`TranscriptEntry::line`] uses — what to
    /// show a reader, and what an "open in editor" link would point at.
    pub line: usize,
    /// Where this hit sits in the *entry* numbering [`TranscriptPage::offset`]
    /// advances over: 0-based, blank lines not counted, so
    /// `read_page(path, hit.entry, limit)` opens on the entry that matched.
    ///
    /// It is counted here, during the same scan that finds the hit, because
    /// no caller can convert `line` into it without re-reading the file:
    /// the two numbers only agree when the transcript holds no blank lines,
    /// and a viewer that jumped by `line` landed a hit's worth of blank lines
    /// short of the match.
    pub entry: usize,
    /// A bounded excerpt of the raw line, centred on the match — bounded so
    /// a hit inside a multi-megabyte tool input still returns a screenful of
    /// text rather than the whole field.
    pub snippet: String,
}

/// Finds `query` in `log_path`, case-insensitively, searching the **raw**
/// JSON text of every line rather than the parsed display model.
///
/// That is what satisfies "finds text inside tool inputs, not only in
/// assistant messages" (task 015's acceptance criteria): a tool call's input
/// is serialized JSON sitting right there in the line, and a substring match
/// against the raw text finds it regardless of which key it lives under —
/// there is no need to enumerate `TranscriptBlock` variants to know where to
/// look.
pub async fn search(log_path: &Path, query: &str) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let needle = query.to_lowercase();

    let file = open_transcript(log_path).await?;
    let mut lines = BufReader::new(file).lines();

    let mut hits = Vec::new();
    let mut line_number = 0usize;
    // Advanced exactly as `read_page` advances its own window: only over
    // non-blank lines, so the two agree on what "entry 41" means.
    let mut entry_index = 0usize;
    while let Some(raw) = lines.next_line().await? {
        line_number += 1;
        if hits.len() >= MAX_SEARCH_HITS {
            break;
        }
        if raw.trim().is_empty() {
            continue;
        }
        let entry = entry_index;
        entry_index += 1;
        if let Some(snippet) = matching_snippet(&raw, &needle) {
            hits.push(SearchHit {
                line: line_number,
                entry,
                snippet,
            });
        }
    }
    Ok(hits)
}

/// `Some` snippet centred on the first case-insensitive match of `needle` in
/// `raw`, or `None` when there isn't one.
///
/// Works in characters throughout, never bytes, so a match beside a
/// multi-byte character cannot slice mid-codepoint — the same discipline
/// `runner::events::clamp` uses for the identical reason.
fn matching_snippet(raw: &str, needle: &str) -> Option<String> {
    let lower = raw.to_lowercase();
    let byte_index = lower.find(needle)?;
    // Both `find` and this count run over `lower`; lowercasing changes the
    // byte length of a handful of characters outside the content this tool
    // ever produces (JSON, code, prose), so treating the two as
    // character-aligned is a known, accepted approximation rather than a
    // guaranteed exact one.
    let match_start = lower[..byte_index].chars().count();
    let match_len = needle.chars().count();

    let characters: Vec<char> = raw.chars().collect();
    let radius = MAX_SNIPPET_CHARS / 2;
    let start = match_start.saturating_sub(radius);
    let end = (match_start + match_len + radius).min(characters.len());

    let mut snippet: String = characters[start..end].iter().collect();
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < characters.len() {
        snippet.push('…');
    }
    Some(snippet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// A struct variant's own fields are not touched by the enum-level
    /// `rename_all` — only its tag is — so `Result` and `Other` each need
    /// their own `#[serde(rename_all = "camelCase")]` to keep `isError` and
    /// `eventType` camelCase on the wire. This pins the shape the frontend's
    /// `types.ts` mirror actually reads, the same way
    /// `runner::events::a_tail_snapshot_serializes_to_the_shape_the_shell_forwards`
    /// pins its own struct's wire shape.
    #[test]
    fn a_result_entry_keeps_is_error_camel_cased_on_the_wire() {
        let kind = TranscriptEntryKind::Result {
            summary: Some("Done.".to_string()),
            errors: Vec::new(),
            is_error: false,
        };

        assert_eq!(
            serde_json::to_value(&kind).expect("a DTO must always serialize"),
            json!({ "type": "result", "summary": "Done.", "errors": [], "isError": false })
        );
    }

    #[test]
    fn an_other_entry_keeps_event_type_camel_cased_on_the_wire() {
        let kind = TranscriptEntryKind::Other {
            event_type: "system".to_string(),
            subtype: Some("thinking_tokens".to_string()),
        };

        assert_eq!(
            serde_json::to_value(&kind).expect("a DTO must always serialize"),
            json!({ "type": "other", "eventType": "system", "subtype": "thinking_tokens" })
        );
    }

    #[test]
    fn a_tool_result_block_keeps_its_two_snake_case_fields_camel_cased() {
        let block = TranscriptBlock::ToolResult {
            tool_use_id: "toolu_1".to_string(),
            is_error: true,
            content: None,
        };

        assert_eq!(
            serde_json::to_value(&block).expect("a DTO must always serialize"),
            json!({ "kind": "tool_result", "toolUseId": "toolu_1", "isError": true, "content": null })
        );
    }

    fn write_lines(lines: &[&str]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir for a transcript");
        let path = dir.path().join("run.jsonl");
        std::fs::write(&path, lines.join("\n")).expect("write the fixture transcript");
        (dir, path)
    }

    const ASSISTANT_TEXT: &str = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Reading the file."}]}}"#;
    const ASSISTANT_TOOL_USE: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test"}}]}}"#;
    const USER_TOOL_RESULT: &str = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":false,"content":"ok"}]}}"#;
    const RESULT_SUCCESS: &str =
        r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","errors":[]}"#;

    #[tokio::test]
    async fn a_page_covers_only_the_requested_window_and_reports_the_total() {
        let (_dir, path) = write_lines(&[
            ASSISTANT_TEXT,
            ASSISTANT_TOOL_USE,
            USER_TOOL_RESULT,
            RESULT_SUCCESS,
        ]);

        let page = read_page(&path, 1, 2).await.expect("read a page");

        assert_eq!(page.offset, 1);
        assert_eq!(page.total_lines, 4);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].line, 2, "the second file line, 1-based");
        assert!(matches!(
            page.entries[0].kind,
            TranscriptEntryKind::Assistant { .. }
        ));
        assert!(matches!(
            page.entries[1].kind,
            TranscriptEntryKind::User { .. }
        ));
    }

    #[tokio::test]
    async fn blank_lines_do_not_count_as_entries_but_do_count_as_file_lines() {
        let (_dir, path) = write_lines(&[ASSISTANT_TEXT, "", RESULT_SUCCESS]);

        let page = read_page(&path, 0, 10).await.expect("read a page");

        assert_eq!(page.total_lines, 2, "the blank line is not an entry");
        assert_eq!(page.entries[1].line, 3, "but it still occupies file line 2");
    }

    #[tokio::test]
    async fn an_assistant_entry_keeps_its_text_block() {
        let (_dir, path) = write_lines(&[ASSISTANT_TEXT]);

        let page = read_page(&path, 0, 10).await.expect("read a page");

        let TranscriptEntryKind::Assistant { blocks } = &page.entries[0].kind else {
            panic!("expected an assistant entry");
        };
        assert_eq!(
            blocks,
            &vec![TranscriptBlock::Text {
                text: "Reading the file.".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn a_tool_use_block_keeps_its_full_input() {
        let (_dir, path) = write_lines(&[ASSISTANT_TOOL_USE]);

        let page = read_page(&path, 0, 10).await.expect("read a page");

        let TranscriptEntryKind::Assistant { blocks } = &page.entries[0].kind else {
            panic!("expected an assistant entry");
        };
        assert_eq!(
            blocks,
            &vec![TranscriptBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({ "command": "cargo test" }),
            }]
        );
    }

    #[tokio::test]
    async fn a_tool_result_block_keeps_its_content_unlike_the_live_tail() {
        let (_dir, path) = write_lines(&[USER_TOOL_RESULT]);

        let page = read_page(&path, 0, 10).await.expect("read a page");

        let TranscriptEntryKind::User { blocks } = &page.entries[0].kind else {
            panic!("expected a user entry");
        };
        assert_eq!(
            blocks,
            &vec![TranscriptBlock::ToolResult {
                tool_use_id: "toolu_1".to_string(),
                is_error: false,
                content: Some("ok".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn a_result_entry_carries_its_summary_and_errors() {
        let (_dir, path) = write_lines(&[RESULT_SUCCESS]);

        let page = read_page(&path, 0, 10).await.expect("read a page");

        assert_eq!(
            page.entries[0].kind,
            TranscriptEntryKind::Result {
                summary: Some("Done.".to_string()),
                errors: Vec::new(),
                is_error: false,
            }
        );
    }

    /// The subtype rides along, because a real transcript is mostly `system`
    /// and its subtypes are nine unrelated things — a token counter, a hook's
    /// exit code, the run's own configuration. One label for all of them told
    /// a reader nothing.
    #[tokio::test]
    async fn an_event_type_this_viewer_does_not_render_keeps_its_subtype() {
        let (_dir, path) = write_lines(&[
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":100}"#,
            r#"{"type":"tool_progress"}"#,
        ]);

        let page = read_page(&path, 0, 10).await.expect("read a page");

        assert_eq!(
            page.entries[0].kind,
            TranscriptEntryKind::Other {
                event_type: "system".to_string(),
                subtype: Some("thinking_tokens".to_string()),
            }
        );
        assert_eq!(
            page.entries[1].kind,
            TranscriptEntryKind::Other {
                event_type: "tool_progress".to_string(),
                subtype: None,
            },
            "an event with no subtype says so rather than inventing one",
        );
    }

    #[tokio::test]
    async fn a_line_that_is_not_json_is_kept_verbatim_rather_than_reduced_to_a_complaint() {
        let (_dir, path) = write_lines(&[
            r#"{"type":"assistant","message":{"content":[{"text":"I am stopping here because"#,
        ]);

        let page = read_page(&path, 0, 10).await.expect("read a page");

        assert_eq!(page.total_lines, 1);
        let TranscriptEntryKind::Malformed { raw } = &page.entries[0].kind else {
            panic!("expected a malformed entry");
        };
        assert!(
            raw.contains("I am stopping here because"),
            "the text a cut-off line was carrying is the whole reason to show it: {raw}",
        );
    }

    #[test]
    fn a_malformed_preview_is_bounded_and_marks_where_it_was_cut() {
        let long = "x".repeat(MAX_MALFORMED_PREVIEW_CHARS + 100);

        let preview = preview(&long, MAX_MALFORMED_PREVIEW_CHARS);

        assert_eq!(preview.chars().count(), MAX_MALFORMED_PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn a_preview_that_fits_is_not_marked_as_cut() {
        assert_eq!(preview("short", MAX_MALFORMED_PREVIEW_CHARS), "short");
    }

    const INIT: &str = r#"{"type":"system","subtype":"init","permissionMode":"acceptEdits","model":"claude-sonnet-5"}"#;
    const DENIED_TOOL_RESULT: &str = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","is_error":true,"content":"This command requires approval"}]}}"#;
    const FAILED_TOOL_RESULT: &str = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_3","is_error":true,"content":"error: could not compile `rimaia-core`"}]}}"#;

    /// The run this was written for, in five lines: it was refused everything
    /// it tried, then its stream stopped mid-message without a `result`. Every
    /// one of those facts was already in the transcript and none of them was
    /// findable by paging through it.
    #[tokio::test]
    async fn a_summary_names_the_permission_mode_the_refusals_and_the_missing_result() {
        let (_dir, path) = write_lines(&[
            INIT,
            ASSISTANT_TOOL_USE,
            DENIED_TOOL_RESULT,
            FAILED_TOOL_RESULT,
            DENIED_TOOL_RESULT,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I am stopping"#,
        ]);

        let summary = summarize(&path).await.expect("summarize");

        assert_eq!(summary.permission_mode.as_deref(), Some("acceptEdits"));
        assert_eq!(summary.model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(
            summary.denied_tool_calls, 2,
            "an ordinary tool failure is not a refusal",
        );
        assert!(!summary.ended_with_result);
        assert!(summary.ends_mid_line);
        assert_eq!(summary.malformed_lines, 1);
    }

    #[tokio::test]
    async fn a_run_that_reached_its_result_says_so_and_counts_no_refusals() {
        let (_dir, path) = write_lines(&[INIT, ASSISTANT_TEXT, RESULT_SUCCESS]);

        let summary = summarize(&path).await.expect("summarize");

        assert!(summary.ended_with_result);
        assert!(!summary.ends_mid_line);
        assert_eq!(summary.denied_tool_calls, 0);
        assert_eq!(summary.malformed_lines, 0);
    }

    /// A bad line the CLI wrote and then carried on past is not a cut stream —
    /// only a *trailing* one is, and conflating them would put "the CLI
    /// stopped writing" on a run that finished perfectly well.
    #[tokio::test]
    async fn a_malformed_line_in_the_middle_does_not_read_as_a_cut_stream() {
        let (_dir, path) = write_lines(&[INIT, "{ not json", ASSISTANT_TEXT, RESULT_SUCCESS]);

        let summary = summarize(&path).await.expect("summarize");

        assert_eq!(summary.malformed_lines, 1);
        assert!(!summary.ends_mid_line);
        assert!(summary.ended_with_result);
    }

    #[tokio::test]
    async fn summarizing_a_transcript_that_does_not_exist_is_a_not_found_error() {
        let dir = tempfile::tempdir().expect("temp dir");

        let error = summarize(&dir.path().join("gone.jsonl"))
            .await
            .expect_err("the file is missing");

        assert_eq!(error.code(), crate::ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn reading_a_transcript_that_does_not_exist_is_a_not_found_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("gone.jsonl");

        let error = read_page(&missing, 0, 10)
            .await
            .expect_err("the file is missing");

        assert_eq!(error.code(), crate::ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn search_finds_a_match_inside_assistant_text() {
        let (_dir, path) = write_lines(&[ASSISTANT_TEXT, RESULT_SUCCESS]);

        let hits = search(&path, "reading the file").await.expect("search");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].entry, 0);
        assert!(hits[0].snippet.to_lowercase().contains("reading the file"));
    }

    /// The bug this pins: a viewer that treated `line` as a page offset
    /// scrolled to the wrong entry on any transcript with a blank line in it,
    /// and every real 4MB transcript has one. `entry` is the number
    /// `read_page` counts, so the two coordinate systems never have to be
    /// converted in the frontend.
    #[tokio::test]
    async fn a_hit_after_blank_lines_reports_the_entry_that_holds_it_not_the_file_line() {
        let (_dir, path) = write_lines(&[ASSISTANT_TEXT, "", "", ASSISTANT_TOOL_USE]);

        let hits = search(&path, "cargo test").await.expect("search");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 4, "the fourth line of the file");
        assert_eq!(hits[0].entry, 1, "but only the second entry");

        let page = read_page(&path, hits[0].entry, 1)
            .await
            .expect("open the page the hit reports");
        assert_eq!(
            page.entries[0].line, 4,
            "opening on the hit's entry shows the line that matched",
        );
    }

    #[tokio::test]
    async fn search_finds_a_match_inside_a_tool_inputs_own_fields() {
        // The whole point: a match inside `input.command`, a field this
        // module's display model would only expose as a nested JSON value,
        // is still found because the raw line is searched directly.
        let (_dir, path) = write_lines(&[ASSISTANT_TOOL_USE]);

        let hits = search(&path, "cargo test").await.expect("search");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[0].entry, 0);
    }

    #[tokio::test]
    async fn search_is_case_insensitive() {
        let (_dir, path) = write_lines(&[ASSISTANT_TEXT]);

        let hits = search(&path, "READING").await.expect("search");

        assert_eq!(hits.len(), 1);
    }

    #[tokio::test]
    async fn search_stops_at_the_hit_cap() {
        let lines: Vec<String> = (0..MAX_SEARCH_HITS + 20)
            .map(|i| format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"needle {i}"}}]}}}}"#))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let (_dir, path) = write_lines(&refs);

        let hits = search(&path, "needle").await.expect("search");

        assert_eq!(hits.len(), MAX_SEARCH_HITS);
    }

    #[tokio::test]
    async fn a_blank_query_finds_nothing_rather_than_matching_every_line() {
        let (_dir, path) = write_lines(&[ASSISTANT_TEXT]);

        let hits = search(&path, "   ").await.expect("search");

        assert!(hits.is_empty());
    }

    #[test]
    fn a_snippet_is_bounded_and_marks_where_it_was_cut() {
        let long_field = "x".repeat(MAX_SNIPPET_CHARS * 3);
        let raw = format!("prefix {long_field} needle {long_field} suffix");

        let snippet = matching_snippet(&raw, "needle").expect("a match");

        assert!(snippet.len() < raw.len());
        assert!(snippet.contains("needle"));
        assert!(snippet.starts_with('…'));
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn a_snippet_near_the_start_of_the_line_is_not_marked_as_cut_there() {
        let raw = format!("needle {}", "x".repeat(MAX_SNIPPET_CHARS));

        let snippet = matching_snippet(&raw, "needle").expect("a match");

        assert!(snippet.starts_with("needle"));
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn no_match_produces_no_snippet() {
        assert_eq!(matching_snippet("nothing here", "needle"), None);
    }
}
