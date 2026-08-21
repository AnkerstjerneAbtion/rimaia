//! The event stream, replayed from the recorded corpus (ADR-0004, ADR-0013,
//! seam-contract D14, task 008).
//!
//! Every test here drives real Claude Code output that a real `claude` process
//! actually produced. The CLI is faked by replaying recorded streams, never by
//! mocking a trait (ADR-0015) — so a parser that only works against output we
//! imagined would fail here rather than at 2am.
//!
//! What is *not* here: any assertion about a usage limit that has actually been
//! hit. `spike/FINDINGS.md` §4 records that the payload when
//! `rate_limit_info.status` is something other than `"allowed"` was never
//! observed, and there is no `usage_limit` fixture. Hand-inventing one would
//! fabricate a contract that the classifier could then be written to "pass"
//! against, in the module ADR-0011 calls the one most likely to break on a CLI
//! update. So the tests below assert the three field *names* the whole corpus
//! proves, and stop there.

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::runner::events::{
    parse_line, Activity, EventStream, InitEvent, McpServer, OtherEvent, RunEvent, RunTail,
    RECENT_ACTIVITY_CAPACITY,
};
use rimaia_core::testing::fixtures::{all_fixtures, fixture_lines, fixture_path};
use rimaia_core::testing::TestContext;
use rimaia_core::AppPaths;
use serde_json::Value;
use tokio::sync::broadcast::error::TryRecvError;

/// Ids a real run would take off its `runs` row; the transcript path is a pure
/// function of the pair (ADR-0013).
const TASK_ID: &str = "1f2b0a5c-0000-4000-8000-000000000001";
const RUN_ID: &str = "1f2b0a5c-0000-4000-8000-000000000002";

// ---------------------------------------------------------------------------
// The corpus, whole
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_recorded_scenario_replays_without_the_stream_ever_refusing_a_line() {
    // Iterating rather than naming scenarios is task 019's property: adding a
    // fixture must require no change outside the fixtures directory. It is also
    // the only test that will notice when the next recording carries something
    // nobody has thought about yet.
    let names = all_fixtures();
    assert!(!names.is_empty(), "the corpus is the harness");

    for name in &names {
        let replay = Replay::of(name).await;
        let lines = fixture_lines(name).count();

        assert!(
            !replay.events.is_empty(),
            "{name} produced no events at all"
        );
        assert_eq!(
            replay.events.len() + replay.malformed_lines as usize,
            lines,
            "{name}: every line is either an event or a skipped line, never neither"
        );
    }
}

#[tokio::test]
async fn a_transcript_is_the_stream_verbatim() {
    // ADR-0013's transcript is evidence, not a projection of what Rimaia
    // understood — which is why the comparison is bytes and includes the
    // fixtures carrying a line the parser had to skip.
    for name in all_fixtures() {
        let recorded = std::fs::read_to_string(fixture_path(&name)).expect("a readable fixture");
        let replay = Replay::of_lines(raw_lines(&recorded)).await;

        // The one permitted difference: `truncated-stream.jsonl` ends without a
        // newline, because its writer was killed mid-line. A transcript always
        // terminates its last line — that is what keeps the file valid JSONL.
        let expected = match recorded.ends_with('\n') {
            true => recorded.clone(),
            false => format!("{recorded}\n"),
        };

        assert_eq!(
            replay.transcript(),
            expected,
            "{name} did not survive the round trip byte for byte"
        );
    }
}

#[tokio::test]
async fn a_successful_run_transcript_is_byte_identical_to_the_recording() {
    let recorded = std::fs::read_to_string(fixture_path("success")).expect("a readable fixture");

    let replay = Replay::of_lines(raw_lines(&recorded)).await;

    assert_eq!(replay.transcript(), recorded);
}

