//! What a client hears, whichever transport carries it (task 048, ADR-0034
//! point 3, ADR-0018).
//!
//! # One mapping, two forwarders
//!
//! ADR-0018 kept the event-name strings in one function in the shell. There
//! are two forwarders now, the desktop shell's Tauri events and the server's
//! `GET /api/v1/events`, so the mapping lives here and both produce the same
//! [`WireEvent`]s:
//!
//! | Wire event | Payload | Board source | Local source |
//! | --- | --- | --- | --- |
//! | `tasks:changed` | array of task ids | `Change::Tasks` | `LocalChange::Worktrees` |
//! | `repositories:changed` | array of repository ids | `Change::Repositories` | `LocalChange::Checkouts` |
//! | `runs:changed` | array of run ids | `Change::Runs` | none |
//! | `settings:changed` | `null` | `Change::Settings` | none |
//! | `teams:changed` | array of team ids | `Change::Teams` | none |
//! | `runs:tail` | `RunTail` | the live channel, opt-in per run | none |
//! | `plan-pass:progress` | `PlanProgressView` | none | the shell's local plan pass |
//! | `runner:changed` | `null` | none | `LocalChange::Runner` |
//! | `schedules:changed` | array of schedule ids | none | `LocalChange::Schedules` |
//!
//! The board half ([`board_wire`], [`tail_wire`]) and the local half
//! ([`local_wire`], [`plan_progress_wire`]) are separate functions over
//! separate types, so no arm can map a local source onto a server's stream.
//!
//! # One subscriber's life
//!
//! [`Subscription`] is one client's stream: the opening re-read, admission by
//! the caller's teams, lag recovery, and re-checking the caller while it is
//! open. The server adds only HTTP framing to it, and the solo shell drives
//! the same type, so "works in solo" is evidence for "works connected".

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use super::{BoardHost, BoxFuture, Caller};
use crate::clock::Clock;
use crate::error::{Error, Result};
use crate::events::{Audience, Change, ChangeEvent, LocalChange, RunId, TeamId, TeamTail, UserId};
use crate::runner::events::RunTail;
use crate::runner::strategy::{PlanOutcome, PlanProgress, PlanResult};

pub const TASKS_CHANGED: &str = "tasks:changed";
pub const REPOSITORIES_CHANGED: &str = "repositories:changed";
pub const RUNS_CHANGED: &str = "runs:changed";
pub const SETTINGS_CHANGED: &str = "settings:changed";
pub const TEAMS_CHANGED: &str = "teams:changed";
pub const RUNS_TAIL: &str = "runs:tail";
pub const PLAN_PASS_PROGRESS: &str = "plan-pass:progress";
pub const RUNNER_CHANGED: &str = "runner:changed";
pub const SCHEDULES_CHANGED: &str = "schedules:changed";

/// One event as a client receives it: a name and its payload.
///
/// The Tauri forwarders emit `data` under `name`; the SSE stream writes
/// `event: <name>` and `data: <json>`, with no `id:` line, because there is no
/// log to resume from (ADR-0034 point 3 chose the re-read).
#[derive(Debug, Clone, PartialEq)]
pub struct WireEvent {
    pub name: &'static str,
    pub data: Value,
}

impl WireEvent {
    fn ids(name: &'static str, ids: &[String]) -> Self {
        Self {
            name,
            data: Value::from(ids.to_vec()),
        }
    }

    fn unit(name: &'static str) -> Self {
        Self {
            name,
            data: Value::Null,
        }
    }
}

/// A board change as the client hears it. The audience is routing metadata
/// and stays behind.
pub fn board_wire(change: &Change) -> WireEvent {
    match change {
        Change::Tasks(ids) => WireEvent::ids(TASKS_CHANGED, ids),
        Change::Repositories(ids) => WireEvent::ids(REPOSITORIES_CHANGED, ids),
        Change::Runs(ids) => WireEvent::ids(RUNS_CHANGED, ids),
        Change::Settings => WireEvent::unit(SETTINGS_CHANGED),
        Change::Teams(ids) => WireEvent::ids(TEAMS_CHANGED, ids),
    }
}

