//! What a service tells the rest of the app it changed (ADR-0018).
//!
//! The board is not the only writer. A task is created by a Tauri command, by an
//! MCP tool call from another Claude Code session (ADR-0006), by a run writing
//! back to its own task, or by the scheduler moving it through its run states.
//! Whichever door a mutation comes through, every open view has to see it — so a
//! mutation publishes a [`ChangeEvent`] on the broadcast channel
//! [`ServiceContext`](crate::ServiceContext) carries, the shell and the server
//! each follow it through one `api::events::Subscription` per client, and the
//! MCP server (task 010) subscribes to the same
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
//!
//! **Board events and local events** (task 048). A [`ChangeEvent`] is about
//! board state, which a server serves to every member of a team. A
//! [`LocalChange`] is about this machine — its runner keys, schedules,
//! checkouts and worktree records — rides [`LocalEvents`] on the
//! `MachineContext`, and never reaches a server.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::runner::events::RunTail;

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
/// event, never a wrong one, and every subscription
/// (`api::events::Subscription`) turns a `RecvError::Lagged` into one wholesale
/// re-read.
pub const CHANGE_BUFFER_CAPACITY: usize = 256;

/// Who may hear about a change (ADR-0034 point 3, refined by task 048).
///
/// Almost every row belongs to a team, and its event is for that team's
/// members. One board state does not: `user_settings` belongs to a person in
/// every team they are in (seam-contract D28 part 4). Routing it by one of
/// their teams would either tell that team's other members about a write that
/// is not theirs, or miss the person's other teams, so it has an audience of
/// its own. Routing metadata, never row data: no wire payload carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Audience {
    /// Every member of the team.
    Team(TeamId),
    /// The one user, whichever teams they are in.
    User(UserId),
}

/// Something in the store changed: who may hear about it, and which ids.
///
/// The audience rides on the event rather than being looked up by whoever
/// receives it (ADR-0034 point 3, amending ADR-0018): a task id arriving on
/// another team's stream still says the task exists and when it changed, so
/// the server's fan-out (`api::events::Subscription`) filters per subscriber
/// on this field. It needs no lookup that way, and still works for a row that
/// has just been deleted.
///
/// **An event never names an id outside its team.** A publish whose rows can
/// span teams goes through [`per_team`], one event per team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvent {
    /// For a team's rows, the team, read from the written row in the same
    /// transaction — never assumed from the context's scope, which may name
    /// several teams. For a person's own settings, the person.
    pub audience: Audience,
    pub change: Change,
}

/// What changed. Carries which ids, and nothing else.
///
/// `Arc<[_]>` rather than `Vec<_>` because broadcast clones the value once per
/// receiver: a fan-out across the board, the MCP server and the scheduler should
/// be three refcount bumps, not three allocations.
///
/// Board state only. What changed on this machine — a runner key, a schedule,
/// a checkout, a worktree record — is a [`LocalChange`] on [`LocalEvents`],
/// because a server has no machine to announce (task 048).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Tasks(Arc<[TaskId]>),
    Repositories(Arc<[RepositoryId]>),
    Runs(Arc<[RunId]>),
    /// A team setting or a user setting changed. Unit rather than a list of
    /// keys: each table is a handful of rows, and every consumer re-reads all
    /// of it.
    Settings,
    /// A team itself changed (task 051): it was created or deleted, someone
    /// joined, left or was removed, a role changed, or an invitation was
    /// accepted or revoked. The switcher, the member list and the invitation
    /// list re-read on it.
    Teams(Arc<[TeamId]>),
}

impl ChangeEvent {
    pub fn tasks(team_id: TeamId, ids: impl IntoIterator<Item = TaskId>) -> Self {
        Self::for_team(team_id, Change::Tasks(ids.into_iter().collect()))
    }

    pub fn repositories(team_id: TeamId, ids: impl IntoIterator<Item = RepositoryId>) -> Self {
        Self::for_team(team_id, Change::Repositories(ids.into_iter().collect()))
    }

