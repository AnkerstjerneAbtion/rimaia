//! Outcome classification and the `runs` row, against the recorded corpus and a
//! real migrated database (ADR-0011, ADR-0013, task 008).
//!
//! ADR-0011 puts classification "in one module with unit tests over captured CLI
//! output, because it is the piece most likely to break on a CLI update and the
//! one whose breakage is least visible". This is the other half of that
//! sentence. Every class below is decided from a stream a real `claude` process
//! actually produced; nothing here mocks the CLI, and nothing here spawns one.
//!
//! # Two things are deliberately absent
//!
//! **No fabricated `usage_limit` payload.** `spike/FINDINGS.md` §4 records that
//! the `rate_limit_event` when `rate_limit_info.status` is not `"allowed"` was
//! never observed, and there is no fixture for it. So the tests below assert the
//! *predicate* the whole corpus proves — the field names, and "the status is not
//! allowed" — using values written to look invented, because a plausible-looking
//! guess is what would turn into a contract nobody checked.
//!
//! **No fabricated pull-request recording.** No run in the corpus opened one:
//! the spike worked in a throwaway local repository with no remote, and
//! `resume-success.jsonl`'s own summary says "No push or PR was made". The rule
//! is exercised against hand-written probes, built with `parse_line` from the
//! same envelope shapes the recordings use, and every one of them is labelled as
//! a probe rather than left to look like evidence.

use chrono::Duration;
use pretty_assertions::assert_eq;
use rimaia_core::db::{settings, BoardColumn, ExitClass, RunState, RunStatus};
use rimaia_core::runner::events::{parse_line, EventStream, RateLimitEvent, ResultEvent, RunEvent};
use rimaia_core::runner::outcome::{
    classify, finish_run, start_run, NewRun, PullRequestWatch, RunOutcome, SpawnedAs, Termination,
};
use rimaia_core::runner::prompt::compose_prompt;
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::fixtures::{all_fixtures, fixture_lines};
use rimaia_core::testing::TestContext;
use rimaia_core::{AppPaths, ChangeEvent, Clock, ErrorCode};
use sqlx::SqlitePool;

// ---------------------------------------------------------------------------
// The corpus, whole
// ---------------------------------------------------------------------------

#[test]
fn every_recorded_scenario_classifies_into_a_self_consistent_outcome() {
    // The tolerance property, and the one that protects a queue from a CLI
    // update: whatever a recording contains, an outcome comes back and its
    // fields agree with each other. Iterating rather than naming scenarios is
    // task 019's rule — adding a fixture must require no change outside the
    // fixtures directory.
    let names = all_fixtures();
    assert!(!names.is_empty(), "the corpus is the harness");

    for name in &names {
        let replay = Replay::of(name);
        let outcome = replay.outcome();

        assert_eq!(
            outcome.status,
            expected_status(outcome.exit_class),
            "{name}: the status must be the one its class collapses onto"
        );
        assert_eq!(
            outcome.error_message.is_none(),
            outcome.exit_class == ExitClass::Success,
            "{name}: every class but `success` owes a human a sentence"
        );
        assert_eq!(
            outcome.usage_limit_resets_at.is_some(),
            outcome.exit_class == ExitClass::UsageLimit,
            "{name}: a reset time on a run that did not hit a limit reads as one that did"
        );
    }
}

#[test]
fn no_recorded_scenario_reports_a_pull_request_because_none_opened_one() {
    // "Does not invent one when absent", over the whole corpus. The spike ran
    // against a throwaway local repository with no remote, and
    // `resume-success.jsonl`'s summary says so in as many words — a run that
    // talks about pull requests in prose still opened none.
    for name in all_fixtures() {
        assert_eq!(
            Replay::of(&name).outcome().pr_url,
            None,
            "{name} reported a pull request nobody opened"
        );
    }
}

// ---------------------------------------------------------------------------
// The four captured `result` signatures (spike/FINDINGS.md section 5)
// ---------------------------------------------------------------------------

#[test]
fn a_completed_run_is_a_success() {
    assert_eq!(Replay::of("success").class(), ExitClass::Success);
}

#[test]
fn a_resumed_run_that_completed_is_a_success_like_any_other() {
    // `resume-success.jsonl` shares its `session_id` with
    // `interrupted-sigterm.jsonl`: the same session, killed and then finished.
    // Nothing about the resume shows up in the classification, which is the
    // point — ADR-0011 makes every attempt its own row and its own outcome.
    assert_eq!(Replay::of("resume-success").class(), ExitClass::Success);
}

#[test]
fn a_run_killed_mid_stream_is_interrupted_and_still_emitted_a_result() {
    // The correction the spike forced on ADR-0011: the stream does not simply
    // stop when a run is killed. `aborted_streaming` is the CLI telling us it
    // was aborted, which is ADR-0011's "process died" — and the class whose
    // action is "resume once immediately", which the corpus then demonstrates.
    let replay = Replay::of("interrupted-sigterm");

    assert!(
        replay.result.is_some(),
        "a SIGTERM-killed run emits a result before exiting (spike section 5)"
    );
    assert_eq!(replay.class(), ExitClass::Interrupted);
}

#[test]
fn a_turn_limit_is_fatal_rather_than_something_to_retry() {
    // ADR-0011's fatal row names max turns explicitly. Retrying a turn limit
    // spends the same tokens on the same wall.
    assert_eq!(Replay::of("max-turns").class(), ExitClass::Fatal);
}

#[test]
fn the_exit_code_a_killed_run_reports_changes_nothing() {
    // `spike/FINDINGS.md` section 7: a killed process exits 143, not by signal,
    // so `status.code().is_none()` is the wrong check and the code is the wrong
    // discriminator. Every plausible code lands on the same class here.
    let replay = Replay::of("interrupted-sigterm");

    for exit_code in [None, Some(0), Some(1), Some(143)] {
        assert_eq!(
            classify(&replay.termination().exited_with(exit_code)),
            ExitClass::Interrupted,
            "exit code {exit_code:?} must not move the classification"
        );
    }
}