/// A machine change as the desktop hears it. `Checkouts` and `Worktrees` keep
/// the board's names, because a view re-reads the same ids either way and the
/// repository list and the cards already listen to them.
pub fn local_wire(change: &LocalChange) -> WireEvent {
    match change {
        LocalChange::Runner => WireEvent::unit(RUNNER_CHANGED),
        LocalChange::Schedules(ids) => WireEvent::ids(SCHEDULES_CHANGED, ids),
        LocalChange::Checkouts(ids) => WireEvent::ids(REPOSITORIES_CHANGED, ids),
        LocalChange::Worktrees(ids) => WireEvent::ids(TASKS_CHANGED, ids),
    }
}

/// A live snapshot as the client hears it: the snapshot alone, its team being
/// routing metadata.
pub fn tail_wire(tail: &RunTail) -> WireEvent {
    WireEvent {
        name: RUNS_TAIL,
        data: serde_json::to_value(tail).expect("a run tail always serializes"),
    }
}

/// One card of the desktop's local plan pass. Only that pass produces it (060
/// Scope 5), and it is emitted straight into Tauri, never onto a stream.
pub fn plan_progress_wire(progress: &PlanProgressView) -> WireEvent {
    WireEvent {
        name: PLAN_PASS_PROGRESS,
        data: serde_json::to_value(progress).expect("plan progress always serializes"),
    }
}

/// Every board entity, once, with an empty id list: "re-read this wholesale".
///
/// A stream opens with it, and sends it again after its change receiver
/// lags. The server does not know whether a connection is a first one or a
/// reconnect, and does not need to: a re-read on first load is harmless, and a
/// missed one after a reconnect is a stale board.
///
/// `teams:changed` matters here more than any other entity (task 051): a
/// person whose team set changed has their stream ended by
/// [`StreamControl::revalidate`], and the `Teams` event published just before
/// may be lost with it. The reconnect's burst is what makes the switcher
/// re-read.
pub fn board_reread() -> Vec<WireEvent> {
    vec![
        WireEvent::ids(TASKS_CHANGED, &[]),
        WireEvent::ids(REPOSITORIES_CHANGED, &[]),
        WireEvent::ids(RUNS_CHANGED, &[]),
        WireEvent::unit(SETTINGS_CHANGED),
        WireEvent::ids(TEAMS_CHANGED, &[]),
    ]
}

/// Every machine entity, once: what the shell's local forwarder sends after
/// its receiver lags.
pub fn local_reread() -> Vec<WireEvent> {
    vec![
        WireEvent::unit(RUNNER_CHANGED),
        WireEvent::ids(SCHEDULES_CHANGED, &[]),
        WireEvent::ids(REPOSITORIES_CHANGED, &[]),
        WireEvent::ids(TASKS_CHANGED, &[]),
    ]
}

// ---------------------------------------------------------------------------
// Plan progress's wire form
// ---------------------------------------------------------------------------

/// One card's line of a planning pass, `camelCase` for the client.
///
/// A projection rather than [`PlanResult`] re-serialized, for the reason
/// `mcp::responses` gives: the wire shape is the client's contract, and a core
/// enum reshaped by serde would make every rename a breaking change to the
/// window. `src/types.ts`'s `PlanResult` describes it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanResultView {
    pub task_id: String,
    pub title: String,
    /// `planned`, `skipped`, `failed` or `cancelled`.
    pub outcome: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub rationale: Option<String>,
    pub cost_usd: Option<f64>,
    pub skip: Option<String>,
    pub reason: Option<String>,
}

