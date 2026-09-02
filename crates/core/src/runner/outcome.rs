//! Why a run stopped, and the `runs` row that records it (ADR-0011, ADR-0013).
//!
//! ADR-0011 puts classification "in one module with unit tests over captured CLI
//! output, because it is the piece most likely to break on a CLI update and the
//! one whose breakage is least visible — a misclassified `usage_limit` looks
//! like a hard failure at 2am". This is that module, and
//! `tests/runner_outcome.rs` is that test: every checked-in scenario is replayed
//! through [`classify`] and asserted, and one test walks the whole corpus to
//! prove that *something* comes back for every recording, which is what protects
//! a queue from a CLI update.
//!
//! # Classify on `terminal_reason` and `subtype`, never on the exit code
//!
//! `spike/FINDINGS.md` §5 measured three signatures against Claude Code 2.1.234:
//!
//! | Scenario | exit | `subtype` | `terminal_reason` |
//! | --- | --- | --- | --- |
//! | Success | 0 | `success` | `completed` |
//! | Killed (SIGTERM) | 143 | `error_during_execution` | `aborted_streaming` |
//! | Turn limit | 1 | `error_max_turns` | `max_turns` |
//!
//! Two things follow that the ADR originally had wrong. **A killed run still
//! emits a `result` before exiting** — the stream does not simply stop. And it
//! exits **143**, not by signal, so `status.code().is_none()` is the wrong "was
//! it signalled" check. [`Termination::exit_code`] is carried for the record and
//! for an error message a human reads; [`classify`] never branches on it.
//!
//! # The `usage_limit` gap, on purpose
//!
//! `spike/FINDINGS.md` §4: the `rate_limit_event` payload when
//! `rate_limit_info.status` is something other than `"allowed"` was **never
//! observed**, and there is no `usage_limit` fixture. So this module classifies
//! on the predicate the whole corpus proves — the field *names* `status`,
//! `resetsAt` and `rateLimitType`, and "the status is not `allowed`" — and
//! invents no vocabulary for the payload nobody has seen. Capturing the real one
//! the first time a queue hits the wall is a human's job.

use chrono::{DateTime, Utc};

use crate::context::ServiceContext;
use crate::db::{new_id, BoardColumn, ExitClass, Run, RunState, RunStatus};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::paths::AppPaths;
use crate::runner::events::{
    transcript_path, ContentBlock, EventStream, RateLimitEvent, ResultEvent, RunEvent, TokenUsage,
};
use crate::tasks::{move_task_to_bottom, set_run_state};

/// The `terminal_reason` and `subtype` vocabulary the corpus proves. Anything
/// outside it is an unknown termination, and ADR-0011 says an unknown
/// termination is [`ExitClass::Transient`].
const TERMINAL_COMPLETED: &str = "completed";
const TERMINAL_ABORTED_STREAMING: &str = "aborted_streaming";
const TERMINAL_MAX_TURNS: &str = "max_turns";
const SUBTYPE_SUCCESS: &str = "success";
const SUBTYPE_MAX_TURNS: &str = "error_max_turns";

/// The one `rate_limit_info.status` any recording has ever carried
/// (`spike/FINDINGS.md` §4). Every other value is a limit; see [`classify`].
const RATE_LIMIT_ALLOWED: &str = "allowed";

// ---------------------------------------------------------------------------
// What the classifier is allowed to look at
// ---------------------------------------------------------------------------

/// Everything known about how a run ended.
///
/// Deliberately a view over borrowed events rather than an owned copy: the
/// terminal facts already live on the [`EventStream`] that folded them, and a
/// second copy is a second thing that can disagree with the transcript.
///
/// [`Default`] is "the stream ended without a `result` and nobody cancelled",
/// which is a real condition — `truncated-stream.jsonl` — and classifies as
/// [`ExitClass::Transient`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Termination<'a> {
    /// The terminal event, or `None` for a stream that never reached one.
    pub result: Option<&'a ResultEvent>,
    /// The most recent `rate_limit_event`. Present on every real run.
    pub rate_limit: Option<&'a RateLimitEvent>,
    /// Whether *Rimaia* asked for this ending. A user cancellation and an
    /// outside kill produce the same `aborted_streaming` result; only the caller
    /// knows which one happened, which is why this is a field and not something
    /// the stream could tell us.
    pub cancel_requested: bool,
    /// Carried for the error message, never for the classification — see this
    /// module's header on why 143 makes the exit code untrustworthy here.
    pub exit_code: Option<i32>,
    /// How many tool calls were refused for want of approval. Carried for the
    /// message on the same terms as [`exit_code`](Self::exit_code): a run that
    /// was refused into stopping ends with no `result` to speak for it, and
    /// "the stream ended" is a true sentence that explains nothing.
    pub denied_tool_calls: u64,
}

impl<'a> Termination<'a> {
    /// The terminal facts a finished [`EventStream`] already holds.
    pub fn from_stream(stream: &'a EventStream) -> Self {
        Self {
            result: stream.result(),
            rate_limit: stream.rate_limit(),
            cancel_requested: false,
            exit_code: None,
            denied_tool_calls: stream.denied_tool_calls(),
        }
    }

    /// Marks the ending as one Rimaia asked for.
    pub fn cancelled(mut self) -> Self {
        self.cancel_requested = true;
        self
    }