    pub fn runs(team_id: TeamId, ids: impl IntoIterator<Item = RunId>) -> Self {
        Self::for_team(team_id, Change::Runs(ids.into_iter().collect()))
    }

    /// A team setting changed.
    pub fn settings(team_id: TeamId) -> Self {
        Self::for_team(team_id, Change::Settings)
    }

    /// Something about `team_id` itself changed (task 051), for its members.
    pub fn teams(team_id: TeamId) -> Self {
        Self::for_team(team_id.clone(), Change::Teams(Arc::from([team_id])))
    }

    /// One user's own setting changed (`user_settings`): heard by that user's
    /// streams, in whichever teams they are, and by nobody else.
    pub fn user_settings(user_id: UserId) -> Self {
        Self {
            audience: Audience::User(user_id),
            change: Change::Settings,
        }
    }

    fn for_team(team_id: TeamId, change: Change) -> Self {
        Self {
            audience: Audience::Team(team_id),
            change,
        }
    }

    /// Whether this event names no ids at all. See [`Change::is_empty`].
    pub fn is_empty(&self) -> bool {
        self.change.is_empty()
    }
}

impl Change {
    /// Whether this change names no ids at all.
    ///
    /// An empty array on the wire means "re-read this entity wholesale", and
    /// only a subscriber's own recovery is entitled to send one: the opening
    /// burst of a stream, and the burst after a lagged receiver reports how
    /// much it missed (`api::events::board_reread`). So a service must never
    /// publish one, and [`ServiceContext::publish`](crate::ServiceContext::publish)
    /// drops it if it tries: a mutation that touched nothing would otherwise
    /// order a full refresh.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Tasks(ids) => ids.is_empty(),
            Self::Repositories(ids) => ids.is_empty(),
            Self::Runs(ids) => ids.is_empty(),
            Self::Settings => false,
            Self::Teams(ids) => ids.is_empty(),
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

/// Something about this machine changed. Never sent to a server, and never
/// filtered by team, because a machine has no team (task 048).
///
/// The board's [`ChangeEvent`]s are what a server's stream carries. These are
/// the runner's own rows (ADR-0031 point 6), which only the desktop shell
/// forwards, so no arm of the board's wire mapping can carry one onto a
/// stream: the two are separate types on separate channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalChange {
    /// A key in D28 part 4's *Runner* row: the queue switch, capacity, the run
    /// window, the usage-limit pause, the MCP port and the rest. Unit, for
    /// [`Change::Settings`]'s reason. Also announced when the loop records a
    /// refused scheduled start, so the Runs view re-reads it.
    Runner,
    /// Rows in `runner.db`'s `schedules`.
    Schedules(Arc<[ScheduleId]>),
    /// This machine's checkouts of these repositories: path, limit,
    /// on-archive, credentials.
    Checkouts(Arc<[RepositoryId]>),
    /// This machine's worktree records of these tasks.
    Worktrees(Arc<[TaskId]>),
}

impl LocalChange {
    /// Whether this change names no ids at all, which [`LocalEvents::publish`]
    /// never sends, for [`Change::is_empty`]'s reason.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Runner => false,
            Self::Schedules(ids) => ids.is_empty(),
            Self::Checkouts(ids) => ids.is_empty(),
            Self::Worktrees(ids) => ids.is_empty(),
        }
    }
}

/// How many unread local events the channel keeps for a receiver that falls
/// behind. One machine's own writes, so [`CHANGE_BUFFER_CAPACITY`]'s
/// reasoning holds unchanged.
pub const LOCAL_EVENT_CAPACITY: usize = 256;

/// The channel a machine's writes are announced on: what
/// `MachineContext.changes` was until task 048, retyped.
///
/// Cheap to clone, and every clone publishes to the same subscribers, for
/// [`ServiceContext`](crate::ServiceContext)'s reason. Built once per host:
/// the solo shell's `setup()`, and later the headless runner.
#[derive(Clone)]
pub struct LocalEvents {
    sender: broadcast::Sender<LocalChange>,
}

