//! What a service tells the rest of the app it changed (ADR-0018).
//!
//! The board is not the only writer. A task is created by a Tauri command, by an
//! MCP tool call from another Claude Code session (ADR-0006), by a run writing
//! back to its own task, or by the scheduler moving it through its run states.
//! Whichever door a mutation comes through, every open view has to see it — so a
//! mutation publishes a [`ChangeEvent`] on the broadcast channel
//! [`ServiceContext`](crate::ServiceContext) carries, the shell subscribes once
//! and re-emits into Tauri, and the MCP server (task 010) subscribes to the same
//! sender it publishes to rather than sitting downstream of the UI.
//!
//! **Ids, never rows.** A payload would be a second source of truth: a client
//! handed a row has to decide whether it is newer than the row it already holds,
//! and it is wrong the moment the next writer commits. An id says "re-read
//! this", which is always safe, and lets the UI and the MCP server project the
//! same change differently.
//!
//! ADR-0013's live run tail does not ride this channel. [`Runs`](Change::Runs)
//! means "a run row changed", once per state transition — not once per token.

use std::collections::BTreeMap;
use std::sync::Arc;

/// The id of a row in `tasks`.
///
/// ADR-0018's snippet writes `Tasks(Arc<[TaskId]>)` and leaves the type open;
/// that snippet is illustrative and **seam-contract D10 governs** — every id is
/// a plain `String` holding a hyphenated UUID, with no newtype wrapper. These
/// are transparent aliases, so they document which table an id came from and
/// cost nothing at a call site or in a `query_as!` type override.
pub type TaskId = String;

/// The id of a row in `repositories`. See [`TaskId`].
pub type RepositoryId = String;

/// The id of a row in `runs`. See [`TaskId`].
pub type RunId = String;

/// The id of a row in `schedules`. See [`TaskId`].
pub type ScheduleId = String;

/// The id of a row in `teams` (ADR-0029). See [`TaskId`].
pub type TeamId = String;

/// The id of a row in `users` (ADR-0030). See [`TaskId`].
pub type UserId = String;

/// The id of a row in `runners` (ADR-0031). See [`TaskId`].
pub type RunnerId = String;

/// How many unread events the channel keeps for a receiver that falls behind.
///
/// One desktop user's mutations pass through here, so 256 is ample; it lives in
/// core rather than in the shell because the MCP server and the scheduler
/// subscribe to the same sender and none of them is entitled to a different
/// answer. Lag is a correctness-neutral hiccup rather than a tuning emergency:
/// because events carry ids, a dropped one costs a stale card until the next
/// event, never a wrong one, and the shell's forwarder turns a
/// `RecvError::Lagged` into one wholesale re-read.
pub const CHANGE_BUFFER_CAPACITY: usize = 256;

/// Something in the store changed: which team it belongs to, and which ids.
///
/// The team rides on the event rather than being looked up by whoever
/// receives it (ADR-0034 point 3, amending ADR-0018): a task id arriving on
/// another team's stream still says the task exists and when it changed, so
/// the server's fan-out (task 048) filters per subscriber on this field. It
/// needs no lookup that way, and still works for a row that has just been
/// deleted.
///
/// **An event never names an id outside its `team_id`.** A publish whose rows
/// can span teams goes through [`per_team`], one event per team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvent {
    /// The team of the rows named, read from the written row in the same
    /// transaction — never assumed from the context's scope, which may name
    /// several teams.
    pub team_id: TeamId,
    pub change: Change,
}

/// What changed. Carries which ids, and nothing else.
///
/// `Arc<[_]>` rather than `Vec<_>` because broadcast clones the value once per
/// receiver: a fan-out across the board, the MCP server and the scheduler should
/// be three refcount bumps, not three allocations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Tasks(Arc<[TaskId]>),
    Repositories(Arc<[RepositoryId]>),
    Runs(Arc<[RunId]>),
    /// A row in `schedules` changed (task 013).
    ///
    /// **A variant of its own rather than a reuse of [`Settings`](Self::Settings),
    /// and the distinction is the one this enum is built on.** `settings` is a
    /// key/value table whose every consumer re-reads all of it, which is why
    /// that variant carries no ids. `schedules` is a table of *entities* the
    /// user creates, names, edits and deletes — the same kind of thing `tasks`
    /// and `repositories` are — so it carries ids for the same reason they do,
    /// and a panel listing thirty schedules is not obliged to re-read them
    /// because a base-instructions textarea was saved.
    ///
    /// The window a schedule opens *is* a settings key and does announce itself
    /// as [`Settings`](Self::Settings). That is not an inconsistency: the window
    /// is one singleton fact about the installation, and the Runs view reading
    /// it re-reads the whole queue status anyway. Seam-contract D24.
    Schedules(Arc<[ScheduleId]>),
    /// A key/value setting changed. Unit rather than a list of keys: the whole
    /// table is a handful of rows, and every consumer re-reads all of it.
    Settings,
}

impl ChangeEvent {
    pub fn tasks(team_id: TeamId, ids: impl IntoIterator<Item = TaskId>) -> Self {
        Self::new(team_id, Change::Tasks(ids.into_iter().collect()))
    }

