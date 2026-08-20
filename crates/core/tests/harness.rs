//! The fixture corpus and the test clock, from the outside.
//!
//! Structure only, deliberately. There is no event enum, no parser and no
//! classifier in this file: those belong to the runner (task 008), and a second
//! implementation living in the harness's own tests is exactly what would let a
//! bug in the real one pass unnoticed. Everything below asserts against
//! [`serde_json::Value`].
//!
//! What these tests defend is the corpus itself. The fixtures are byte-for-byte
//! recordings that nobody will read again once tasks 008 and 014 are green, so
//! the properties those tasks silently assume — a terminal `result` on every
//! real run, exactly one bad line in the malformed fixture, none at all in the
//! truncated one — are asserted here, where a careless re-recording trips them.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::clock::Clock;
use rimaia_core::testing::fixtures::{all_fixtures, fixture_lines, fixtures_dir};
use rimaia_core::testing::TestClock;
use serde_json::Value;

/// The scenarios captured from a real `claude` process. The synthesized ones are
/// listed nowhere: two of them are not valid streams, so every property below
/// would have to carve them out anyway.
const RECORDED: [&str; 6] = [
    "env-leak-default-settings",
    "env-leak-isolated-settings",
    "interrupted-sigterm",
    "max-turns",
    "resume-success",
    "success",
];

#[test]
fn all_fixtures_reports_every_jsonl_in_the_fixtures_directory() {
    // The expectation is read off the directory rather than written down, because
    // "adding a fixture requires no changes outside the fixtures directory" is
    // task 019's acceptance criterion and a hardcoded list is how it gets broken.
    let mut expected: Vec<String> = std::fs::read_dir(fixtures_dir())
        .expect("the fixtures directory must exist")
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.extension() == Some(OsStr::new("jsonl")))
        .map(|path| {
            path.file_stem()
                .expect("a .jsonl file has a stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    expected.sort();

    assert!(
        !expected.is_empty(),
        "no recorded scenarios under {} — the corpus is the harness",
        fixtures_dir().display()
    );
    assert_eq!(all_fixtures(), expected);
    assert!(
        RECORDED
            .iter()
            .all(|name| expected.contains(&name.to_string())),
        "a real recording went missing from {}",
        fixtures_dir().display()
    );
}

#[test]
fn every_recorded_scenario_is_line_delimited_json() {
    for name in RECORDED {
        let lines: Vec<String> = fixture_lines(name).collect();
        assert!(!lines.is_empty(), "{name} is empty");

        for (index, line) in lines.iter().enumerate() {
            let event: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{name} event {} is not JSON: {error}", index + 1));
            assert!(
                event.get("type").and_then(Value::as_str).is_some(),
                "{name} event {} has no top-level type to dispatch on",
                index + 1
            );
        }
    }
}

#[test]
fn every_recorded_scenario_ends_with_a_terminal_result_event() {
    for name in RECORDED {
        let last = last_event(name);

        assert_eq!(last["type"], "result", "{name} never terminated");
        // Classification reads `terminal_reason` and `subtype` together, never
        // the exit code alone (ADR-0004). A recording missing either leaves task
        // 014 with nothing to decide on.
        for key in ["terminal_reason", "subtype"] {
            assert!(
                last.get(key)
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.is_empty()),
                "{name}'s result carries no {key}"
            );
        }
    }
}

#[test]
fn a_sigterm_killed_run_still_ends_with_a_result_event() {
    // The spike's most load-bearing finding (spike/FINDINGS.md section 5): the
    // process exits 143 but still writes a terminal result first. Everything that
    // classifies on the stream instead of the exit status rests on this one
    // recording, so it gets an assertion of its own rather than only a place in
    // the loop above.
    let last = last_event("interrupted-sigterm");

    assert_eq!(last["type"], "result");
    assert_eq!(last["is_error"], true);
}

#[test]
fn the_malformed_fixture_has_exactly_one_line_a_parser_must_skip() {
    // Tolerant parsing (ADR-0004): the bad line must cost the events around it
    // nothing, so the corpus has to keep the damage to precisely one line.
    let lines: Vec<String> = fixture_lines("malformed-line").collect();

    let unparseable: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| serde_json::from_str::<Value>(line).is_err())
        .map(|(index, _)| index + 1)
        .collect();

    assert_eq!(unparseable, vec![6]);
    assert_eq!(
        last_event("malformed-line")["type"],
        "result",
        "the stream after the bad line must still terminate"
    );
}