impl From<&PlanResult> for PlanResultView {
    fn from(result: &PlanResult) -> Self {
        let mut view = Self {
            task_id: result.task_id.clone(),
            title: result.title.clone(),
            outcome: "cancelled".to_string(),
            model: None,
            effort: None,
            rationale: None,
            cost_usd: None,
            skip: None,
            reason: None,
        };
        match &result.outcome {
            PlanOutcome::Planned {
                model,
                effort,
                rationale,
                cost_usd,
            } => {
                view.outcome = "planned".to_string();
                view.model = model.clone();
                view.effort = effort.clone();
                view.rationale = rationale.clone();
                view.cost_usd = *cost_usd;
            }
            PlanOutcome::Skipped(skip) => {
                view.outcome = "skipped".to_string();
                view.skip = Some(skip.as_str().to_string());
                view.reason = Some(skip.message());
            }
            PlanOutcome::Failed(reason) => {
                view.outcome = "failed".to_string();
                view.reason = Some(reason.clone());
            }
            PlanOutcome::Cancelled => {}
        }
        view
    }
}

/// What one `plan-pass:progress` event carries: the card that just finished,
/// how far through the pass it was, and what has been spent so far.
/// `src/types.ts`'s `PlanProgress` describes it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanProgressView {
    /// 1-based, because it is rendered as "3 of 10".
    pub completed: usize,
    pub total: usize,
    pub spent_usd: f64,
    pub result: PlanResultView,
}

impl From<&PlanProgress<'_>> for PlanProgressView {
    fn from(progress: &PlanProgress<'_>) -> Self {
        Self {
            completed: progress.index + 1,
            total: progress.total,
            spent_usd: progress.spent_usd,
            result: PlanResultView::from(progress.result),
        }
    }
}

// ---------------------------------------------------------------------------
// The subscription
// ---------------------------------------------------------------------------

/// How many runs one stream may ask to tail. A client that wants more is a
/// client bug: 049 caps at this before it connects.
pub const MAX_TAILS_PER_STREAM: usize = 8;

/// How long an open stream goes without resolving its caller again, when
/// nothing signals it to sooner: the backstop for what nothing signals, such
/// as a desktop token reaching its `expires_at` (ADR-0030 point 2).
pub const STREAM_REVALIDATE_INTERVAL: Duration = Duration::from_secs(60);

/// How many revalidation signals a slow stream may miss before it is told it
/// lagged, which it answers by revalidating at once.
pub const STREAM_CONTROL_CAPACITY: usize = 64;

/// Which runs' tails a stream delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tails {
    /// Every run's, as the solo desktop has always heard them.
    All,
    /// Only these runs', named at connect with `?tail=`.
    Only(BTreeSet<RunId>),
}

impl Tails {
    /// The runs a client asked for: at most [`MAX_TAILS_PER_STREAM`], each a
    /// UUID, which every run id is (`db::new_id`). A malformed id or one too
    /// many is `Invalid`, a client bug. A well-formed id is never looked up:
    /// admission already requires a tail's team to be the caller's, so an
    /// unknown run, a deleted one and another team's behave alike, and there
    /// is nothing to probe.
    pub fn requested(ids: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut runs = BTreeSet::new();
        for (count, id) in ids.into_iter().enumerate() {
            if count >= MAX_TAILS_PER_STREAM {
                return Err(Error::invalid(format!(
                    "a stream may tail at most {MAX_TAILS_PER_STREAM} runs"
                )));
            }
            if uuid::Uuid::try_parse(&id).is_err() {
                return Err(Error::invalid("a run to tail is named by its id"));
            }
            runs.insert(id);
        }
        Ok(Self::Only(runs))
    }

    fn admits(&self, run_id: &str) -> bool {
        match self {
            Tails::All => true,
            Tails::Only(runs) => runs.contains(run_id),
        }
    }
}