impl LocalEvents {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(LOCAL_EVENT_CAPACITY);
        Self { sender }
    }

    /// Announces a machine write. **Call it after the write**, and it ignores
    /// whether anyone heard: ADR-0018's two rules, exactly as
    /// [`ServiceContext::publish`](crate::ServiceContext::publish) enforces
    /// them. A change naming no ids is dropped.
    pub fn publish(&self, change: LocalChange) {
        if change.is_empty() {
            return;
        }
        let _ = self.sender.send(change);
    }

    /// A receiver for every local change published from here on.
    pub fn subscribe(&self) -> broadcast::Receiver<LocalChange> {
        self.sender.subscribe()
    }

    /// How many receivers are subscribed, for a `Debug` line.
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for LocalEvents {
    fn default() -> Self {
        Self::new()
    }
}

/// A live run snapshot and the team of its run (seam-contract D14, amended by
/// task 048): what the live channel carries, so a stream admits a tail without
/// a lookup, as it admits a change event.
///
/// The team is routing metadata, as an [`Audience`] is: the wire payload is
/// the [`RunTail`] alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamTail {
    pub team_id: TeamId,
    pub tail: RunTail,
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
                audience: Audience::Team(TEAM.to_string()),
                change: Change::Tasks(Arc::from(["second".to_string(), "first".to_string()])),
            }
        );
    }

    #[test]
    fn an_event_naming_no_ids_reports_itself_empty() {
        assert!(ChangeEvent::tasks(TEAM.to_string(), []).is_empty());
        assert!(ChangeEvent::repositories(TEAM.to_string(), []).is_empty());
        assert!(ChangeEvent::runs(TEAM.to_string(), []).is_empty());
    }

    #[test]
    fn the_settings_variant_is_never_empty() {
        // It carries no ids by design, which is not the same as naming none —
        // suppressing it would mean a settings change nobody ever hears about.
        assert!(!ChangeEvent::settings(TEAM.to_string()).is_empty());
        assert!(!ChangeEvent::user_settings("a-user".to_string()).is_empty());
    }

    #[test]
    fn a_team_change_is_for_that_team_and_names_it() {
        assert_eq!(
            ChangeEvent::teams(TEAM.to_string()),
            ChangeEvent {
                audience: Audience::Team(TEAM.to_string()),
                change: Change::Teams(Arc::from([TEAM.to_string()])),
            }
        );
        assert!(!ChangeEvent::teams(TEAM.to_string()).is_empty());
    }

    #[test]
    fn a_user_setting_is_for_its_user_and_no_team() {
        assert_eq!(
            ChangeEvent::user_settings("a-user".to_string()),
            ChangeEvent {
                audience: Audience::User("a-user".to_string()),
                change: Change::Settings,
            }
        );
    }

    #[test]
    fn a_local_change_naming_no_ids_is_never_published() {
        let local = LocalEvents::new();
        let mut changes = local.subscribe();

        local.publish(LocalChange::Schedules(Arc::from([])));
        local.publish(LocalChange::Checkouts(Arc::from([])));
        local.publish(LocalChange::Worktrees(Arc::from([])));
        local.publish(LocalChange::Runner);

        assert_eq!(
            changes.try_recv().expect("the runner change"),
            LocalChange::Runner
        );
        assert!(changes.try_recv().is_err(), "nothing else was sent");
    }

    #[test]
    fn publishing_a_local_change_with_nobody_listening_is_not_an_error() {
        LocalEvents::new().publish(LocalChange::Runner);
    }

    #[test]
    fn a_clone_of_the_local_channel_publishes_to_the_original_subscribers() {
        let local = LocalEvents::new();
        let mut changes = local.subscribe();

        local
            .clone()
            .publish(LocalChange::Schedules(Arc::from(["nightly".to_string()])));

        assert_eq!(
            changes.try_recv().expect("the clone's publication"),
            LocalChange::Schedules(Arc::from(["nightly".to_string()]))
        );
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