#[test]
fn the_unknown_event_fixture_carries_a_type_and_a_subtype_no_recording_contains() {
    let (recorded_types, recorded_subtypes) = vocabulary_of(RECORDED);
    let (fixture_types, fixture_subtypes) = vocabulary_of(["unknown-event-type"]);

    assert!(fixture_types.contains("telemetry_ping"));
    assert!(
        !recorded_types.contains("telemetry_ping"),
        "the unfamiliar type has become familiar; the fixture no longer tests anything"
    );
    assert!(fixture_subtypes.contains("context_compaction"));
    assert!(
        !recorded_subtypes.contains("context_compaction"),
        "the unfamiliar subtype has become familiar; the fixture no longer tests anything"
    );
    assert_eq!(
        last_event("unknown-event-type")["type"],
        "result",
        "tolerating an unknown event must not cost the terminal one"
    );
}

#[test]
fn the_truncated_fixture_never_reaches_a_result_event() {
    // A writer killed mid-write: the run has no outcome in its own stream, which
    // is a different condition from a run that ended badly and said so.
    let lines: Vec<String> = fixture_lines("truncated-stream").collect();
    let parsed: Vec<Value> = lines
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    assert!(
        parsed.iter().all(|event| event["type"] != "result"),
        "the truncated stream must not terminate; that is the whole scenario"
    );
    assert_eq!(
        lines.len(),
        parsed.len() + 1,
        "only the final half-written line should fail to parse"
    );
}

#[test]
fn advancing_the_test_clock_moves_now_by_exactly_the_requested_duration() {
    let clock = TestClock::new(at("2026-08-20T02:00:00Z"));

    clock.advance(Duration::minutes(15));
    assert_eq!(clock.now(), at("2026-08-20T02:15:00Z"));

    clock.advance(Duration::seconds(1));
    assert_eq!(clock.now(), at("2026-08-20T02:15:01Z"));
}

#[test]
fn a_clock_injected_into_a_collaborator_sees_the_tests_own_advances() {
    // The sharing is the point of the type: task 014 drives a scheduler's
    // fifteen-minute backoff by advancing the handle the test kept, from outside,
    // with no sleep anywhere.
    let clock = TestClock::new(at("2026-08-20T02:00:00Z"));
    let injected: Arc<dyn Clock> = Arc::new(clock.clone());

    clock.advance(Duration::hours(4));

    assert_eq!(injected.now(), at("2026-08-20T06:00:00Z"));
    assert_eq!(injected.now(), clock.now());
}

/// The scenario's last event, parsed. Panics if the stream is empty or its final
/// line is not JSON — for the fixtures that call this, both are defects.
fn last_event(name: &str) -> Value {
    let last = fixture_lines(name)
        .last()
        .unwrap_or_else(|| panic!("{name} has no events"));

    serde_json::from_str(&last)
        .unwrap_or_else(|error| panic!("{name}'s last line is not JSON: {error}"))
}

/// Every `type` and every `subtype` appearing across `names`, so one fixture's
/// vocabulary can be compared against another's. Sets of strings, not a taxonomy
/// — the event model belongs to task 008.
fn vocabulary_of<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut types = BTreeSet::new();
    let mut subtypes = BTreeSet::new();

    for name in names {
        for line in fixture_lines(name) {
            let event: Value = serde_json::from_str(&line)
                .unwrap_or_else(|error| panic!("{name} has an unparseable line: {error}"));

            for (key, seen) in [("type", &mut types), ("subtype", &mut subtypes)] {
                if let Some(value) = event.get(key).and_then(Value::as_str) {
                    seen.insert(value.to_owned());
                }
            }
        }
    }

    (types, subtypes)
}

fn at(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339)
        .expect("test timestamp must be valid RFC 3339")
        .with_timezone(&Utc)
}