    pub fn exited_with(mut self, exit_code: Option<i32>) -> Self {
        self.exit_code = exit_code;
        self
    }

    /// Whether the usage window reported anything other than "you may proceed".
    ///
    /// The predicate is "not `allowed`" rather than a match against a set of
    /// limit values, because the corpus proves exactly one value and it is this
    /// one. A closed set of guessed alternatives would be a fabricated contract
    /// in the one module ADR-0011 says must not have one — and it would fail
    /// *closed*, classifying a real limit as a hard failure at 2am, which is the
    /// precise mistake that ADR names. See `spike/FINDINGS.md` §4.
    fn hit_a_usage_limit(&self) -> bool {
        self.rate_limit
            .and_then(|limit| limit.status.as_deref())
            .is_some_and(|status| !status.eq_ignore_ascii_case(RATE_LIMIT_ALLOWED))
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Which of ADR-0011's six classes this ending is.
///
/// The order of the tests below *is* the decision, so it is worth reading as
/// one:
///
/// 1. **Cancellation wins outright.** Rimaia asked for this, so whatever the CLI
///    said on its way out describes a killing we ordered.
/// 2. **A completed run is a success even under a usage warning.** The
///    `rate_limit_event` arrives early on every run; a run that finished the
///    work despite one is finished.
/// 3. **No `result` at all** still asks the usage-limit predicate — there is no
///    terminal reason here for a limit to outrank, so nothing is lost by
///    keeping the protection — and is otherwise [`Transient`](ExitClass::Transient).
///    ADR-0011's own signal list gives "empty stream" to that class and its
///    Consequences say "unknown terminations default to `transient` with
///    limited attempts". It is deliberately **not** `interrupted`:
///    seam-contract D9 and ADR-0011's startup reconciliation reserve that word
///    for a run that died *with the app*, discovered by the reconciler from a
///    row left `running`. A classifier that also produced it would make the
///    word mean two things and stop `status = 'interrupted'` answering "did
///    Rimaia crash?".
/// 4. **`max_turns` is [`Fatal`](ExitClass::Fatal)**, named in ADR-0011's fatal
///    row, and it is tested *before* the usage-limit check below. A turn limit
///    is the one terminal reason a rate limit cannot itself produce, and it is
///    the one ADR-0011 names fatal by hand — so it is the one thing a stray or
///    unrecognised `rate_limit_info.status` must not be allowed to outrank.
///    Retrying a turn limit just spends the same tokens again.
/// 5. **A usage limit outranks every other terminal reason.** Whatever the CLI
///    reports after hitting the wall — including `aborted_streaming` below,
///    which a real limit plausibly produces — waiting for the reset is the
///    recovery, and misreading it as `fatal` is the failure mode ADR-0011 calls
///    out by name.
/// 6. **`aborted_streaming` is [`Interrupted`](ExitClass::Interrupted)** —
///    positive evidence from the CLI that the run was killed mid-stream, which
///    is ADR-0011's "process died". The corpus backs the action too:
///    `interrupted-sigterm.jsonl` and `resume-success.jsonl` share a
///    `session_id`, which is that class's "resume once immediately" actually
///    working.
/// 7. **Everything else is transient**, per the same ADR-0011 default. Retrying
///    a fatal error a few times is cheaper than abandoning a recoverable one.
pub fn classify(termination: &Termination<'_>) -> ExitClass {
    if termination.cancel_requested {
        return ExitClass::Cancelled;
    }

    if termination.result.is_some_and(is_success) {
        return ExitClass::Success;
    }

    let Some(result) = termination.result else {
        return if termination.hit_a_usage_limit() {
            ExitClass::UsageLimit
        } else {
            ExitClass::Transient
        };
    };

    match (result.terminal_reason.as_deref(), result.subtype.as_deref()) {
        (Some(TERMINAL_MAX_TURNS), _) | (_, Some(SUBTYPE_MAX_TURNS)) => ExitClass::Fatal,
        _ if termination.hit_a_usage_limit() => ExitClass::UsageLimit,
        (Some(TERMINAL_ABORTED_STREAMING), _) => ExitClass::Interrupted,
        _ => ExitClass::Transient,
    }
}

/// A run that both said it was not an error and named a shape of success the
/// corpus proves.
///
/// The conjunction is deliberate. `is_error: false` alone would let a renamed
/// `terminal_reason` read as success, and the wrong direction to fail in is
/// "declare victory": a success moves the task to `in_review` and stops, where
/// the [`Transient`](ExitClass::Transient) default resumes the session and finds
/// the work already done.
fn is_success(result: &ResultEvent) -> bool {
    !result.is_error
        && (result.terminal_reason.as_deref() == Some(TERMINAL_COMPLETED)
            || result.subtype.as_deref() == Some(SUBTYPE_SUCCESS))
}

// ---------------------------------------------------------------------------
// The outcome
// ---------------------------------------------------------------------------

/// Everything [`finish_run`] writes onto a `runs` row, plus the two numbers the
/// row has no column for.
///
/// The metrics are **extracted, never derived**: `spike/FINDINGS.md` §6 found
/// `num_turns`, `total_cost_usd`, `duration_ms`, `usage`, `modelUsage` and
/// `permission_denials` already on the terminal event.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    pub exit_class: ExitClass,
    /// The coarser lifecycle the Runs view queries. See [`status_for`].
    pub status: RunStatus,
    /// What went wrong, in a sentence a card can render. `None` on success.
    pub error_message: Option<String>,
    pub num_turns: Option<i64>,
    pub cost_usd: Option<f64>,
    /// The CLI's own measure of how long the run took. Reported but **not
    /// persisted**: ADR-0013 keeps the row to "only what the UI queries", and
    /// the row's timings are `started_at` and `ended_at` — which are when
    /// *Rimaia* started and stopped watching, and are the honest answer for a
    /// run whose process outlived its stream.
    pub duration_ms: Option<i64>,
    pub pr_url: Option<String>,
    /// When the usage window reports it resets — populated only when
    /// [`exit_class`](Self::exit_class) is [`ExitClass::UsageLimit`], because on
    /// every other run this is just "your five-hour window rolls over at some
    /// point" and saying so would read as a limit that was hit.
    ///
    /// **What the CLI said**, which is not the same thing as
    /// [`resume_after`](Self::resume_after) — see that field.
    pub usage_limit_resets_at: Option<DateTime<Utc>>,
    /// **What the policy decided**: when the next attempt of this task becomes
    /// due, or `None` for an attempt nothing will follow.
    ///
    /// Beside `usage_limit_resets_at` rather than replacing it, and keeping
    /// both is deliberate. One is an observation — the window the CLI reported
    /// — and the other is a decision made from it, ADR-0011's "reset plus
    /// jitter", which for a `transient` ending is not derived from it at all.
    /// A single field would make a morning reviewer unable to tell "the window
    /// reopened at 06:00 and we waited until 06:41" from "we invented 06:41".
    /// This is the column `runs.resume_after` has been waiting for since the
    /// initial schema; task 008 left it NULL because filling it needs a retry
    /// policy, which is task 014's.
    pub resume_after: Option<DateTime<Utc>>,
    /// What the attempt was spawned as. Filled by [`crate::runner::execute`],
    /// which is the only caller holding the [`Invocation`](crate::runner::Invocation)
    /// and the `init` event at once; `None` everywhere else, including the
    /// hand-made outcomes reconciliation and a failed spawn produce.
    pub spawned_as: SpawnedAs,
    /// What the attempt spent, off the terminal `result` event.
    pub usage: TokenUsage,
}

/// The three ADR-0022 columns that describe *how* a run was started.
///
/// They ride on the outcome rather than being read at `finish_run` time because
/// none of them can be recovered afterwards: `tasks.model` is rewritten by a
/// planner or a human (ADR-0016) and `run_environment` was a setting when the run
/// started. Seam-contract D18: every `None` here reaches the column as NULL and
/// means *not recorded*.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpawnedAs {
    /// The model the run actually used. Preferring the `init` event over the
    /// invocation is deliberate: the flag may have been absent (the CLI's own
    /// default) or an alias, and `init` echoes back the resolved name — which is
    /// the thing a later chart wants to group by.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `inherit` or `strict_local`, in the wire spelling the setting uses.
    pub run_environment: Option<String>,
}