// ---------------------------------------------------------------------------
// Both `run_environment` modes
// ---------------------------------------------------------------------------

#[test]
fn both_environment_fixtures_succeed_despite_wildly_different_init_events() {
    // Task 008's acceptance criterion is about the `init` event; this is the
    // classifier's half of it. The same prompt, inherited and isolated, differs
    // by 229 tools, two MCP servers and 3.6x the cost — and none of that is a
    // classification input. A run that finished is a run that finished.
    let inherited = Replay::of("env-leak-default-settings");
    let isolated = Replay::of("env-leak-isolated-settings");

    assert_eq!(inherited.tools, 255, "the measured inherited tool count");
    assert_eq!(inherited.mcp_servers, 2);
    assert_eq!(isolated.tools, 26, "the measured isolated tool count");
    assert_eq!(isolated.mcp_servers, 0);

    assert_eq!(inherited.class(), ExitClass::Success);
    assert_eq!(isolated.class(), ExitClass::Success);
}

#[test]
fn the_cost_of_inheriting_the_operators_config_is_on_the_row_to_be_seen() {
    // ADR-0004's amendment says the ~3.6x is the thing that must not be hidden,
    // and `result` reports it for free. Both numbers are the spike's measured
    // `total_cost_usd`, byte for byte out of the recordings.
    let inherited = Replay::of("env-leak-default-settings").outcome();
    let isolated = Replay::of("env-leak-isolated-settings").outcome();

    assert_eq!(inherited.cost_usd, Some(0.106_138_200_000_000_02));
    assert_eq!(isolated.cost_usd, Some(0.029_108_1));
}

// ---------------------------------------------------------------------------
// A stream that never reached a `result`
// ---------------------------------------------------------------------------

#[test]
fn a_stream_that_never_reached_a_result_is_transient_and_not_interrupted() {
    // ADR-0011's own signal list gives "empty stream" to `transient`, and its
    // Consequences say unknown terminations default there with limited attempts.
    //
    // Not `interrupted`, deliberately. Seam-contract D9 and ADR-0011's startup
    // reconciliation reserve that word for a run that died *with the app*,
    // discovered by the reconciler from a row left `running`. A classifier that
    // also produced it would make `status = 'interrupted'` stop answering "did
    // Rimaia crash?" — and the two are told apart by evidence, not by guessing:
    // `aborted_streaming` is the CLI saying it was killed, where this is the
    // absence of any terminal event at all.
    let replay = Replay::of("truncated-stream");

    assert!(replay.result.is_none(), "the fixture has no result event");
    assert_eq!(replay.class(), ExitClass::Transient);
}

#[test]
fn a_truncated_stream_says_what_it_could_not_read_and_what_the_process_did() {
    let replay = Replay::of("truncated-stream");

    assert_eq!(
        RunOutcome::of(&replay.termination().exited_with(Some(137)), None).error_message,
        Some(
            "the event stream ended without a result event; the process exited with code 137"
                .to_string()
        )
    );
    assert_eq!(
        replay.outcome().error_message,
        Some("the event stream ended without a result event".to_string())
    );
}

/// A run refused every command it tried ends with no `result` to speak for
/// it, and "the stream ended" is a true sentence that explains nothing — it
/// reads as a CLI fault when the cause was a permission mode that could not
/// approve anything (ADR-0012). The count is the difference between those two
/// readings, so it belongs in the message a reviewer actually sees.
#[test]
fn a_stream_that_ended_after_refusals_names_them_rather_than_only_its_own_silence() {
    let termination = Termination {
        exit_code: Some(0),
        denied_tool_calls: 24,
        ..Termination::default()
    };

    assert_eq!(
        RunOutcome::of(&termination, None).error_message,
        Some(
            "the event stream ended without a result event; the process exited with code 0; \
             24 tool calls were refused for want of approval, so the run could not do anything \
             its permission mode had not already allowed"
                .to_string()
        )
    );
}

#[test]
fn a_single_refusal_is_worded_as_one() {
    let termination = Termination {
        denied_tool_calls: 1,
        ..Termination::default()
    };

    assert_eq!(
        RunOutcome::of(&termination, None).error_message,
        Some(
            "the event stream ended without a result event; 1 tool call was refused for want of \
             approval, so the run could not do anything its permission mode had not already \
             allowed"
                .to_string()
        )
    );
}

/// The refusals are a note on a run that ended unexplained, not a class of
/// their own: ADR-0011's six classes are decided by the terminal vocabulary,
/// and a refused run is still transient — retrying it under a mode that can
/// approve is exactly the right next move.
#[test]
fn refusals_are_reported_but_never_classify_a_run() {
    let refused = Termination {
        denied_tool_calls: 24,
        ..Termination::default()
    };

    assert_eq!(classify(&refused), classify(&Termination::default()));
    assert_eq!(classify(&refused), ExitClass::Transient);
}

#[test]
fn a_line_the_parser_had_to_skip_does_not_change_the_outcome() {
    // `malformed-line.jsonl` is `success.jsonl` with exactly one unreadable line
    // in the middle. Tolerant parsing is worth nothing if the classifier then
    // treats the run differently for it.
    let malformed = Replay::of("malformed-line");
    let clean = Replay::of("success");

    assert_eq!(malformed.class(), ExitClass::Success);
    assert_eq!(malformed.outcome().num_turns, clean.outcome().num_turns);
}

#[test]
fn an_event_type_nobody_models_does_not_change_the_outcome() {
    // `unknown-event-type.jsonl` carries a `telemetry_ping` and a
    // `system/context_compaction`, neither of which any recording contains.
    // ADR-0004's rule, at the classifier: a Claude Code update must not break a
    // queue.
    assert_eq!(Replay::of("unknown-event-type").class(), ExitClass::Success);
}

// ---------------------------------------------------------------------------
// Cancellation
// ---------------------------------------------------------------------------