/// Resolves a stream's caller again, as the route that opened it resolved it
/// (task 048 Scope 7): the credential it was opened with through
/// `Authenticate`, then every later edge step, such as task 050's
/// `Caller::narrow_to`. Built by the route, which holds the credential.
pub type Revalidate = Box<dyn Fn() -> BoxFuture<'static, Result<Caller>> + Send + Sync>;

/// Tells every open stream of a user to resolve its caller again before it
/// delivers anything more.
///
/// A broadcast of user ids on `BoardHost`. Task 048 signals it from 047's
/// revocation paths (signing out, revoking a session, revoking a token), and
/// task 051 from every change to a person's team set: creating, joining,
/// leaving or deleting a team, being removed from one, or a role change.
#[derive(Clone)]
pub struct StreamControl {
    sender: broadcast::Sender<UserId>,
}

impl StreamControl {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(STREAM_CONTROL_CAPACITY);
        Self { sender }
    }

    /// Every open stream of `user_id` resolves its caller again. Infallible:
    /// with no stream open there is nobody to tell.
    pub fn revalidate(&self, user_id: &str) {
        let _ = self.sender.send(user_id.to_string());
    }

    fn subscribe(&self) -> broadcast::Receiver<UserId> {
        self.sender.subscribe()
    }
}

impl Default for StreamControl {
    fn default() -> Self {
        Self::new()
    }
}

/// One client's stream of board events.
///
/// Opened with every receiver already subscribed, so a client that has read
/// the opening burst is subscribed: nothing published after the burst can be
/// missed. [`next`](Self::next) yields the burst, then each change and tail
/// the caller may see, and `None` once the stream is over.
pub struct Subscription {
    caller: Caller,
    /// The caller's team ids as the stream opened: its scope for life. A
    /// revalidation that answers another set ends the stream rather than
    /// patching it.
    teams: BTreeSet<TeamId>,
    tails: Tails,
    changes: broadcast::Receiver<ChangeEvent>,
    live: broadcast::Receiver<TeamTail>,
    live_open: bool,
    control: broadcast::Receiver<UserId>,
    control_open: bool,
    revalidate: Option<Revalidate>,
    clock: Arc<dyn Clock>,
    next_revalidation: DateTime<Utc>,
    pending: VecDeque<WireEvent>,
    ended: bool,
}

/// What woke a stream.
enum Woke {
    Control(std::result::Result<UserId, RecvError>),
    Due,
    Change(std::result::Result<ChangeEvent, RecvError>),
    Tail(std::result::Result<TeamTail, RecvError>),
}

impl Subscription {
    /// Subscribes to the change channel, the live channel and the host's
    /// [`StreamControl`], **before** anything is yielded, and queues
    /// [`board_reread`] as the first five events.
    ///
    /// `revalidate` is `None` in solo, whose caller is the installation's own
    /// user and has no credential to lose; a server passes the route's
    /// closure.
    pub fn open(
        host: &BoardHost,
        caller: Caller,
        tails: Tails,
        revalidate: Option<Revalidate>,
    ) -> Self {
        let changes = host.context.subscribe();
        let live = host.context.subscribe_tail();
        let control = host.streams.subscribe();
        let clock = host.context.clock.clone();
        let next_revalidation = clock.now() + interval();
        Self {
            teams: team_ids(&caller),
            caller,
            tails,
            changes,
            live,
            live_open: true,
            control,
            control_open: true,
            revalidate,
            clock,
            next_revalidation,
            pending: board_reread().into(),
            ended: false,
        }
    }