impl RunOutcome {
    /// Classifies `termination` and pulls the metrics off its `result`.
    ///
    /// `pr_url` comes from [`PullRequestWatch`] rather than from here, because
    /// finding it means watching the whole run rather than reading its last
    /// event.
    pub fn of(termination: &Termination<'_>, pr_url: Option<String>) -> Self {
        let exit_class = classify(termination);
        let result = termination.result;

        Self {
            exit_class,
            status: status_for(exit_class),
            error_message: error_message(exit_class, termination),
            num_turns: result.and_then(|result| result.num_turns),
            cost_usd: result.and_then(|result| result.total_cost_usd),
            duration_ms: result.and_then(|result| result.duration_ms),
            pr_url,
            usage_limit_resets_at: match exit_class {
                ExitClass::UsageLimit => termination
                    .rate_limit
                    .and_then(RateLimitEvent::resets_at_utc),
                _ => None,
            },
            // Not known here either, and for a sharper reason than the fields
            // below: this function sees one ending, and the policy that fills
            // this needs the *history* — how many attempts this session has
            // already spent. `runner::process::run_task` is the only caller
            // holding both, so it is the one place a decision is made.
            resume_after: None,
            // Not known here: this function sees a termination, not the
            // invocation that caused it. `execute` fills it.
            spawned_as: SpawnedAs::default(),
            usage: result.map(|result| result.usage).unwrap_or_default(),
        }
    }
}

/// ADR-0011's six classes collapsed onto the four terminal values the `runs`
/// row carries.
///
/// `usage_limit`, `transient` and `fatal` all land on
/// [`Failed`](RunStatus::Failed) because whether a failure will be retried is
/// the *task's* business (`run_state = waiting_retry`) and not this row's — the
/// attempt is over either way, and the class is still on the row for anyone who
/// wants the distinction.
fn status_for(exit_class: ExitClass) -> RunStatus {
    match exit_class {
        ExitClass::Success => RunStatus::Succeeded,
        ExitClass::Cancelled => RunStatus::Cancelled,
        ExitClass::Interrupted => RunStatus::Interrupted,
        ExitClass::UsageLimit | ExitClass::Transient | ExitClass::Fatal => RunStatus::Failed,
    }
}