#[test]
fn a_cancelled_run_is_cancelled_however_the_cli_described_its_ending() {
    // A user cancellation and an outside kill produce the identical
    // `aborted_streaming` result — this fixture *is* a SIGTERM. Only the caller
    // knows which one it ordered, which is why the flag exists rather than being
    // inferred.
    let replay = Replay::of("interrupted-sigterm");

    assert_eq!(replay.class(), ExitClass::Interrupted);
    assert_eq!(
        classify(&replay.termination().cancelled()),
        ExitClass::Cancelled
    );
}

#[test]
fn cancelling_a_run_that_had_already_succeeded_is_still_a_cancellation() {
    // The order in `classify` made visible. Rimaia asked for this ending, so
    // whatever arrived on the way out describes a killing we ordered — and the
    // task must not be moved to `in_review` on the strength of it.
    assert_eq!(
        classify(&Replay::of("success").termination().cancelled()),
        ExitClass::Cancelled
    );
}

// ---------------------------------------------------------------------------
// Usage limits — the predicate, never a payload
// ---------------------------------------------------------------------------

#[test]
fn the_rate_limit_event_every_run_emits_is_not_a_usage_limit() {
    // Every recording carries `"status":"allowed"` early and unprompted. Reading
    // that as a limit would fail every run in the corpus, which is the mistake
    // this test exists to keep failing loudly.
    for name in all_fixtures() {
        let replay = Replay::of(&name);

        assert_eq!(
            replay
                .rate_limit
                .as_ref()
                .and_then(|limit| limit.status.clone()),
            Some("allowed".to_string()),
            "{name}: the corpus proves exactly one status value"
        );
        assert_ne!(replay.class(), ExitClass::UsageLimit, "{name}");
    }
}

#[test]
fn a_rate_limit_status_that_is_not_allowed_is_a_usage_limit_whatever_the_word_turns_out_to_be() {
    // `spike/FINDINGS.md` section 4: the non-`allowed` payload was never
    // observed. So the rule under test is "not allowed", and the values below
    // are chosen to be obviously invented — if one of them ever looked like a
    // real vocabulary, this test would have quietly become a contract.
    for status in ["limited", "blocked", "a status nobody has observed yet"] {
        let rate_limit = RateLimitEvent {
            status: Some(status.to_string()),
            resets_at: Some(1_787_224_800),
            rate_limit_type: Some("five_hour".to_string()),
        };
        let termination = Termination {
            rate_limit: Some(&rate_limit),
            ..Termination::default()
        };

        assert_eq!(classify(&termination), ExitClass::UsageLimit, "{status:?}");
    }
}

#[test]
fn a_usage_limit_carries_the_reset_instant_the_scheduler_will_need() {
    // The epoch `resetsAt` is the field name the whole corpus proves, and the
    // reason ADR-0011's amendment prefers the typed event over grepping a
    // message. Task 014 turns this into `runs.resume_after` — with the jitter
    // ADR-0011 asks for, which is why this module leaves that column NULL.
    let rate_limit = RateLimitEvent {
        status: Some("limited".to_string()),
        resets_at: Some(1_787_224_800),
        rate_limit_type: Some("five_hour".to_string()),
    };
    let outcome = RunOutcome::of(
        &Termination {
            rate_limit: Some(&rate_limit),
            ..Termination::default()
        },
        None,
    );

    assert_eq!(outcome.status, RunStatus::Failed);
    // The epoch is the one every recording carries, spelled as the instant it
    // means so a reader can see the two agree.
    assert_eq!(
        outcome.usage_limit_resets_at,
        Some("2026-08-20T11:20:00Z".parse().expect("a literal timestamp"))
    );
    assert_eq!(
        outcome.error_message,
        Some(
            "the run stopped at a usage limit (five_hour); it resets at 2026-08-20T11:20:00+00:00"
                .to_string()
        )
    );
}

#[test]
fn a_run_that_finished_the_work_is_a_success_even_under_a_usage_limit() {
    // The window rolling over mid-run is not a reason to disbelieve a `result`
    // that says the work is done. Retrying it would re-spend tokens on a task
    // that is already in `in_review`.
    let replay = Replay::of("success");
    let rate_limit = RateLimitEvent {
        status: Some("limited".to_string()),
        ..RateLimitEvent::default()
    };
    let termination = Termination {
        rate_limit: Some(&rate_limit),
        ..replay.termination()
    };

    assert_eq!(classify(&termination), ExitClass::Success);
}

#[test]
fn a_usage_limit_outranks_aborted_streaming_but_not_max_turns() {
    // Misreading a limit as a hard failure is the 2am mistake ADR-0011 names,
    // so the limit wins over `aborted_streaming` — a real rate limit plausibly
    // produces exactly that terminal reason. It does **not** win over
    // `max_turns`: that is the one terminal reason a rate limit cannot itself
    // produce, and the one ADR-0011 names fatal by hand, so an unrecognised or
    // stray `rate_limit_info.status` must not be allowed to turn it into an
    // unbounded retry against a turn limit that will just re-hit immediately.
    let rate_limit = RateLimitEvent {
        status: Some("limited".to_string()),
        ..RateLimitEvent::default()
    };

    let aborted = Replay::of("interrupted-sigterm");
    assert_eq!(aborted.class(), ExitClass::Interrupted);
    assert_eq!(
        classify(&Termination {
            rate_limit: Some(&rate_limit),
            ..aborted.termination()
        }),
        ExitClass::UsageLimit
    );

    let max_turns = Replay::of("max-turns");
    assert_eq!(max_turns.class(), ExitClass::Fatal);
    assert_eq!(
        classify(&Termination {
            rate_limit: Some(&rate_limit),
            ..max_turns.termination()
        }),
        ExitClass::Fatal,
        "max_turns is the one terminal reason a usage limit must not outrank"
    );
}

// ---------------------------------------------------------------------------
// The metrics, extracted rather than derived
// ---------------------------------------------------------------------------

