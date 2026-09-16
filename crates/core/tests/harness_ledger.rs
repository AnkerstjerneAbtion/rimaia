//! The second corpus, from the outside (ADR-0026, seam-contract D27.6).
//!
//! Structure only, exactly as `harness.rs` is structure only: no event enum, no
//! parser, no classifier, everything asserted against
//! [`serde_json::Value`]. A second implementation of the parser living in the
//! harness's own tests is what would let a bug in the real one pass unnoticed,
//! and that argument does not become weaker for a second vocabulary.
//!
//! **It lives in its own file because the corpus lives in its own directory.**
//! Seven tests in `harness.rs` iterate the recordings asserting properties every
//! real recording has; a foreign file dropped in beside them breaks all seven,
//! and every repair is an exclusion list. So the two are siblings, `harness.rs`
//! was not loosened by a byte, and one test there —
//! `the_two_corpora_share_no_vocabulary` — keeps them apart.
//!
//! # What these defend, and what they cannot
//!
//! `harness.rs` defends *recordings*: bytes that came out of a real program, so
//! its assertions are about what was observed. **Nothing here was observed.**
//! These files were written by hand to be maximally unlike the recordings, and
//! what is defended is that they stay that way — a scenario edited to look more
//! like Claude Code is a scenario that has stopped testing the seam.

use std::collections::BTreeSet;

use pretty_assertions::assert_eq;
use rimaia_core::runner::provider::ProviderId;
use rimaia_core::testing::fixtures::{all_for, lines_for};
use serde_json::Value;

/// The eight scenarios, spelled out so a deletion fails here rather than
/// silently shrinking what the seam tests cover.
const SCENARIOS: [&str; 8] = [
    "budget",
    "continued",
    "finished",
    "stopped",
    "torn",
    "unknown-kind",
    "window-closed",
    "window-closed-no-reopen",
];

/// The two that are not valid streams end to end, and are not expected to be.
const NOT_WHOLE: [&str; 1] = ["torn"];

fn ledger() -> ProviderId {
    ProviderId::Ledger
}

fn lines(name: &str) -> Vec<String> {
    lines_for(ledger(), name).collect()
}