/// The sentence the card shows, in descending order of how much it tells a human
/// at 2am: the CLI's own `errors`, then the agent's closing text, then the
/// terminal vocabulary itself.
fn error_message(exit_class: ExitClass, termination: &Termination<'_>) -> Option<String> {
    match exit_class {
        ExitClass::Success => None,
        ExitClass::Cancelled => Some("the run was cancelled".to_string()),
        ExitClass::UsageLimit => Some(usage_limit_message(termination.rate_limit)),
        _ => Some(match termination.result {
            Some(result) => failure_message(result),
            // The exit code earns its keep here and nowhere else: it does not
            // classify anything, but "exited 137" versus "exited 1" is the
            // difference between an OOM kill and a crash for whoever reads this.
            None => {
                let mut message = match termination.exit_code {
                    Some(code) => format!(
                        "the event stream ended without a result event; the process exited with code {code}"
                    ),
                    None => "the event stream ended without a result event".to_string(),
                };
                // The one fact that turns "it stopped" into "it was never
                // allowed to start". Appended only here, where there is no
                // `result` to say anything better.
                if termination.denied_tool_calls > 0 {
                    message.push_str(&refusal_clause(termination.denied_tool_calls));
                }
                message
            }
        }),
    }
}

/// Names the refusals, and what the operator can do about them: the mode is
/// the actual lever (ADR-0012 — a manual run is deliberately `acceptEdits`,
/// and a queued one on an opted-in repository is not).
fn refusal_clause(denied: u64) -> String {
    format!(
        "; {denied} tool call{} {} refused for want of approval, so the run could not do \
         anything its permission mode had not already allowed",
        if denied == 1 { "" } else { "s" },
        if denied == 1 { "was" } else { "were" },
    )
}

fn failure_message(result: &ResultEvent) -> String {
    if !result.errors.is_empty() {
        return result.errors.join("; ");
    }
    if let Some(text) = result
        .result
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return text.to_string();
    }

    format!(
        "the run ended with subtype \"{subtype}\" and terminal reason \"{reason}\"",
        subtype = result.subtype.as_deref().unwrap_or("unknown"),
        reason = result.terminal_reason.as_deref().unwrap_or("unknown"),
    )
}

/// Assembled from the three field names the corpus proves and nothing else — a
/// message quoting an invented `status` value would read as observed fact.
fn usage_limit_message(rate_limit: Option<&RateLimitEvent>) -> String {
    let mut message = "the run stopped at a usage limit".to_string();
    let Some(rate_limit) = rate_limit else {
        return message;
    };

    if let Some(kind) = rate_limit.rate_limit_type.as_deref() {
        message.push_str(&format!(" ({kind})"));
    }
    if let Some(reset) = rate_limit.resets_at_utc() {
        message.push_str(&format!("; it resets at {}", reset.to_rfc3339()));
    }
    message
}

// ---------------------------------------------------------------------------
// The pull-request URL
// ---------------------------------------------------------------------------

/// Watches a run for the pull request it opened.
///
/// # Where a PR URL is allowed to come from
///
/// **Only the agent's own narration**: the `result` event's closing summary
/// first, and failing that the `text` blocks of its `assistant` messages, last
/// mention winning. Three sources are excluded on purpose, because the question
/// is not "does a PR URL appear in this run" but "did *this run* open one":
///
/// - **Tool inputs.** A URL there is one the agent is *consuming* — a `WebFetch`
///   of a PR, a `gh pr view <url>`. Reading a pull request is not opening one.
/// - **Tool results.** `runner::events` deliberately does not model their
///   content, and the reason applies here too: a `gh pr list` result is a list
///   of pull requests this run did not open.
/// - **The composed prompt.** Structurally impossible rather than merely
///   avoided: the prompt goes in on stdin and is not echoed into the stream —
///   every `user` event in the corpus carries tool results. That is what stops a
///   task link naming an existing PR (ADR-0007's links are "an Asana task, a
///   GitHub issue, a doc") from being reported as this run's work.
///
/// The summary outranks the narration because it is the agent answering "what
/// did you do"; the seeded base instructions ask it to "push the branch and open
/// a pull request describing what changed and why", so that is the designed
/// channel. The narration is kept as a fallback for the run that opened a PR and
/// was then killed before it could summarise — `interrupted-sigterm.jsonl` is
/// exactly that shape, and its `result` carries no summary at all.
///
/// **The residual false positive is accepted and named:** an agent whose summary
/// quotes a pull request it merely read. The alternative — gating on a
/// PR-creating tool call, `gh pr create` or an MCP `create_pull_request` — trades
/// it for an allowlist of tool names that rots exactly the way ADR-0011 says this
/// module must not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullRequestWatch {
    from_summary: Option<String>,
    from_narration: Option<String>,
}

impl PullRequestWatch {
    /// Folds one event in. O(1) memory: only the latest match from each source
    /// is kept, because retaining a run's events to scan them later is the thing
    /// ADR-0013 keeps off the heap.
    pub fn observe(&mut self, event: &RunEvent) {
        match event {
            RunEvent::Assistant(assistant) => {
                for block in &assistant.content {
                    if let ContentBlock::Text(body) = block {
                        if let Some(url) = last_pull_request_url(body) {
                            self.from_narration = Some(url);
                        }
                    }
                }
            }
            RunEvent::Result(result) => {
                if let Some(url) = result.result.as_deref().and_then(last_pull_request_url) {
                    self.from_summary = Some(url);
                }
            }
            _ => {}
        }
    }