#[test]
fn the_metrics_are_the_numbers_the_result_event_already_carried() {
    // `spike/FINDINGS.md` section 6: `num_turns`, `total_cost_usd` and
    // `duration_ms` all arrive on the terminal event, so there is nothing to
    // derive. Each literal below is read out of `success.jsonl` itself, and the
    // second half of the test proves the point rather than restating it — the
    // turn count is *not* the number of assistant events, so a derivation would
    // land somewhere else.
    let replay = Replay::of("success");
    let outcome = replay.outcome();

    assert_eq!(outcome.num_turns, Some(5));
    assert_eq!(outcome.cost_usd, Some(0.150_292_5));
    assert_eq!(outcome.duration_ms, Some(21_427));
    assert_eq!(outcome.error_message, None, "a success owes no explanation");

    assert_ne!(
        replay.assistant_events, 5,
        "if these ever coincide the test below stops proving anything"
    );
}

#[test]
fn a_failed_runs_error_text_is_the_clis_own_words() {
    assert_eq!(
        Replay::of("max-turns").outcome().error_message,
        Some("Reached maximum number of turns (2)".to_string())
    );
    assert_eq!(
        Replay::of("interrupted-sigterm").outcome().error_message,
        Some(
            "[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"
                .to_string()
        )
    );
}

#[test]
fn a_cancelled_run_says_it_was_cancelled_rather_than_repeating_the_kill_diagnostic() {
    // The card reads this sentence. "[ede_diagnostic] result_type=user ..." is
    // true and useless when the user is the one who pressed Cancel.
    let replay = Replay::of("interrupted-sigterm");
    let outcome = RunOutcome::of(&replay.termination().cancelled(), None);

    assert_eq!(outcome.status, RunStatus::Cancelled);
    assert_eq!(
        outcome.error_message,
        Some("the run was cancelled".to_string())
    );
}

// ---------------------------------------------------------------------------
// The pull-request rule
// ---------------------------------------------------------------------------

#[test]
fn a_pull_request_the_agent_reports_opening_is_read_off_its_closing_summary() {
    // A probe, not a recording: the envelope is copied from the corpus, the
    // narration is invented, and no fixture claims otherwise. The seeded base
    // instructions ask the agent to "push the branch and open a pull request
    // describing what changed and why", so the summary is the designed channel.
    let watch = watching([
        assistant_text("Running the tests before I push."),
        result_summary("Opened https://github.com/abtion/rimaia/pull/42 with three commits."),
    ]);

    assert_eq!(
        watch.url(),
        Some("https://github.com/abtion/rimaia/pull/42")
    );
}

#[test]
fn a_pull_request_opened_before_the_run_was_killed_survives_in_the_narration() {
    // `interrupted-sigterm.jsonl` is exactly this shape: a `result` with no
    // summary at all. Without the fallback, the first attempt's row would show
    // no pull request for one it actually opened.
    let watch = watching([
        assistant_text("Pushed the branch and opened https://github.com/abtion/rimaia/pull/42."),
        result_without_summary(),
    ]);

    assert_eq!(
        watch.url(),
        Some("https://github.com/abtion/rimaia/pull/42")
    );
}

#[test]
fn a_pull_request_url_the_agent_only_read_is_not_one_the_run_opened() {
    // The question is not "does a pull-request URL appear in this run" but "did
    // this run open one". A tool input is a URL the agent is consuming — a
    // `WebFetch` of a pull request, a `gh pr view`.
    let watch = watching([
        tool_call(
            "WebFetch",
            serde_json::json!({ "url": "https://github.com/abtion/rimaia/pull/9" }),
        ),
        tool_call(
            "Bash",
            serde_json::json!({ "command": "gh pr view https://github.com/abtion/rimaia/pull/9" }),
        ),
        result_summary("Followed the pattern from the linked pull request. No PR opened."),
    ]);

    assert_eq!(watch.url(), None);
}

#[test]
fn a_pull_request_url_in_a_tool_result_is_not_one_the_run_opened() {
    // A `gh pr list` result is a list of pull requests this run did not open,
    // and `runner::events` does not model tool-result content in the first
    // place. Both reasons point the same way.
    let watch = watching([
        tool_result("https://github.com/abtion/rimaia/pull/9\thandle slugs\tOPEN"),
        result_without_summary(),
    ]);

    assert_eq!(watch.url(), None);
}

#[test]
fn the_pull_request_url_reaches_the_outcome_that_will_be_stored() {
    let replay = Replay::of("success");
    let outcome = RunOutcome::of(
        &replay.termination(),
        Some("https://github.com/abtion/rimaia/pull/42".to_string()),
    );

    assert_eq!(
        outcome.pr_url.as_deref(),
        Some("https://github.com/abtion/rimaia/pull/42")
    );
}

// ---------------------------------------------------------------------------
// The `runs` row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_started_run_records_its_prompt_verbatim_beside_its_transcript_path() {
    let mut fixture = RunFixture::new().await;
    let prompt = fixture.composed_prompt().await;

    let run = fixture.start(&prompt).await;

    assert_eq!(run.task_id, fixture.task_id);
    assert_eq!(run.attempt, 1);
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.session_id, SESSION_ID);
    assert_eq!(run.prompt, prompt, "ADR-0009 stores the exact string sent");
    assert_eq!(run.started_at, fixture.harness.clock.now());
    assert_eq!(run.ended_at, None);
    assert_eq!(run.exit_class, None);
    assert_eq!(run.pr_url, None);
    assert_eq!(
        run.log_path,
        fixture
            .paths
            .runs_dir()
            .join(&fixture.task_id)
            .join(format!("{}.jsonl", run.id))
            .to_string_lossy(),
        "ADR-0013's path is a pure function of the task and run ids"
    );

    assert_eq!(
        fixture.harness.changes.try_recv().expect("a publication"),
        ChangeEvent::runs([run.id])
    );
    assert_eq!(
        fixture.harness.changes.try_recv().expect("a publication"),
        ChangeEvent::tasks([fixture.task_id.clone()]),
        "a card renders its last run (seam-contract D12), so the task changed too"
    );
}