fn parsed(name: &str) -> Vec<Value> {
    lines(name)
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[test]
fn the_corpus_is_exactly_the_scenarios_the_seam_tests_name() {
    assert_eq!(all_for(ledger()), SCENARIOS);
}

#[test]
fn every_whole_scenario_is_line_delimited_json_dispatching_on_kind() {
    for name in SCENARIOS.iter().filter(|name| !NOT_WHOLE.contains(name)) {
        let lines = lines(name);
        assert!(!lines.is_empty(), "{name} is empty");

        for (index, line) in lines.iter().enumerate() {
            let event: Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{name} event {} is not JSON: {error}", index + 1));
            assert!(
                event.get("kind").and_then(Value::as_str).is_some(),
                "{name} event {} has no top-level kind to dispatch on",
                index + 1
            );
            assert!(
                event.get("body").is_some(),
                "{name} event {} carries no body",
                index + 1
            );
        }
    }
}

#[test]
fn every_whole_scenario_ends_with_a_finished_event_that_says_why() {
    for name in SCENARIOS.iter().filter(|name| !NOT_WHOLE.contains(name)) {
        let last = parsed(name)
            .pop()
            .unwrap_or_else(|| panic!("{name} is empty"));

        assert_eq!(last["kind"], "finished", "{name} never terminated");
        assert!(
            last["body"]["why"]
                .as_str()
                .is_some_and(|why| !why.is_empty()),
            "{name}'s ending carries no reason to classify on"
        );
    }
}

#[test]
fn the_torn_scenario_never_reaches_an_ending_and_its_last_line_is_unparseable() {
    // A writer killed mid-line. The run has no outcome in its own stream, which
    // is a different condition from a run that ended badly and said so — and it
    // is the second provider's version of the property `truncated-stream.jsonl`
    // holds for the first.
    let lines = lines("torn");
    let parsed = parsed("torn");

    assert!(
        parsed.iter().all(|event| event["kind"] != "finished"),
        "the torn stream must not terminate; that is the whole scenario"
    );
    assert_eq!(
        lines.len(),
        parsed.len() + 1,
        "only the final half-written line should fail to parse"
    );
}

#[test]
fn no_scenario_reports_a_cost_in_dollars() {
    // **Deliberate pressure on seam-contract D18.** This provider reports tokens
    // and never a price, so `runs.cost_usd` has to reach the column as NULL and
    // mean "not recorded" — all the way to the analytics that would otherwise
    // average a zero nobody paid.
    for name in SCENARIOS {
        for line in lines(name) {
            assert!(
                !line.contains("cost"),
                "{name} names a cost; this provider has never reported one",
            );
        }
    }
}

#[test]
fn every_window_this_corpus_reports_is_relative_rather_than_an_instant() {
    // The half of ADR-0026 point 7 the recordings cannot exercise. `reopens_in_s`
    // means "from the moment you read this line", so the instant it names depends
    // on when it was read — and resolving it at `finish_run` time instead would
    // be wrong by however long the run then took to die.
    let mut seen_a_reopen = false;

    for name in SCENARIOS {
        for event in parsed(name) {
            for window in [&event["body"], &event["body"]["window"]] {
                if window.get("reopens_in_s").is_some() {
                    seen_a_reopen = true;
                    assert!(
                        window["reopens_in_s"].as_i64().is_some(),
                        "{name}: a relative window is a number of seconds",
                    );
                }
                assert!(
                    window.get("resetsAt").is_none(),
                    "{name}: an absolute epoch is the other corpus's shape",
                );
            }
        }
    }

    assert!(
        seen_a_reopen,
        "no scenario reports a relative reopen, so the relative path is untested",
    );
}

#[test]
fn one_scenario_reports_the_wall_only_mid_run_and_one_repeats_a_window_on_its_ending() {
    // The two halves of ADR-0026 points 7 and 8, split so each scenario tests one
    // thing. `window-closed` reports the wall **once**, mid-run, with a countdown
    // — so the instant it names depends only on when that line was read, and
    // resolving it at the end instead would be wrong by however long the run then
    // took to die. `window-closed-no-reopen` reports the wall and then an *open*
    // window on its ending, which is the heartbeat a latch has to refuse.
    let windows = |name: &str| -> Vec<(String, bool)> {
        parsed(name)
            .iter()
            .filter_map(|event| match event["kind"].as_str() {
                Some("window") => Some(&event["body"]),
                Some("finished") if event["body"].get("window").is_some() => {
                    Some(&event["body"]["window"])
                }
                _ => None,
            })
            .map(|window| {
                (
                    window["state"].as_str().unwrap_or_default().to_string(),
                    window.get("reopens_in_s").is_some(),
                )
            })
            .collect()
    };

    assert_eq!(
        windows("window-closed"),
        [("open".to_string(), false), ("closed".to_string(), true),],
        "the ending must carry no window, or the countdown is simply restated later",
    );
    assert_eq!(
        windows("window-closed-no-reopen"),
        [
            ("open".to_string(), false),
            ("closed".to_string(), false),
            ("open".to_string(), false),
        ],
        "the ending reports the window open again, which is what a latch refuses",
    );
}

#[test]
fn one_scenario_reaches_a_wall_with_no_reopen_at_all() {
    // The fixed-poll fallback's input: a closed window that names no reopen, so
    // the policy has nothing to wait for and falls back to a poll rather than
    // waiting forever.
    let reopens: BTreeSet<bool> = parsed("window-closed-no-reopen")
        .iter()
        .filter(|event| event["kind"] == "window" || event["kind"] == "finished")
        .map(|event| {
            event["body"].get("reopens_in_s").is_some()
                || event["body"]["window"].get("reopens_in_s").is_some()
        })
        .collect();

    assert_eq!(reopens, BTreeSet::from([false]));
}

#[test]
fn the_continuation_scenario_names_the_conversation_the_wall_opened() {
    assert_eq!(
        parsed("continued")
            .first()
            .and_then(|event| event["body"]["conversation"].as_str()),
        Some("lgr_9d44be03"),
    );
}

#[test]
fn two_scenarios_continue_one_conversation_the_provider_minted_itself() {
    // The id is `lgr_…` and it is the provider's own. Rimaia's conversation id is
    // what `runs.session_id` carries, and these two files exist so a test can
    // prove the two are not the same string (ADR-0026 point 5).
    let announced = |name: &str| -> String {
        parsed(name)
            .iter()
            .find(|event| event["kind"] == "conversation.opened")
            .and_then(|event| event["body"]["conversation"].as_str())
            .unwrap_or_else(|| panic!("{name} does not announce a conversation"))
            .to_string()
    };

    let first = announced("window-closed");
    let second = announced("continued");

    assert!(first.starts_with("lgr_"), "{first}");
    assert_eq!(
        first, second,
        "the continuation must name the conversation the first attempt opened",
    );
}

#[test]
fn the_unknown_kind_scenario_carries_two_kinds_nothing_else_does() {
    let familiar: BTreeSet<String> = SCENARIOS
        .iter()
        .filter(|name| **name != "unknown-kind")
        .flat_map(|name| parsed(name))
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect();

    for unfamiliar in ["telemetry.beat", "compaction.ran"] {
        assert!(
            !familiar.contains(unfamiliar),
            "`{unfamiliar}` has become familiar; the fixture no longer tests anything",
        );
    }
    assert_eq!(
        parsed("unknown-kind")
            .last()
            .map(|event| event["kind"].clone()),
        Some(Value::String("finished".to_string())),
        "tolerating an unknown event must not cost the terminal one",
    );
}