    pub fn repositories(team_id: TeamId, ids: impl IntoIterator<Item = RepositoryId>) -> Self {
        Self::new(team_id, Change::Repositories(ids.into_iter().collect()))
    }

    pub fn runs(team_id: TeamId, ids: impl IntoIterator<Item = RunId>) -> Self {
        Self::new(team_id, Change::Runs(ids.into_iter().collect()))
    }

    pub fn schedules(team_id: TeamId, ids: impl IntoIterator<Item = ScheduleId>) -> Self {
        Self::new(team_id, Change::Schedules(ids.into_iter().collect()))
    }

    pub fn settings(team_id: TeamId) -> Self {
        Self::new(team_id, Change::Settings)
    }

    fn new(team_id: TeamId, change: Change) -> Self {
        Self { team_id, change }
    }

    /// Whether this event names no ids at all. See [`Change::is_empty`].
    pub fn is_empty(&self) -> bool {
        self.change.is_empty()
    }
}

impl Change {
    /// Whether this change names no ids at all.
    ///
    /// An empty array on the wire means "re-read this entity wholesale", and the
    /// shell's forwarder is the only thing entitled to send one — it does,
    /// exactly once, after a lagged receiver reports how much it missed. So a
    /// service must never publish one, and
    /// [`ServiceContext::publish`](crate::ServiceContext::publish) drops it if it
    /// tries: a mutation that touched nothing would otherwise order a full
    /// refresh.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Tasks(ids) => ids.is_empty(),
            Self::Repositories(ids) => ids.is_empty(),
            Self::Runs(ids) => ids.is_empty(),
            Self::Schedules(ids) => ids.is_empty(),
            Self::Settings => false,
        }
    }
}

/// One event per team, from the `(team_id, id)` pairs a publish gathered.
///
/// The one place a publish whose rows can span teams splits them, so no
/// service writes its own loop: the query selects `team_id` beside each id,
/// and this keeps ADR-0034 point 3's rule that an event names only its own
/// team's rows. Ids keep the order they arrived in within their team, and a
/// team with no ids gets no event, because there is no pair to name it.
/// `build` is the constructor of the variant being published, such as
/// `ChangeEvent::tasks`.
pub fn per_team<Id>(
    pairs: impl IntoIterator<Item = (TeamId, Id)>,
    build: impl Fn(TeamId, Vec<Id>) -> ChangeEvent,
) -> Vec<ChangeEvent> {
    let mut grouped: BTreeMap<TeamId, Vec<Id>> = BTreeMap::new();
    for (team_id, id) in pairs {
        grouped.entry(team_id).or_default().push(id);
    }
    grouped
        .into_iter()
        .map(|(team_id, ids)| build(team_id, ids))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const TEAM: &str = "3f2b1c00-0000-4000-8000-0000000000a1";
    const OTHER_TEAM: &str = "3f2b1c00-0000-4000-8000-0000000000b2";

    #[test]
    fn an_event_built_from_ids_keeps_them_in_the_order_given() {
        let event = ChangeEvent::tasks(
            TEAM.to_string(),
            ["second".to_string(), "first".to_string()],
        );

        assert_eq!(
            event,
            ChangeEvent {
                team_id: TEAM.to_string(),
                change: Change::Tasks(Arc::from(["second".to_string(), "first".to_string()])),
            }
        );
    }

    #[test]
    fn an_event_naming_no_ids_reports_itself_empty() {
        assert!(ChangeEvent::tasks(TEAM.to_string(), []).is_empty());
        assert!(ChangeEvent::repositories(TEAM.to_string(), []).is_empty());
        assert!(ChangeEvent::runs(TEAM.to_string(), []).is_empty());
        assert!(ChangeEvent::schedules(TEAM.to_string(), []).is_empty());
    }

    #[test]
    fn the_settings_variant_is_never_empty() {
        // It carries no ids by design, which is not the same as naming none —
        // suppressing it would mean a settings change nobody ever hears about.
        assert!(!ChangeEvent::settings(TEAM.to_string()).is_empty());
    }

    #[test]
    fn ids_from_two_teams_become_one_event_per_team() {
        let pairs = [
            (OTHER_TEAM.to_string(), "b-first".to_string()),
            (TEAM.to_string(), "a-first".to_string()),
            (OTHER_TEAM.to_string(), "b-second".to_string()),
            (TEAM.to_string(), "a-second".to_string()),
        ];

        let events = per_team(pairs, ChangeEvent::tasks);

        assert_eq!(
            events,
            vec![
                ChangeEvent::tasks(
                    TEAM.to_string(),
                    ["a-first".to_string(), "a-second".to_string()]
                ),
                ChangeEvent::tasks(
                    OTHER_TEAM.to_string(),
                    ["b-first".to_string(), "b-second".to_string()]
                ),
            ]
        );
    }

    #[test]
    fn no_pairs_become_no_events() {
        // A team with no ids gets no event: there is nothing for it to name,
        // and an empty list on the wire would order a wholesale re-read.
        let events = per_team(Vec::<(TeamId, TaskId)>::new(), ChangeEvent::tasks);

        assert_eq!(events, Vec::<ChangeEvent>::new());
    }
}