#[tokio::test]
async fn a_stored_prompt_is_a_copy_and_a_later_edit_of_the_base_instructions_leaves_it_alone() {
    // Task 006's acceptance criterion, satisfied here because this is where the
    // copy is made. ADR-0009's reason: when a run goes wrong the first question
    // is always "what did it actually get", and a composition redone against
    // today's settings does not answer it.
    let mut fixture = RunFixture::new().await;
    let prompt = fixture.composed_prompt().await;
    let run = fixture.start(&prompt).await;

    settings::set_base_instructions(&fixture.harness.context, "Do something else entirely.")
        .await
        .expect("edit the base instructions");

    assert_eq!(fixture.reread(&run.id).await.prompt, prompt);
    assert_ne!(fixture.composed_prompt().await, prompt, "the edit did land");
}

#[tokio::test]
async fn a_second_attempt_takes_the_next_attempt_number_on_the_same_session() {
    // ADR-0011: "each attempt is a row in `runs`, sharing the task's session id,
    // so the history of an overnight task reads as the sequence of walls it hit".
    let mut fixture = RunFixture::new().await;

    let first = fixture.start("first").await;
    fixture
        .finish(&first.id, &Replay::of("interrupted-sigterm").outcome())
        .await;
    let second = fixture.start("second").await;

    assert_eq!(first.attempt, 1);
    assert_eq!(second.attempt, 2);
    assert_eq!(first.session_id, second.session_id);
}

#[tokio::test]
async fn a_run_against_a_task_that_does_not_exist_is_refused_by_name() {
    let fixture = RunFixture::new().await;

    let error = start_run(
        &fixture.harness.context,
        &fixture.paths,
        NewRun {
            task_id: "3f2b1c00-0000-4000-8000-00000000dead".to_string(),
            session_id: SESSION_ID.to_string(),
            prompt: "do the thing".to_string(),
        },
    )
    .await
    .expect_err("a run needs a task");

    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(
        error.to_string(),
        "no task with id 3f2b1c00-0000-4000-8000-00000000dead"
    );
}

#[tokio::test]
async fn a_successful_run_moves_its_task_to_in_review_and_back_to_idle() {
    // Task 008's scope, in one sentence, and the thing that makes the morning
    // review possible. Both writes go through task 004's services — a second
    // writer of `board_column` or `run_state` is the ADR-0006 bug.
    let mut fixture = RunFixture::new().await;
    let run = fixture.start("do the thing").await;
    fixture.harness.clock.advance(Duration::seconds(21));

    let finished = fixture
        .finish(&run.id, &Replay::of("success").outcome())
        .await;

    assert_eq!(finished.status, RunStatus::Succeeded);
    assert_eq!(finished.exit_class, Some(ExitClass::Success));
    assert_eq!(finished.ended_at, Some(fixture.harness.clock.now()));
    assert_eq!(finished.num_turns, Some(5));
    assert_eq!(finished.cost_usd, Some(0.150_292_5));
    assert_eq!(finished.error_message, None);
    assert_eq!(
        finished.resume_after, None,
        "the wait plus jitter is ADR-0011's, and task 014's"
    );

    let task = fixture.task().await;
    assert_eq!(task.column, BoardColumn::InReview);
    assert_eq!(task.run_state, RunState::Idle);
}

#[tokio::test]
async fn a_second_successful_task_lands_below_the_first_in_in_review() {
    // Through `move_task` with a named neighbour, which is seam-contract D1's
    // rule for every caller: send `before_id` and `after_id`, never a position.
    // Naming neither is only legal into an empty column, so a runner that always
    // passed `None` would fail on the second task of the night.
    let mut first = RunFixture::new().await;
    let second = first.sibling_task("the second task").await;

    let first_run = first.start("do the first thing").await;
    first
        .finish(&first_run.id, &Replay::of("success").outcome())
        .await;

    let second_run = first.start_for(&second, "do the second thing").await;
    first
        .finish(&second_run.id, &Replay::of("success").outcome())
        .await;

    assert_eq!(
        first.in_review_titles().await,
        vec![TASK_TITLE.to_string(), "the second task".to_string()]
    );
}

#[tokio::test]
async fn a_fatal_run_fails_its_task_and_leaves_the_card_where_it_was() {
    // ADR-0011: "no retry. Task -> `run_state = failed`, error surfaced on the
    // card." ADR-0007's failure rule keeps the card in `ready`, because a task
    // that failed is still a task that is ready to be implemented.
    let mut fixture = RunFixture::new().await;
    let run = fixture.start("do the thing").await;

    let finished = fixture
        .finish(&run.id, &Replay::of("max-turns").outcome())
        .await;

    assert_eq!(finished.status, RunStatus::Failed);
    assert_eq!(finished.exit_class, Some(ExitClass::Fatal));
    assert_eq!(
        finished.error_message,
        Some("Reached maximum number of turns (2)".to_string())
    );

    let task = fixture.task().await;
    assert_eq!(task.column, BoardColumn::Ready);
    assert_eq!(task.run_state, RunState::Failed);
}

#[tokio::test]
async fn a_cancelled_run_fails_its_task_because_run_state_has_no_cancelled_for_it() {
    // ADR-0010's literal words: cancel-one on a *running* task "goes to `failed`
    // with `cancelled` reason". `Running -> Cancelled` is illegal by design and
    // the reason lives on the run's `exit_class`, which is where the card reads
    // the word from (seam-contract D9).
    let mut fixture = RunFixture::new().await;
    let run = fixture.start("do the thing").await;
    let outcome = RunOutcome::of(
        &Replay::of("interrupted-sigterm").termination().cancelled(),
        None,
    );

    let finished = fixture.finish(&run.id, &outcome).await;

    assert_eq!(finished.status, RunStatus::Cancelled);
    assert_eq!(finished.exit_class, Some(ExitClass::Cancelled));
    assert_eq!(fixture.task().await.run_state, RunState::Failed);
}

#[tokio::test]
async fn a_run_that_is_going_to_be_resumed_leaves_its_task_waiting_rather_than_running() {
    // The three classes the MVP does not act on still have to leave the machine
    // somewhere true. A task left `running` with no process is a badge that
    // lies, and `waiting_retry` is where ADR-0011's own table puts a run that is
    // going to be resumed — task 014 schedules the wait, it does not name it.
    for scenario in ["interrupted-sigterm", "truncated-stream"] {
        let mut fixture = RunFixture::new().await;
        let run = fixture.start("do the thing").await;

        let finished = fixture
            .finish(&run.id, &Replay::of(scenario).outcome())
            .await;

        assert_eq!(
            fixture.task().await.run_state,
            RunState::WaitingRetry,
            "{scenario}"
        );
        assert_eq!(finished.ended_at, Some(fixture.harness.clock.now()));
    }
}