    pub fn url(&self) -> Option<&str> {
        self.from_summary
            .as_deref()
            .or(self.from_narration.as_deref())
    }

    pub fn into_url(self) -> Option<String> {
        self.from_summary.or(self.from_narration)
    }
}

/// The path segment that names a pull request, per forge. Three spellings rather
/// than GitHub's alone because ADR-0005 registers *local git repositories* and
/// says nothing about where their remotes live.
const PULL_REQUEST_SEGMENTS: [&str; 3] = ["pull", "pull-requests", "merge_requests"];

/// How many path segments must precede the one above. Two, so an owner and a
/// repository are named: `https://host/pull/1` is not a pull request URL, and
/// requiring them is most of what keeps prose from matching.
const SEGMENTS_BEFORE: usize = 2;

/// Characters that end a URL rather than belong to one. `)` and `>` are here for
/// Markdown — `[#42](https://…/pull/42)` and `<https://…>` are how an agent
/// usually writes a link.
const URL_TERMINATORS: [char; 10] = ['<', '>', '"', '\'', '`', ')', ']', '}', ',', '\\'];

/// Trailing characters a sentence leaves on a URL. The trailing `/` goes too, so
/// one PR has one spelling.
const URL_TRAILING: [char; 7] = ['.', ',', ';', ':', '!', '?', '/'];

/// The last pull-request URL in `text`, canonicalised.
///
/// Last rather than first because a run that opens a pull request mentions it at
/// the end; an earlier mention is more likely to be context it was given.
fn last_pull_request_url(text: &str) -> Option<String> {
    let mut found = None;
    let mut from = 0;

    while let Some(offset) = text[from..].find("http") {
        let start = from + offset;
        from = start + "http".len();

        let tail = &text[start..];
        if !(tail.starts_with("https://") || tail.starts_with("http://")) {
            continue;
        }

        let end = tail
            .find(|character: char| {
                character.is_whitespace() || URL_TERMINATORS.contains(&character)
            })
            .unwrap_or(tail.len());
        if let Some(url) = canonical_pull_request_url(&tail[..end]) {
            found = Some(url);
        }
    }

    found
}

/// `scheme://host/owner/repo/pull/42` for anything that names a pull request,
/// and `None` for everything else.
///
/// Canonical, so `…/pull/42/files`, `…/pull/42#discussion_r1` and `…/pull/42/`
/// are one stored value. The number is required: `…/pull/` and `…/pulls` never
/// match, which is most of the difference between a link to a pull request and a
/// link to a list of them.
fn canonical_pull_request_url(candidate: &str) -> Option<String> {
    let candidate = candidate.trim_end_matches(|character| URL_TRAILING.contains(&character));
    let (scheme, rest) = candidate.split_once("://")?;
    let rest = rest.split(['#', '?']).next().unwrap_or(rest);

    let mut path = rest.split('/');
    let host = path.next().filter(|host| !host.is_empty())?;
    let segments: Vec<&str> = path.collect();

    segments
        .iter()
        .enumerate()
        .skip(SEGMENTS_BEFORE)
        .find(|(index, segment)| {
            PULL_REQUEST_SEGMENTS.contains(segment)
                && segments
                    .get(index + 1)
                    .is_some_and(|number| is_pull_request_number(number))
        })
        .map(|(index, _)| {
            format!(
                "{scheme}://{host}/{path}",
                path = segments[..=index + 1].join("/")
            )
        })
}