    /// The next event this stream delivers, or `None` once it has ended: its
    /// caller no longer resolves to the teams it opened with, or the host is
    /// gone. A client reconnects and is either refused or re-scoped.
    pub async fn next(&mut self) -> Option<WireEvent> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }
            if self.ended {
                return None;
            }

            let revalidating = self.revalidate.is_some();
            let due: BoxFuture<'static, ()> = if revalidating {
                self.clock.sleep_until(self.next_revalidation)
            } else {
                Box::pin(std::future::pending())
            };
            // Biased, control first: a signal waiting beside an event is
            // honoured before the event is delivered, so a revoked caller
            // never hears what was published after the revocation.
            let woke = tokio::select! {
                biased;
                message = self.control.recv(), if revalidating && self.control_open => {
                    Woke::Control(message)
                }
                _ = due => Woke::Due,
                change = self.changes.recv() => Woke::Change(change),
                tail = self.live.recv(), if self.live_open => Woke::Tail(tail),
            };

            match woke {
                Woke::Control(Ok(user_id)) => {
                    if user_id == self.caller.user_id && !self.still_admitted().await {
                        self.ended = true;
                    }
                }
                // The lost message may have been this user's.
                Woke::Control(Err(RecvError::Lagged(_))) => {
                    if !self.still_admitted().await {
                        self.ended = true;
                    }
                }
                Woke::Control(Err(RecvError::Closed)) => self.control_open = false,
                Woke::Due => {
                    if !self.still_admitted().await {
                        self.ended = true;
                    }
                }
                Woke::Change(Ok(event)) => {
                    if self.admits_change(&event) {
                        return Some(board_wire(&event.change));
                    }
                }
                Woke::Change(Err(RecvError::Lagged(dropped))) => {
                    tracing::warn!(
                        dropped,
                        "a stream fell behind the change channel; it re-reads every board entity"
                    );
                    self.pending.extend(board_reread());
                }
                Woke::Change(Err(RecvError::Closed)) => self.ended = true,
                Woke::Tail(Ok(tail)) => {
                    if self.admits_tail(&tail) {
                        return Some(tail_wire(&tail.tail));
                    }
                }
                // D14 rule 1: a dropped snapshot is already on disk in the
                // transcript, and the next one replaces it.
                Woke::Tail(Err(RecvError::Lagged(dropped))) => {
                    tracing::debug!(dropped, "a stream fell behind the live channel");
                }
                Woke::Tail(Err(RecvError::Closed)) => self.live_open = false,
            }
        }
    }

    /// A change is for this stream when its team is one of the caller's, or
    /// it is the caller's own setting. Reads the event and the caller and
    /// nothing else, so a deleted row's event routes as a live one's does
    /// (ADR-0029 point 5).
    fn admits_change(&self, event: &ChangeEvent) -> bool {
        match &event.audience {
            Audience::Team(team_id) => self.teams.contains(team_id),
            Audience::User(user_id) => *user_id == self.caller.user_id,
        }
    }

    /// A tail is for this stream when its run's team is one of the caller's
    /// and it is a run the stream asked for.
    fn admits_tail(&self, tail: &TeamTail) -> bool {
        self.teams.contains(&tail.team_id) && self.tails.admits(&tail.tail.run_id)
    }

    /// Resolves the caller again, and whether it still reaches exactly the
    /// teams the stream opened with. A role change alone keeps the stream,
    /// because both roles read the same events (ADR-0029 point 3).
    async fn still_admitted(&mut self) -> bool {
        let Some(revalidate) = &self.revalidate else {
            return true;
        };
        let resolved = revalidate().await;
        self.next_revalidation = self.clock.now() + interval();
        match resolved {
            Ok(caller) if team_ids(&caller) == self.teams => true,
            Ok(_) => {
                tracing::info!("a stream's caller reaches other teams now; ending the stream");
                false
            }
            Err(error) => {
                tracing::info!(code = ?error.code(), "a stream's caller no longer resolves; ending the stream");
                false
            }
        }
    }
}

fn team_ids(caller: &Caller) -> BTreeSet<TeamId> {
    caller
        .teams
        .iter()
        .map(|grant| grant.team_id.clone())
        .collect()
}