#[tokio::test]
async fn an_interrupted_run_keeps_the_word_seam_contract_d9_puts_on_it() {
    // The one place "interrupted" is ever supposed to appear is the run row, and
    // the card reads it from there — never from `run_state`, which has no such
    // value.
    let mut fixture = RunFixture::new().await;
    let run = fixture.start("do the thing").await;

    let finished = fixture
        .finish(&run.id, &Replay::of("interrupted-sigterm").outcome())
        .await;

    assert_eq!(finished.status, RunStatus::Interrupted);
    assert_eq!(finished.exit_class, Some(ExitClass::Interrupted));
}

#[tokio::test]
async fn finishing_a_run_twice_is_refused_rather_than_replaying_the_task_transitions() {
    let mut fixture = RunFixture::new().await;
    let run = fixture.start("do the thing").await;
    let outcome = Replay::of("success").outcome();
    fixture.finish(&run.id, &outcome).await;

    let error = finish_run(&fixture.harness.context, &run.id, &outcome)
        .await
        .expect_err("a run ends once");

    assert_eq!(error.code(), ErrorCode::Invalid);
    assert_eq!(
        error.to_string(),
        format!("run {} has already been finalized", run.id)
    );
}

#[tokio::test]
async fn finishing_a_run_publishes_the_run_and_the_task_it_belongs_to() {
    let mut fixture = RunFixture::new().await;
    let run = fixture.start("do the thing").await;
    while fixture.harness.changes.try_recv().is_ok() {}

    fixture
        .finish(&run.id, &Replay::of("max-turns").outcome())
        .await;

    assert_eq!(
        fixture.harness.changes.try_recv().expect("the run"),
        ChangeEvent::runs([run.id])
    );
    // Twice for the task: once for the row this run changed, once from
    // `set_run_state`'s own publication. Both are ids, so a subscriber re-reads
    // and neither is wrong (ADR-0018).
    assert_eq!(
        fixture.harness.changes.try_recv().expect("the task"),
        ChangeEvent::tasks([fixture.task_id.clone()])
    );
    assert_eq!(
        fixture.harness.changes.try_recv().expect("the run state"),
        ChangeEvent::tasks([fixture.task_id.clone()])
    );
}

#[tokio::test]
async fn a_finished_run_is_the_one_the_board_reads_off_the_task() {
    // The round trip that matters to the morning review: what the classifier
    // decided is what the card shows, through task 004's own read.
    let mut fixture = RunFixture::new().await;
    let run = fixture.start("do the thing").await;
    fixture
        .finish(&run.id, &Replay::of("interrupted-sigterm").outcome())
        .await;

    let detail = tasks::get_task(&fixture.harness.context, &fixture.task_id)
        .await
        .expect("read the task back");
    let last_run = detail.last_run.expect("the attempt just recorded");

    assert_eq!(last_run.id, run.id);
    assert_eq!(last_run.status, RunStatus::Interrupted);
    assert_eq!(last_run.exit_class, Some(ExitClass::Interrupted));
    assert_eq!(last_run.prompt, "do the thing");
}

#[tokio::test]
async fn a_stream_classifies_the_same_way_whether_it_is_replayed_or_read_off_the_event_stream() {
    // `Termination::from_stream` is what the runner will actually call. This is
    // the same fixture through the real `EventStream` — transcript on disk,
    // tolerant parsing, the lot — landing on the same class.
    let harness = TestContext::new().await;
    let root = tempfile::Builder::new()
        .prefix("rimaia-runs-")
        .tempdir()
        .expect("temp dir for the run directory");
    let paths = AppPaths::new(root.path());
    let mut stream = EventStream::create(&harness.context, &paths, "task", "run")
        .expect("the run directory is creatable");

    for line in fixture_lines("max-turns") {
        stream
            .observe(&line)
            .expect("the transcript stays writable");
    }

    let outcome = RunOutcome::of(&Termination::from_stream(&stream), None);
    assert_eq!(outcome.exit_class, ExitClass::Fatal);
    assert_eq!(outcome, Replay::of("max-turns").outcome());
}

// ---------------------------------------------------------------------------
// ADR-0022's capture, and seam-contract D18's NULL rule
// ---------------------------------------------------------------------------

#[test]
fn a_recorded_run_carries_the_four_token_counts_off_its_result_event() {
    // The exact numbers in `success.jsonl`. Asserted as literals rather than
    // recomputed from the file, because the point of the test is that the four
    // field names ADR-0022 reads — `input_tokens`, `output_tokens`,
    // `cache_read_input_tokens`, `cache_creation_input_tokens` — are the ones
    // the corpus actually spells. A test that re-derived them from the same
    // keys it is checking would pass on a rename.
    let usage = Replay::of("success").outcome().usage;

    assert_eq!(usage.input_tokens, Some(10));
    assert_eq!(usage.output_tokens, Some(1949));
    assert_eq!(usage.cache_read_tokens, Some(163_145));
    assert_eq!(usage.cache_creation_tokens, Some(11_819));
}

#[test]
fn a_failed_run_still_records_what_it_spent_getting_there() {
    // The cost of a wasted night is the thing ADR-0022's failure-rate and
    // cost-per-completed-task numbers are made of, so a run that ended badly
    // must still carry its tokens. `max-turns.jsonl` is `fatal`; it spent real
    // money before hitting the wall.
    let outcome = Replay::of("max-turns").outcome();

    assert_eq!(outcome.exit_class, ExitClass::Fatal);
    assert_eq!(outcome.usage.input_tokens, Some(4));
    assert_eq!(outcome.usage.output_tokens, Some(1016));
    assert_eq!(outcome.usage.cache_read_tokens, Some(57_999));
    assert_eq!(outcome.usage.cache_creation_tokens, Some(9_557));
}