fn is_pull_request_number(segment: &str) -> bool {
    !segment.is_empty() && segment.chars().all(|character| character.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// The `runs` row
// ---------------------------------------------------------------------------

/// What a run needs before its process exists.
///
/// The session id is generated by Rimaia *up front* rather than read off the
/// `init` event, so `--resume` works even if the process dies before announcing
/// itself (ADR-0004). ADR-0011 has every attempt of a task's retry loop share
/// this id, so the history of an overnight task reads as one continued session
/// — and since task 014 that is what happens: [`crate::runner::process::run_task`]
/// mints a fresh id for a first attempt and reuses the session's own for a
/// resume, which is also the boundary
/// [`scheduler::attempts`](crate::scheduler::attempts) counts a retry budget
/// against.
///
/// [`prompt`](Self::prompt) is what was *sent*, verbatim — so a resume stores
/// the one-line continuation rather than a second copy of the composed prompt,
/// and a morning reviewer reading four rows sees one long prompt and three
/// one-liners: the sequence of walls the task hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRun {
    pub task_id: String,
    pub session_id: String,
    /// The composed prompt, verbatim, stored as a copy (ADR-0009). Task 006's
    /// "editing base instructions does not alter any already-stored run prompt"
    /// is satisfied here and only here.
    pub prompt: String,
}

/// Opens the `runs` row for an attempt that is about to start.
///
/// This module is the only writer of that table, which is the same rule
/// [`set_run_state`] states for `tasks.run_state` and for the same ADR-0006
/// reason. It writes the row and nothing else: putting the *task* into
/// `run_state = running` belongs to whoever selected it, inside the transaction
/// ADR-0010 requires for selection.
///
/// `log_path` is computed rather than passed, because ADR-0013 makes it a pure
/// function of the task and run ids and the run id is minted here.
pub async fn start_run(ctx: &ServiceContext, paths: &AppPaths, new_run: NewRun) -> Result<Run> {
    let id = new_id();
    let log_path = transcript_path(paths, &new_run.task_id, &id)
        .to_string_lossy()
        .into_owned();
    let started_at = ctx.clock.now();

    let mut tx = ctx.pool.begin().await?;

    // The foreign key would refuse a task that does not exist, but as a
    // constraint violation nobody can read. Same sentence `tasks::get_task`
    // answers the identical question with.
    let task_exists: i64 =
        sqlx::query_scalar!("SELECT count(*) FROM tasks WHERE id = ?1", new_run.task_id)
            .fetch_one(&mut *tx)
            .await?;
    if task_exists == 0 {
        return Err(Error::not_found(format!(
            "no task with id {}",
            new_run.task_id
        )));
    }

    // Inside the transaction, because `idx_runs_task_attempt` is UNIQUE on
    // `(task_id, attempt)`: two writers racing to claim a task must not both
    // record attempt 3, and reading the maximum outside would let them.
    let previous: Option<i64> = sqlx::query_scalar!(
        r#"SELECT max(attempt) AS "attempt: i64" FROM runs WHERE task_id = ?1"#,
        new_run.task_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    let attempt = previous.unwrap_or(0) + 1;

    sqlx::query!(
        r#"INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, log_path)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
        id,
        new_run.task_id,
        attempt,
        RunStatus::Running,
        new_run.session_id,
        new_run.prompt,
        started_at,
        log_path,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // Both, and after the commit (ADR-0018). The task's own id rides along
    // because a card renders its last run (seam-contract D12) — a board told
    // only about the run would keep drawing the previous attempt's badge.
    ctx.publish(ChangeEvent::runs([id.clone()]));
    ctx.publish(ChangeEvent::tasks([new_run.task_id.clone()]));

    fetch_run_row(&ctx.pool, &id).await
}

/// Closes the `runs` row and applies the outcome to the task.
///
/// Three transactions rather than one, and the order is the point: the row is
/// written **first**, so a crash between them leaves the outcome recorded and
/// only the task stale, which is the recoverable direction. It cannot be one
/// transaction because [`set_run_state`] and [`move_task_to_bottom`] own theirs — and
/// reaching around them to write `run_state` or `board_column` directly is the
/// exact ADR-0006 bug both of those functions exist to prevent.
///
/// Refuses a run that already ended: finalising twice would run the task-side
/// transitions again from a state they no longer apply to.
pub async fn finish_run(ctx: &ServiceContext, run_id: &str, outcome: &RunOutcome) -> Result<Run> {
    let ended_at = ctx.clock.now();

    let mut tx = ctx.pool.begin().await?;
    let run = fetch_run_row(&mut *tx, run_id).await?;
    if run.ended_at.is_some() {
        return Err(Error::invalid(format!(
            "run {run_id} has already been finalized"
        )));
    }

    let pr_url = outcome.pr_url.as_deref();
    let error_message = outcome.error_message.as_deref();
    // ADR-0022's seven, written here and nowhere else, once per row. Every one
    // of them is `Option` all the way from the event to the bind, so a value
    // nobody observed lands as NULL rather than as a zero a later chart would
    // average (seam-contract D18).
    let model = outcome.spawned_as.model.as_deref();
    let effort = outcome.spawned_as.effort.as_deref();
    let run_environment = outcome.spawned_as.run_environment.as_deref();
    // One more bound parameter on the `UPDATE` that was already being issued,
    // rather than a second transaction: the deadline and the class it was
    // decided from are one fact about one attempt, and a crash between two
    // writes of them would leave a task the queue reads as retryable with
    // nothing to retry at.
    sqlx::query!(
        r#"UPDATE runs
              SET ended_at = ?1, status = ?2, exit_class = ?3, error_message = ?4,
                  num_turns = ?5, cost_usd = ?6, pr_url = ?7,
                  model = ?8, effort = ?9, run_environment = ?10,
                  input_tokens = ?11, output_tokens = ?12,
                  cache_read_tokens = ?13, cache_creation_tokens = ?14,
                  resume_after = ?15
            WHERE id = ?16"#,
        ended_at,
        outcome.status,
        outcome.exit_class,
        error_message,
        outcome.num_turns,
        outcome.cost_usd,
        pr_url,
        model,
        effort,
        run_environment,
        outcome.usage.input_tokens,
        outcome.usage.output_tokens,
        outcome.usage.cache_read_tokens,
        outcome.usage.cache_creation_tokens,
        outcome.resume_after,
        run_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    ctx.publish(ChangeEvent::runs([run_id.to_string()]));
    ctx.publish(ChangeEvent::tasks([run.task_id.clone()]));

    apply_to_task(ctx, &run.task_id, outcome).await?;

    fetch_run_row(&ctx.pool, run_id).await
}

/// Where the task lands, per ADR-0011's action column.
///
/// Only `success` also moves the card — that is task 008's scope, and it is the
/// board's whole promise: work that finished is waiting to be reviewed. The
/// other five leave the card where it is, because ADR-0007's failure rule keeps
/// a failed task in `ready` and shows the failure on it.
///
/// Every class transitions *out* of `running`. A task left `running` with no
/// process is a state nothing can recover from and a badge that lies.
///
/// # The class alone is not enough any more
///
/// Task 008 routed on `exit_class` and put all three retryable classes in
/// `waiting_retry`, which was right while nothing resumed a waiting task —
/// naming the state was the whole of what it could do. Now a retryable class
/// means two different things depending on whether the budget is spent:
///
/// ```text
/// usage_limit | transient | interrupted, resume_after.is_some() => waiting_retry
/// usage_limit | transient | interrupted, otherwise             => failed
/// ```
///
/// The `otherwise` arm is what makes "transient retries stop at the cap and the
/// task lands in `failed` with the reason" true. Without it a task that had
/// exhausted its five attempts would sit in `waiting_retry` with no deadline —
/// invisible to a morning review that ADR-0007 wants a failure to interrupt,
/// and skipped forever by the queue's own selection. The *reason* is still on
/// the run's `exit_class` and `error_message`, never on `run_state`
/// (seam-contract D9's two dimensions).
async fn apply_to_task(ctx: &ServiceContext, task_id: &str, outcome: &RunOutcome) -> Result<()> {
    if outcome.exit_class == ExitClass::Success {
        move_to_in_review(ctx, task_id).await?;
    }

    let run_state = match outcome.exit_class {
        ExitClass::Success => RunState::Idle,
        // ADR-0011 for `fatal` ("no retry... run_state = failed"), and ADR-0010
        // for a cancelled run: cancel-one on a *running* task "goes to `failed`
        // with `cancelled` reason". `Running -> Cancelled` is illegal by design;
        // the reason lives on the run's `exit_class`, not on `run_state`.
        ExitClass::Fatal | ExitClass::Cancelled => RunState::Failed,
        ExitClass::UsageLimit | ExitClass::Transient | ExitClass::Interrupted => {
            match outcome.resume_after {
                Some(_) => RunState::WaitingRetry,
                None => RunState::Failed,
            }
        }
    };

    set_run_state(ctx, task_id, run_state).await?;
    Ok(())
}

/// Appends the task to the bottom of `in_review`.
///
/// Through [`move_task_to_bottom`], which does the neighbour lookup inside the
/// transaction that writes — seam-contract D1's rule still holds (the caller
/// names neighbours, never a position; the arithmetic stays `position.rs`'s),
/// it is only the *read* that moved.
///
/// It moved because task 012 made it wrong. This function used to run the
/// lookup here, against the pool, and pass the id it found to `move_task`; the
/// gap between the two was accepted in a comment as an ordering nit, on the
/// stated grounds that a card landing in `in_review` in between would put this
/// one second from the bottom instead of last. With two runs finishing at once
/// the consequence is worse than that: both read the same bottom card, both
/// compute the same midpoint against it, and both land on the same position.
/// See `tasks::move_task_to_bottom` for the rest of the argument.
async fn move_to_in_review(ctx: &ServiceContext, task_id: &str) -> Result<()> {
    move_task_to_bottom(ctx, task_id, BoardColumn::InReview).await?;
    Ok(())
}

/// The one place a `runs` row is read back — inside a transaction before the
/// write that depends on it, and against the pool afterwards to hand the caller
/// what was actually stored. Generic over the executor for the same reason
/// `tasks::service::fetch_task_row` is.
async fn fetch_run_row<'e, E>(executor: E, id: &str) -> Result<Run>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query_as!(
        Run,
        r#"SELECT id, task_id, attempt, status AS "status: RunStatus", session_id, prompt,
            started_at AS "started_at: DateTime<Utc>", ended_at AS "ended_at: DateTime<Utc>",
            exit_class AS "exit_class: ExitClass", error_message, num_turns, cost_usd, log_path,
            pr_url, resume_after AS "resume_after: DateTime<Utc>", base_ref,
            model, effort, run_environment, input_tokens, output_tokens,
            cache_read_tokens, cache_creation_tokens
           FROM runs WHERE id = ?1"#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| Error::not_found(format!("no run with id {id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// The corpus proves neither of these — no recording opened a pull request,
    /// because the spike ran against a throwaway local repository with no
    /// remote. What it *can* prove is the rule, which is a pure function of a
    /// string, so it is tested as one.
    #[test]
    fn a_canonical_pull_request_url_keeps_the_forge_the_owner_and_the_number() {
        for (text, expected) in [
            (
                "Opened https://github.com/abtion/rimaia/pull/42 for review.",
                "https://github.com/abtion/rimaia/pull/42",
            ),
            // Trailing path, fragment and query all collapse to one spelling, so
            // the same pull request is never stored two ways.
            (
                "see https://github.com/abtion/rimaia/pull/42/files",
                "https://github.com/abtion/rimaia/pull/42",
            ),
            (
                "see https://github.com/abtion/rimaia/pull/42#discussion_r1",
                "https://github.com/abtion/rimaia/pull/42",
            ),
            (
                "see https://github.com/abtion/rimaia/pull/42?w=1",
                "https://github.com/abtion/rimaia/pull/42",
            ),
            // Markdown, which is how an agent usually writes a link.
            (
                "[#42](https://github.com/abtion/rimaia/pull/42)",
                "https://github.com/abtion/rimaia/pull/42",
            ),
            (
                "<https://github.com/abtion/rimaia/pull/42>",
                "https://github.com/abtion/rimaia/pull/42",
            ),
            // Not GitHub. ADR-0005 registers local repositories and says nothing
            // about where their remotes live.
            (
                "https://gitlab.com/abtion/rimaia/-/merge_requests/7",
                "https://gitlab.com/abtion/rimaia/-/merge_requests/7",
            ),
            (
                "https://bitbucket.org/abtion/rimaia/pull-requests/7",
                "https://bitbucket.org/abtion/rimaia/pull-requests/7",
            ),
            // Self-hosted, on a port and over plain http.
            (
                "http://git.internal:8080/abtion/rimaia/pull/3",
                "http://git.internal:8080/abtion/rimaia/pull/3",
            ),
        ] {
            assert_eq!(
                last_pull_request_url(text).as_deref(),
                Some(expected),
                "in {text:?}"
            );
        }
    }

    #[test]
    fn a_url_that_does_not_name_one_pull_request_is_not_one() {
        for text in [
            "nothing here at all",
            // A list of pull requests is not a pull request.
            "https://github.com/abtion/rimaia/pulls",
            "https://github.com/abtion/rimaia/pull/",
            // A branch comparison, which is what `git push` prints.
            "https://github.com/abtion/rimaia/compare/rimaia/x?expand=1",
            // An issue is not a pull request, however adjacent.
            "https://github.com/abtion/rimaia/issues/42",
            // No owner and no repository: `SEGMENTS_BEFORE` refusing prose.
            "https://example.com/pull/42",
            // A word, not a URL.
            "the pull/42 branch",
        ] {
            assert_eq!(last_pull_request_url(text), None, "in {text:?}");
        }
    }

    #[test]
    fn the_last_pull_request_mentioned_is_the_one_reported() {
        // A run that opens a pull request mentions it at the end; an earlier
        // mention is more likely context it was handed.
        assert_eq!(
            last_pull_request_url(
                "Following https://github.com/abtion/rimaia/pull/1, opened \
                 https://github.com/abtion/rimaia/pull/2."
            )
            .as_deref(),
            Some("https://github.com/abtion/rimaia/pull/2")
        );
    }

    #[test]
    fn a_closing_summary_outranks_the_narration_it_followed() {
        let mut watch = PullRequestWatch {
            from_narration: Some("https://github.com/abtion/rimaia/pull/1".to_string()),
            from_summary: Some("https://github.com/abtion/rimaia/pull/2".to_string()),
        };

        assert_eq!(watch.url(), Some("https://github.com/abtion/rimaia/pull/2"));

        // And the narration stands alone when the run was killed before it could
        // summarise — which is what `interrupted-sigterm.jsonl` looks like.
        watch.from_summary = None;
        assert_eq!(watch.url(), Some("https://github.com/abtion/rimaia/pull/1"));
    }

    #[test]
    fn every_exit_class_has_exactly_one_run_status() {
        // The collapse ADR-0013's row documents, asserted rather than assumed:
        // three classes share `failed`, and the two that do not are the two
        // seam-contract D9 and ADR-0010 give their own word to.
        assert_eq!(status_for(ExitClass::Success), RunStatus::Succeeded);
        assert_eq!(status_for(ExitClass::Cancelled), RunStatus::Cancelled);
        assert_eq!(status_for(ExitClass::Interrupted), RunStatus::Interrupted);
        assert_eq!(status_for(ExitClass::UsageLimit), RunStatus::Failed);
        assert_eq!(status_for(ExitClass::Transient), RunStatus::Failed);
        assert_eq!(status_for(ExitClass::Fatal), RunStatus::Failed);
    }
}

/// What this installation's finished runs have actually cost.
///
/// Exists so the Settings copy can put
/// [`ENVIRONMENT_SETUP_COST_USD`](crate::db::settings::ENVIRONMENT_SETUP_COST_USD)
/// in proportion against real runs rather than against the spike's one-word
/// prompt. Without it the panel can only quote a ratio measured on a run that
/// did no work, which overstates the cost of `inherit` by an order of magnitude
/// on anything substantial.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCostSummary {
    /// `None` until something has finished and reported a cost.
    pub median_usd: Option<f64>,
    pub sample_size: i64,
}

/// The median cost of a finished run, and how many there were to look at.
///
/// **Median, not mean.** Run costs are wildly skewed — one $32 implementation
/// sits beside a dozen runs under a dollar — and a mean would let a single
/// outlier decide what the panel tells the user about every run they do.
///
/// Only runs that reported a cost count. A cancelled run that died before its
/// `result` has `NULL` here, and treating that as zero would drag the answer
/// toward nothing.
pub async fn observed_run_cost(pool: &sqlx::SqlitePool) -> Result<RunCostSummary> {
    let costs: Vec<f64> = sqlx::query_scalar!(
        r#"SELECT cost_usd AS "cost_usd!: f64" FROM runs
           WHERE cost_usd IS NOT NULL AND cost_usd > 0 ORDER BY cost_usd ASC"#,
    )
    .fetch_all(pool)
    .await?;

    let sample_size = costs.len() as i64;
    let median_usd = match costs.len() {
        0 => None,
        // The lower of the two middles on an even count, rather than their
        // mean: it is an actual run the user paid for, which is easier to
        // defend in a sentence than a number no run ever cost.
        n => Some(costs[(n - 1) / 2]),
    };

    Ok(RunCostSummary {
        median_usd,
        sample_size,
    })
}