/// [`STREAM_REVALIDATE_INTERVAL`] as the clock's kind of duration, for a test
/// that advances a fake clock past it.
pub fn interval() -> chrono::Duration {
    chrono::Duration::from_std(STREAM_REVALIDATE_INTERVAL)
        .expect("the revalidation interval fits a chrono duration")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn every_change_maps_to_the_name_the_frontend_listens_for() {
        let ids = |ids: &[&str]| -> Arc<[String]> { ids.iter().map(|id| id.to_string()).collect() };
        let board = [
            (Change::Tasks(ids(&["t"])), "tasks:changed", json!(["t"])),
            (
                Change::Repositories(ids(&["r"])),
                "repositories:changed",
                json!(["r"]),
            ),
            (Change::Runs(ids(&["u"])), "runs:changed", json!(["u"])),
            (Change::Settings, "settings:changed", Value::Null),
            (Change::Teams(ids(&["m"])), "teams:changed", json!(["m"])),
        ];
        for (change, name, data) in board {
            assert_eq!(board_wire(&change), WireEvent { name, data }, "{change:?}");
        }

        let local = [
            (LocalChange::Runner, "runner:changed", Value::Null),
            (
                LocalChange::Schedules(ids(&["s"])),
                "schedules:changed",
                json!(["s"]),
            ),
            (
                LocalChange::Checkouts(ids(&["r"])),
                "repositories:changed",
                json!(["r"]),
            ),
            (
                LocalChange::Worktrees(ids(&["t"])),
                "tasks:changed",
                json!(["t"]),
            ),
        ];
        for (change, name, data) in local {
            assert_eq!(local_wire(&change), WireEvent { name, data }, "{change:?}");
        }

        let tail = RunTail {
            run_id: "u".to_string(),
            elapsed_ms: 1500,
            turns: 2,
            current_tool: None,
            last_assistant_text: Some("Reading the plan.".to_string()),
        };
        assert_eq!(
            tail_wire(&tail),
            WireEvent {
                name: "runs:tail",
                data: json!({
                    "runId": "u",
                    "elapsedMs": 1500,
                    "turns": 2,
                    "currentTool": null,
                    "lastAssistantText": "Reading the plan.",
                }),
            }
        );

        let wire = |name, data| WireEvent { name, data };
        assert_eq!(
            board_reread(),
            vec![
                wire("tasks:changed", json!([])),
                wire("repositories:changed", json!([])),
                wire("runs:changed", json!([])),
                wire("settings:changed", Value::Null),
                wire("teams:changed", json!([])),
            ]
        );
        assert_eq!(
            local_reread(),
            vec![
                wire("runner:changed", Value::Null),
                wire("schedules:changed", json!([])),
                wire("repositories:changed", json!([])),
                wire("tasks:changed", json!([])),
            ]
        );
    }

    #[test]
    fn plan_progress_names_its_event() {
        let result = PlanResult {
            task_id: "t".to_string(),
            title: "A card".to_string(),
            outcome: PlanOutcome::Failed("no proposal".to_string()),
        };
        let progress = PlanProgress {
            index: 0,
            total: 3,
            spent_usd: 0.5,
            result: &result,
        };

        let wire = plan_progress_wire(&PlanProgressView::from(&progress));

        assert_eq!(wire.name, "plan-pass:progress");
        assert_eq!(wire.data["completed"], json!(1));
    }

    #[test]
    fn a_malformed_or_ninth_tail_id_is_invalid() {
        let run = || uuid::Uuid::new_v4().to_string();

        let eight: Vec<String> = (0..MAX_TAILS_PER_STREAM).map(|_| run()).collect();
        assert!(
            matches!(Tails::requested(eight.clone()), Ok(Tails::Only(runs)) if runs.len() == 8)
        );

        let nine = eight.into_iter().chain([run()]);
        assert_eq!(
            Tails::requested(nine).expect_err("a ninth id").code(),
            crate::ErrorCode::Invalid
        );
        assert_eq!(
            Tails::requested(["not-a-run".to_string()])
                .expect_err("a malformed id")
                .code(),
            crate::ErrorCode::Invalid
        );
    }
}