#[test]
fn a_stream_that_never_reached_a_result_records_no_tokens_rather_than_zero() {
    // Seam-contract D18, stated as an assertion: NULL means *not recorded*.
    // `truncated-stream.jsonl` is a run whose stream stopped mid-flight, so it
    // honestly never learned what it spent. Four zeroes here would be a claim
    // that it spent nothing, which a later average would repeat as a fact.
    let outcome = Replay::of("truncated-stream").outcome();

    assert_eq!(outcome.usage.input_tokens, None);
    assert_eq!(outcome.usage.output_tokens, None);
    assert_eq!(outcome.usage.cache_read_tokens, None);
    assert_eq!(outcome.usage.cache_creation_tokens, None);
}

#[test]
fn classification_never_reads_the_usage_numbers() {
    // The two are independent by design: ADR-0011 classifies on
    // `terminal_reason` and `subtype`, and ADR-0022's numbers are a record, not
    // a signal. Proven by walking the corpus — every fixture keeps its class
    // whether or not it carried a `usage` object.
    for name in all_fixtures() {
        let replay = Replay::of(&name);
        let outcome = replay.outcome();
        assert_eq!(
            outcome.exit_class,
            replay.class(),
            "{name} classified differently once its usage was read",
        );
    }
}

#[tokio::test]
async fn finishing_a_run_records_what_it_was_spawned_as_and_what_it_spent() {
    let mut fixture = RunFixture::new().await;
    let prompt = fixture.composed_prompt().await;
    let run = fixture.start(&prompt).await;

    let mut outcome = Replay::of("success").outcome();
    outcome.spawned_as = SpawnedAs {
        model: Some("claude-sonnet-5".to_string()),
        effort: Some("high".to_string()),
        run_environment: Some("inherit".to_string()),
    };

    let stored = fixture.finish(&run.id, &outcome).await;

    assert_eq!(stored.model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(stored.effort.as_deref(), Some("high"));
    assert_eq!(stored.run_environment.as_deref(), Some("inherit"));
    assert_eq!(stored.input_tokens, Some(10));
    assert_eq!(stored.output_tokens, Some(1949));
    assert_eq!(stored.cache_read_tokens, Some(163_145));
    assert_eq!(stored.cache_creation_tokens, Some(11_819));
}

#[tokio::test]
async fn a_run_that_learned_nothing_leaves_every_capture_column_null() {
    // The other half of D18, through the database rather than in memory: an
    // outcome with nothing recorded must reach the row as seven NULLs. This is
    // the shape `scheduler::reconcile` writes for a run that died with the app,
    // and the one a later analytics view has to be able to tell apart from a
    // run that genuinely cost nothing.
    let mut fixture = RunFixture::new().await;
    let prompt = fixture.composed_prompt().await;
    let run = fixture.start(&prompt).await;

    let outcome = Replay::of("truncated-stream").outcome();
    let stored = fixture.finish(&run.id, &outcome).await;

    assert_eq!(stored.model, None);
    assert_eq!(stored.effort, None);
    assert_eq!(stored.run_environment, None);
    assert_eq!(stored.input_tokens, None);
    assert_eq!(stored.output_tokens, None);
    assert_eq!(stored.cache_read_tokens, None);
    assert_eq!(stored.cache_creation_tokens, None);
}

// ---------------------------------------------------------------------------
// Replaying a recorded scenario
// ---------------------------------------------------------------------------

/// One recorded stream, folded down to the facts an outcome is made of.
///
/// Deliberately not an [`EventStream`]: this is the pure path, so a
/// classification test needs no database, no temporary directory and no clock.
/// One test above drives the real stream and asserts the two agree.
struct Replay {
    result: Option<ResultEvent>,
    rate_limit: Option<RateLimitEvent>,
    pull_request: PullRequestWatch,
    assistant_events: i64,
    /// The two `init` numbers the `env-leak-*` pair exists to measure.
    tools: usize,
    mcp_servers: usize,
}

impl Replay {
    fn of(fixture: &str) -> Self {
        let mut replay = Self {
            result: None,
            rate_limit: None,
            pull_request: PullRequestWatch::default(),
            assistant_events: 0,
            tools: 0,
            mcp_servers: 0,
        };

        for line in fixture_lines(fixture) {
            // A line the parser cannot read is skipped and never fatal — the
            // condition `malformed-line.jsonl` and the tail of
            // `truncated-stream.jsonl` record.
            let Ok(event) = parse_line(&line) else {
                continue;
            };
            replay.pull_request.observe(&event);
            match &event {
                RunEvent::Init(init) => {
                    replay.tools = init.tools.len();
                    replay.mcp_servers = init.mcp_servers.len();
                }
                RunEvent::Assistant(_) => replay.assistant_events += 1,
                RunEvent::RateLimit(rate_limit) => replay.rate_limit = Some(rate_limit.clone()),
                RunEvent::Result(result) => replay.result = Some(result.clone()),
                _ => {}
            }
        }

        replay
    }

    fn termination(&self) -> Termination<'_> {
        Termination {
            result: self.result.as_ref(),
            rate_limit: self.rate_limit.as_ref(),
            ..Termination::default()
        }
    }

    fn class(&self) -> ExitClass {
        classify(&self.termination())
    }

    fn outcome(&self) -> RunOutcome {
        RunOutcome::of(
            &self.termination(),
            self.pull_request.url().map(ToOwned::to_owned),
        )
    }
}

// ---------------------------------------------------------------------------
// Hand-written probes for the pull-request rule
// ---------------------------------------------------------------------------
//
// Every one of these is built with `parse_line` from the envelope shapes the
// recordings use, and every one is a *probe* rather than evidence: no run in the
// corpus opened a pull request, and inventing a recording that had would be a
// fixture nobody captured.

fn watching<const N: usize>(lines: [String; N]) -> PullRequestWatch {
    let mut watch = PullRequestWatch::default();
    for line in lines {
        watch.observe(&parse_line(&line).expect("a probe must be valid JSON"));
    }
    watch
}