#[tokio::test]
async fn no_event_type_is_ever_read_out_of_a_nested_content_block() {
    // Spike §3, as a property over the whole corpus: naive substring matching on
    // `"type":"` mis-parses, because an assistant event nests `"type":"message"`
    // inside its payload and `"type":"tool_use"` inside that. If any of those
    // words ever surfaces as an *event* type, the parser is matching on the
    // wrong nesting level.
    const NESTED_ONLY: [&str; 5] = ["message", "tool_use", "tool_result", "text", "thinking"];

    for name in all_fixtures() {
        for event in Replay::of(&name).await.events {
            assert!(
                !NESTED_ONLY.contains(&event.event_type()),
                "{name}: `{}` is a content-block type, not an event type",
                event.event_type()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tolerance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_malformed_line_is_skipped_and_every_event_after_it_still_arrives() {
    let replay = Replay::of("malformed-line").await;

    assert_eq!(replay.malformed_lines, 1);
    // The bad line is the seventh; the twenty-two either side of it are events,
    // and the stream still reaches its terminal one.
    assert_eq!(replay.events.len(), 22);
    assert!(
        matches!(replay.events.last(), Some(RunEvent::Result(_))),
        "the stream after the bad line must still terminate"
    );
    assert_eq!(
        replay
            .stream
            .result()
            .and_then(|result| result.terminal_reason.clone())
            .as_deref(),
        Some("completed")
    );
}

#[tokio::test]
async fn a_line_the_parser_had_to_skip_is_still_in_the_transcript() {
    let replay = Replay::of("malformed-line").await;

    let transcript = replay.transcript();
    let unreadable: Vec<&str> = transcript
        .lines()
        .filter(|line| serde_json::from_str::<Value>(line).is_err())
        .collect();

    assert_eq!(
        unreadable.len(),
        1,
        "the transcript is evidence: a line Rimaia could not read is still what the CLI said"
    );
}

#[tokio::test]
async fn an_unknown_event_type_survives_with_its_whole_document() {
    let replay = Replay::of("unknown-event-type").await;

    let telemetry = replay
        .others()
        .find(|other| other.event_type == "telemetry_ping")
        .expect("an unmodelled event type must arrive, not disappear");

    assert_eq!(telemetry.subtype, None);
    assert_eq!(telemetry.raw["payload"]["heartbeat"], Value::Bool(true));
    assert!(
        matches!(replay.events.last(), Some(RunEvent::Result(_))),
        "tolerating an unknown event must not cost the terminal one"
    );
}

#[tokio::test]
async fn an_unknown_system_subtype_stays_opaque_rather_than_becoming_an_init() {
    let replay = Replay::of("unknown-event-type").await;

    let compaction = replay
        .others()
        .find(|other| other.subtype.as_deref() == Some("context_compaction"))
        .expect("an unmodelled system subtype must arrive, not disappear");

    assert_eq!(compaction.event_type, "system");
    // Only `init` is modelled; a CLI that adds a subtype tomorrow must not be
    // able to overwrite the applied configuration this run verified.
    assert_eq!(
        replay.init().permission_mode.as_deref(),
        Some("bypassPermissions")
    );
}

#[tokio::test]
async fn a_truncated_stream_reaches_no_result_and_is_not_an_error() {
    // A writer killed mid-line: the run has no outcome in its own stream, which
    // is a different condition from a run that ended badly and said so.
    let replay = Replay::of("truncated-stream").await;

    assert_eq!(replay.stream.result(), None);
    assert_eq!(replay.malformed_lines, 1);
    assert!(replay
        .events
        .iter()
        .all(|event| !matches!(event, RunEvent::Result(_))));
    // Everything before the truncation is still usable evidence.
    assert_eq!(replay.init().model.as_deref(), Some("claude-sonnet-5"));
}

// ---------------------------------------------------------------------------
// The typed events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_assistant_event_is_read_from_its_own_type_not_its_messages() {
    let line = first_line_of_type("success", "assistant");
    let raw: Value = serde_json::from_str(&line).expect("the fixture line is JSON");

    // The trap really is in the data: this document says `"type":"message"` one
    // level down, and `"type":"thinking"` or `"tool_use"` two levels down.
    assert_eq!(raw["message"]["type"], "message");

    let event = parse_line(&line).expect("the line is JSON");

    assert_eq!(event.event_type(), "assistant");
    let RunEvent::Assistant(assistant) = event else {
        panic!("the nested `message` type was mistaken for the event type");
    };
    assert_eq!(
        assistant.session_id.as_deref(),
        Some("c6529619-d49b-479b-b4ce-a97cad085fda")
    );
}

#[tokio::test]
async fn the_init_event_reports_the_isolation_that_was_actually_applied() {
    // Task 008's acceptance criterion, and the measurement in `spike/FINDINGS.md`
    // §2 that made `run_environment` a setting: same one-word prompt, 255 tools
    // and two of the operator's MCP servers inherited against 26 and none
    // isolated. The runner verifies rather than assumes, so the parser has to
    // surface both lists.
    let inherited = Replay::of("env-leak-default-settings").await;
    let isolated = Replay::of("env-leak-isolated-settings").await;

    assert_eq!(inherited.init().tools.len(), 255);
    assert_eq!(
        inherited
            .init()
            .mcp_servers
            .iter()
            .map(|server| server.name.clone())
            .collect::<Vec<_>>(),
        vec!["Brewale".to_string(), "claude.ai Google Drive".to_string()]
    );

    assert_eq!(isolated.init().tools.len(), 26);
    assert_eq!(isolated.init().mcp_servers, Vec::<McpServer>::new());

    for init in [inherited.init(), isolated.init()] {
        // `none` is the CLI confirming subscription auth rather than a metered
        // API key, which is ADR-0004's premise.
        assert_eq!(init.api_key_source.as_deref(), Some("none"));
    }
}

#[tokio::test]
async fn the_usage_limit_signal_is_read_from_its_own_fields_rather_than_grepped() {
    // ADR-0011's amendment: a typed event on every run, early and unprompted —
    // not an error message to pattern-match. Only `status` is asserted by value,
    // and only because `"allowed"` is the one value the corpus contains; see
    // this file's header and `spike/FINDINGS.md` §4 for why nothing else is.
    let replay = Replay::of("success").await;
    let limit = replay
        .stream
        .rate_limit()
        .expect("every recorded run reports its limit state");

    assert_eq!(limit.status.as_deref(), Some("allowed"));
    assert_eq!(limit.rate_limit_type.as_deref(), Some("five_hour"));
    assert_eq!(limit.resets_at, Some(1_787_224_800));
    assert_eq!(limit.resets_at_utc(), Some(at("2026-08-20T11:20:00Z")));
}

#[tokio::test]
async fn a_successful_result_carries_every_metric_the_runs_row_needs() {
    // `spike/FINDINGS.md` §6: turns, cost and duration all arrive on the
    // terminal event. Nothing is derived.
    let replay = Replay::of("success").await;
    let result = replay.stream.result().expect("a terminal result");

    assert_eq!(result.subtype.as_deref(), Some("success"));
    assert_eq!(result.terminal_reason.as_deref(), Some("completed"));
    assert!(!result.is_error);
    assert_eq!(result.num_turns, Some(5));
    assert_eq!(result.total_cost_usd, Some(0.150_292_5));
    assert_eq!(result.duration_ms, Some(21_427));
    assert!(result.errors.is_empty());
    assert!(result.result.is_some(), "a success states what it did");
}

#[tokio::test]
async fn a_sigterm_killed_run_still_produces_a_result_event() {
    // The spike's most load-bearing finding (§5): the stream does not simply
    // stop, which is what ADR-0011 originally assumed. Everything that
    // classifies on `terminal_reason` instead of the exit code rests on this.
    let replay = Replay::of("interrupted-sigterm").await;
    let result = replay.stream.result().expect("a terminal result");

    assert_eq!(result.subtype.as_deref(), Some("error_during_execution"));
    assert_eq!(result.terminal_reason.as_deref(), Some("aborted_streaming"));
    assert!(result.is_error);
    assert_eq!(result.result, None);
    assert!(!result.errors.is_empty(), "an error subtype says why");
}

#[tokio::test]
async fn a_turn_limit_reports_max_turns_rather_than_a_bare_non_zero_exit() {
    let replay = Replay::of("max-turns").await;
    let result = replay.stream.result().expect("a terminal result");

    assert_eq!(result.subtype.as_deref(), Some("error_max_turns"));
    assert_eq!(result.terminal_reason.as_deref(), Some("max_turns"));
    assert_eq!(result.errors, vec!["Reached maximum number of turns (2)"]);
}

// ---------------------------------------------------------------------------
// The ring buffer and the tail (seam-contract D14)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_ring_buffer_stops_growing_at_its_capacity() {
    // Fed the same recorded run over and over: far more activity than the buffer
    // holds, in a process that has to survive a night of it.
    let single = Replay::of("success").await.activities();
    assert!(
        !single.is_empty(),
        "the recording has to produce activity at all"
    );

    let passes = RECENT_ACTIVITY_CAPACITY.div_ceil(single.len()) + 4;
    let repeated: Vec<String> = (0..passes).flat_map(|_| fixture_lines("success")).collect();
    let flooded = Replay::of_lines(repeated).await.activities();

    assert!(
        single.len() * passes > RECENT_ACTIVITY_CAPACITY,
        "the flood has to overflow the buffer or this proves nothing"
    );
    assert_eq!(flooded.len(), RECENT_ACTIVITY_CAPACITY);
    // What survives is the newest end, which is what "recent activity" means.
    assert_eq!(flooded.last(), single.last());
}

#[tokio::test]
async fn a_tail_subscriber_receives_what_the_parser_produced() {
    let mut replay = Replay::of("success").await;

    let published = replay.published_tail();
    assert!(
        !published.is_empty(),
        "a run in flight has to say something"
    );

    // The last snapshot is the run as it finished, and it agrees with the
    // progress the parser folded up.
    assert_eq!(published.last(), Some(&replay.stream.progress().tail()));
    // Something the agent actually did reached a watcher.
    assert!(
        published.iter().any(|tail| tail
            .current_tool
            .as_ref()
            .is_some_and(|call| call.name == "Read")),
        "the live view never saw the current tool call"
    );
    assert!(published
        .iter()
        .any(|tail| tail.last_assistant_text.is_some()));
}

#[tokio::test]
async fn the_turn_count_defers_to_the_number_the_result_event_reports() {
    // Seam-contract D14 rule 2, at the one number where the tail and the `runs`
    // row could visibly disagree: the live count is an approximation over
    // assistant message ids, and the CLI's own `num_turns` replaces it the
    // moment it arrives. The two genuinely differ in this recording — seven
    // distinct assistant messages against nine turns — so this is a real
    // override and not a coincidence that would survive deleting it.
    let mut lines: Vec<String> = fixture_lines("interrupted-sigterm").collect();
    let terminal = lines.pop().expect("a terminal result");

    let mut replay = Replay::of_lines(lines).await;
    assert_eq!(replay.stream.progress().turns(), 7);

    replay.observe(&terminal);

    assert_eq!(replay.stream.result().expect("a result").num_turns, Some(9));
    assert_eq!(replay.stream.progress().turns(), 9);
}

#[tokio::test]
async fn elapsed_time_is_read_off_the_injected_clock() {
    // No sleep anywhere: the run "takes" four minutes because the test says so.
    let mut replay = Replay::empty().await;
    replay.harness.clock.advance(Duration::minutes(4));

    for line in fixture_lines("success") {
        replay.observe(&line);
    }

    assert_eq!(
        replay.stream.progress().elapsed_ms(),
        Duration::minutes(4).num_milliseconds()
    );
    assert_eq!(
        replay
            .published_tail()
            .last()
            .expect("a snapshot")
            .elapsed_ms,
        Duration::minutes(4).num_milliseconds()
    );
}

// ---------------------------------------------------------------------------
// The files on disk
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_transcript_lands_where_adr_0013_says_it_does() {
    let replay = Replay::of("success").await;

    assert_eq!(
        replay.stream.transcript_path(),
        replay
            .paths
            .runs_dir()
            .join(TASK_ID)
            .join(format!("{RUN_ID}.jsonl"))
    );
}

#[tokio::test]
async fn stderr_is_captured_beside_the_transcript_and_not_inside_it() {
    let mut replay = Replay::of("success").await;

    replay
        .stream
        .observe_stderr("node:internal/errors: something went wrong")
        .expect("stderr stays writable");

    assert_eq!(
        std::fs::read_to_string(replay.stream.stderr_path()).expect("a captured stderr file"),
        "node:internal/errors: something went wrong\n"
    );
    // The `.jsonl` stays valid JSONL, which is the reason the two are separate.
    for line in replay.transcript().lines() {
        serde_json::from_str::<Value>(line)
            .unwrap_or_else(|error| panic!("stderr leaked into the transcript: {error}"));
    }
}

#[tokio::test]
async fn a_run_that_writes_no_stderr_leaves_no_file_behind() {
    let replay = Replay::of("success").await;

    assert!(replay.stream.stderr_is_empty());
    assert!(!replay.stream.stderr_path().exists());
}

#[tokio::test]
async fn a_transcript_is_readable_at_every_point_of_a_run_not_only_at_its_end() {
    // Task 008's acceptance criterion — the transcript survives force-quitting
    // the app mid-run — restated as what it actually requires: after each line,
    // without any flush the caller had to remember, what is on disk is complete
    // JSONL up to that line.
    let mut replay = Replay::empty().await;

    for (index, line) in fixture_lines("success").enumerate() {
        replay.observe(&line);

        let so_far = replay.transcript();
        assert_eq!(so_far.lines().count(), index + 1);
        assert!(so_far.ends_with('\n'), "a half-written line is not JSONL");
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One run's stream, replayed into a real temporary run directory.
struct Replay {
    /// Held only for its `Drop`; [`paths`](Self::paths) points inside it.
    _root: tempfile::TempDir,
    harness: TestContext,
    paths: AppPaths,
    stream: EventStream,
    events: Vec<RunEvent>,
    malformed_lines: u64,
    tail: tokio::sync::broadcast::Receiver<RunTail>,
}

impl Replay {
    async fn of(fixture: &str) -> Self {
        Self::of_lines(fixture_lines(fixture)).await
    }

    /// A stream that has seen nothing yet, for a test that wants to drive the
    /// clock or the transcript between lines.
    async fn empty() -> Self {
        Self::of_lines(Vec::<String>::new()).await
    }

    async fn of_lines(lines: impl IntoIterator<Item = String>) -> Self {
        let harness = TestContext::new().await;
        let root = tempfile::Builder::new()
            .prefix("rimaia-runs-")
            .tempdir()
            .expect("temp dir for the run directory");
        let paths = AppPaths::new(root.path());

        // Subscribed before the first line, because broadcast delivers only to
        // receivers that already existed — the same trap `TestContext` documents
        // for change events.
        let tail = harness.context.subscribe_tail();
        let stream = EventStream::create(&harness.context, &paths, TASK_ID, RUN_ID)
            .expect("the run directory is creatable");

        let mut replay = Self {
            _root: root,
            harness,
            paths,
            stream,
            events: Vec::new(),
            malformed_lines: 0,
            tail,
        };
        for line in lines {
            replay.observe(&line);
        }
        replay
    }

    fn observe(&mut self, line: &str) {
        if let Some(event) = self
            .stream
            .observe(line)
            .expect("the transcript stays writable")
        {
            self.events.push(event);
        }
        self.malformed_lines = self.stream.malformed_lines();
    }

    fn transcript(&self) -> String {
        std::fs::read_to_string(self.stream.transcript_path()).expect("a written transcript")
    }

    fn init(&self) -> &InitEvent {
        self.stream.init().expect("a recorded run announces itself")
    }

    fn others(&self) -> impl Iterator<Item = &OtherEvent> {
        self.events.iter().filter_map(|event| match event {
            RunEvent::Other(other) => Some(other),
            _ => None,
        })
    }

    fn activities(&self) -> Vec<Activity> {
        self.stream.progress().recent().cloned().collect()
    }

    /// Every snapshot a watcher subscribed from the start would have seen.
    fn published_tail(&mut self) -> Vec<RunTail> {
        let mut published = Vec::new();
        loop {
            match self.tail.try_recv() {
                Ok(tail) => published.push(tail),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                Err(TryRecvError::Lagged(dropped)) => {
                    panic!("the tail channel dropped {dropped} snapshots in a test")
                }
            }
        }
        published
    }
}

/// The recording split the way a line reader would split it, keeping a final
/// line that has no terminator — which is exactly what `truncated-stream.jsonl`
/// is.
fn raw_lines(recorded: &str) -> Vec<String> {
    recorded.split_terminator('\n').map(str::to_owned).collect()
}

fn first_line_of_type(fixture: &str, event_type: &str) -> String {
    fixture_lines(fixture)
        .find(|line| {
            serde_json::from_str::<Value>(line)
                .is_ok_and(|event| event["type"] == Value::String(event_type.to_owned()))
        })
        .unwrap_or_else(|| panic!("{fixture} has no {event_type} event"))
}

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .expect("test timestamp must be valid RFC 3339")
        .with_timezone(&Utc)
}