fn assistant_text(text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": { "id": "msg_probe", "content": [{ "type": "text", "text": text }] },
    })
    .to_string()
}

fn tool_call(name: &str, input: serde_json::Value) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg_probe",
            "content": [{ "type": "tool_use", "id": "toolu_probe", "name": name, "input": input }],
        },
    })
    .to_string()
}

fn tool_result(content: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_probe",
                "content": content,
                "is_error": false,
            }],
        },
    })
    .to_string()
}

fn result_summary(summary: &str) -> String {
    serde_json::json!({
        "type": "result",
        "subtype": "success",
        "terminal_reason": "completed",
        "is_error": false,
        "result": summary,
    })
    .to_string()
}

/// A `result` with no summary at all — which is what every error subtype in the
/// corpus looks like.
fn result_without_summary() -> String {
    serde_json::json!({
        "type": "result",
        "subtype": "error_during_execution",
        "terminal_reason": "aborted_streaming",
        "is_error": true,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// A task with a run, against a real migrated database
// ---------------------------------------------------------------------------

const TASK_TITLE: &str = "Add truncate_slug";
const SESSION_ID: &str = "c6529619-d49b-479b-b4ce-a97cad085fda";
const NOW: &str = "2026-08-20T00:00:00+00:00";

/// A repository, a task in `ready` with a plan, and the task already moved into
/// `run_state = running` the way the scheduler would leave it.
struct RunFixture {
    harness: TestContext,
    /// Held only for its `Drop`; [`paths`](Self::paths) points inside it.
    _root: tempfile::TempDir,
    paths: AppPaths,
    repository_id: String,
    task_id: String,
}

impl RunFixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let root = tempfile::Builder::new()
            .prefix("rimaia-runs-")
            .tempdir()
            .expect("temp dir for the run directory");
        let paths = AppPaths::new(root.path());
        let repository_id = seed_repository(&harness.context.pool).await;

        let mut fixture = Self {
            harness,
            _root: root,
            paths,
            repository_id,
            task_id: String::new(),
        };
        fixture.task_id = fixture.ready_task(TASK_TITLE).await;
        fixture
    }

    /// A second task in the same repository, so a test can put two cards in
    /// `in_review`.
    async fn sibling_task(&mut self, title: &str) -> String {
        self.ready_task(title).await
    }

    async fn ready_task(&mut self, title: &str) -> String {
        let task = tasks::create_task(
            &self.harness.context,
            NewTask {
                repository_id: self.repository_id.clone(),
                title: title.to_string(),
                plan: Some("1. Add the function\n2. Test it".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task");

        // Idle -> Queued -> Running is the only legal way into `running`
        // (ADR-0010's selection transaction), and the runner inherits a task
        // that is already there.
        for state in [RunState::Queued, RunState::Running] {
            tasks::set_run_state(&self.harness.context, &task.id, state)
                .await
                .expect("the scheduler's own transitions");
        }
        while self.harness.changes.try_recv().is_ok() {}
        task.id
    }

    /// What task 006 composes for this task, so the prompt under test is the one
    /// the runner would actually send.
    async fn composed_prompt(&self) -> String {
        let detail = tasks::get_task(&self.harness.context, &self.task_id)
            .await
            .expect("read the task");
        let repository = rimaia_core::repo::get(&self.harness.context, &self.repository_id)
            .await
            .expect("read the repository");
        let base = settings::base_instructions(&self.harness.context.pool)
            .await
            .expect("read the base instructions");

        compose_prompt(&base, &detail, &repository, None)
    }

    async fn start(&mut self, prompt: &str) -> rimaia_core::db::Run {
        let task_id = self.task_id.clone();
        self.start_for(&task_id, prompt).await
    }

    async fn start_for(&mut self, task_id: &str, prompt: &str) -> rimaia_core::db::Run {
        start_run(
            &self.harness.context,
            &self.paths,
            NewRun {
                task_id: task_id.to_string(),
                session_id: SESSION_ID.to_string(),
                prompt: prompt.to_string(),
            },
        )
        .await
        .expect("open a run row")
    }

    async fn finish(&mut self, run_id: &str, outcome: &RunOutcome) -> rimaia_core::db::Run {
        finish_run(&self.harness.context, run_id, outcome)
            .await
            .expect("close a run row")
    }

    async fn reread(&self, run_id: &str) -> rimaia_core::db::Run {
        let detail = tasks::get_task(&self.harness.context, &self.task_id)
            .await
            .expect("read the task");
        let run = detail.last_run.expect("a recorded attempt");
        assert_eq!(run.id, run_id, "the last attempt is the one under test");
        run
    }

    async fn task(&self) -> rimaia_core::db::Task {
        tasks::get_task(&self.harness.context, &self.task_id)
            .await
            .expect("read the task")
            .task
    }

    async fn in_review_titles(&self) -> Vec<String> {
        tasks::list_tasks(
            &self.harness.context,
            tasks::TaskFilter {
                repository_id: Some(self.repository_id.clone()),
                column: Some(BoardColumn::InReview),
                run_state: None,
            },
        )
        .await
        .expect("list in_review")
        .into_iter()
        .map(|summary| summary.task.title)
        .collect()
    }
}

async fn seed_repository(pool: &SqlitePool) -> String {
    let id = rimaia_core::db::new_id();
    sqlx::query!(
        r#"INSERT INTO repositories (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
           VALUES (?1, 'rimaia', '/tmp/rimaia', 'main', '/tmp/rimaia-worktrees', 1, ?2)"#,
        id,
        NOW,
    )
    .execute(pool)
    .await
    .expect("seed a repository");
    id
}

/// The collapse `db::models` documents, restated where the corpus test can use
/// it — if the two ever disagree, one of them is wrong about ADR-0013's row.
fn expected_status(exit_class: ExitClass) -> RunStatus {
    match exit_class {
        ExitClass::Success => RunStatus::Succeeded,
        ExitClass::Cancelled => RunStatus::Cancelled,
        ExitClass::Interrupted => RunStatus::Interrupted,
        ExitClass::UsageLimit | ExitClass::Transient | ExitClass::Fatal => RunStatus::Failed,
    }
}
