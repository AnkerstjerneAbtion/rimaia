//! One subscriber's stream, without HTTP (task 048, ADR-0034 point 3,
//! ADR-0029 point 5, ADR-0030 point 2).
//!
//! The server's `GET /api/v1/events` adds framing to [`Subscription`] and
//! nothing else, so the rules are held here, over the type itself: admission
//! by the caller's teams, lag recovery, and revalidation. `crates/server/
//! tests/events.rs` holds the same rules over a loopback listener.
//!
//! **Absence is proven by ordering, never by a timeout.** Every "must not
//! arrive" is followed by a sentinel published on the same channel after the
//! forbidden event: the stream reads its channel in order, so receiving the
//! sentinel means the forbidden event was read and dropped. No test sleeps.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Duration;
use pretty_assertions::assert_eq;
use rimaia_core::api::events::{
    board_reread, board_wire, tail_wire, Revalidate, STREAM_CONTROL_CAPACITY,
    STREAM_REVALIDATE_INTERVAL,
};
use rimaia_core::api::{BoardHost, Caller, Subscription, Tails, TeamGrant, WireEvent};
use rimaia_core::events::CHANGE_BUFFER_CAPACITY;
use rimaia_core::identity::Role;
use rimaia_core::runner::events::{RunTail, TAIL_CHANNEL_CAPACITY};
use rimaia_core::tasks;
use rimaia_core::testing::api::{self as testing_api, as_desktop};
use rimaia_core::testing::teams::TwoTeams;
use rimaia_core::{ChangeEvent, Error};

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_subscriber_receives_only_its_teams_change_events() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let a = as_desktop(&teams.team_a);
    let b = as_desktop(&teams.team_b);
    let both = in_both(&teams);
    let mut streams = [
        opened(&host, a.clone(), Tails::All).await,
        opened(&host, b.clone(), Tails::All).await,
        opened(&host, both.clone(), Tails::All).await,
    ];

    let team_a = teams.team_a.team_id.clone();
    let team_b = teams.team_b.team_id.clone();
    let published = [
        ChangeEvent::tasks(team_a.clone(), [teams.team_a.ready.clone()]),
        ChangeEvent::repositories(team_a.clone(), [teams.team_a.repository.id.clone()]),
        ChangeEvent::runs(team_a.clone(), [teams.team_a.implementation_run.clone()]),
        ChangeEvent::settings(team_a.clone()),
        ChangeEvent::tasks(team_b.clone(), [teams.team_b.ready.clone()]),
        ChangeEvent::repositories(team_b.clone(), [teams.team_b.repository.id.clone()]),
        ChangeEvent::runs(team_b.clone(), [teams.team_b.implementation_run.clone()]),
        ChangeEvent::settings(team_b.clone()),
    ];
    for event in &published {
        host.context.publish(event.clone());
    }
    // B's owner's own setting: for that user alone.
    let b_setting = ChangeEvent::user_settings(b.user_id.clone());
    host.context.publish(b_setting.clone());
    // A sentinel per team, after everything a stream must not receive.
    let sentinel_a = ChangeEvent::tasks(team_a, ["sentinel-a".to_string()]);
    let sentinel_b = ChangeEvent::tasks(team_b, ["sentinel-b".to_string()]);
    host.context.publish(sentinel_a.clone());
    host.context.publish(sentinel_b.clone());

    let wire = |events: &[ChangeEvent]| -> Vec<WireEvent> {
        events
            .iter()
            .map(|event| board_wire(&event.change))
            .collect()
    };
    let [in_a, in_b, in_both] = &mut streams;
    assert_eq!(
        until(in_a, &board_wire(&sentinel_a.change)).await,
        wire(&[&published[..4], std::slice::from_ref(&sentinel_a)].concat())
    );
    assert_eq!(
        until(in_b, &board_wire(&sentinel_b.change)).await,
        wire(&[&published[4..], &[b_setting, sentinel_b.clone()]].concat())
    );
    assert_eq!(
        until(in_both, &board_wire(&sentinel_b.change)).await,
        wire(&[&published[..], &[sentinel_a, sentinel_b]].concat())
    );
}

#[tokio::test]
async fn a_deleted_tasks_event_reaches_its_team_without_a_lookup() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let mut in_a = opened(&host, as_desktop(&teams.team_a), Tails::All).await;
    let mut in_b = opened(&host, as_desktop(&teams.team_b), Tails::All).await;

    // The service publishes after its commit, so the row is gone before the
    // event exists: admission reads the event, never the row.
    tasks::delete_task(&teams.a, &teams.team_a.not_ready)
        .await
        .expect("delete team A's task");
    let gone = ChangeEvent::tasks(
        teams.team_a.team_id.clone(),
        [teams.team_a.not_ready.clone()],
    );
    let sentinel = ChangeEvent::settings(teams.team_b.team_id.clone());
    host.context.publish(sentinel.clone());

    let deleted = board_wire(&gone.change);
    assert_eq!(until(&mut in_a, &deleted).await, vec![deleted.clone()]);
    assert_eq!(
        until(&mut in_b, &board_wire(&sentinel.change)).await,
        vec![board_wire(&sentinel.change)],
        "team B's stream skipped team A's deletion"
    );
}

#[tokio::test]
async fn a_live_event_outside_the_callers_teams_is_never_admitted() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let own = teams.team_a.implementation_run.clone();
    let foreign = teams.team_b.implementation_run.clone();
    let mut in_a = opened(
        &host,
        as_desktop(&teams.team_a),
        Tails::requested([own.clone(), foreign.clone()]).expect("two run ids"),
    )
    .await;

    // Team B's run, which the stream asked for by id; a run the stream asked
    // for whose event names team B, which only a forged or confused publisher
    // could send; and a run of team A's it did not ask for.
    let team_a = teams.team_a.team_id.clone();
    let team_b = teams.team_b.team_id.clone();
    host.context.publish_tail(team_b.clone(), tail(&foreign, 1));
    host.context.publish_tail(team_b, tail(&own, 2));
    host.context
        .publish_tail(team_a.clone(), tail(&teams.team_a.review_run, 3));
    host.context.publish_tail(team_a, tail(&own, 4));

    assert_eq!(
        in_a.next().await,
        Some(tail_wire(&tail(&own, 4))),
        "the one tail of team A's that the stream asked for, and nothing before it"
    );
}

// ---------------------------------------------------------------------------
// Lag
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_lagged_stream_rereads_each_board_entity_once_and_carries_on() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let mut in_a = opened(&host, as_desktop(&teams.team_a), Tails::All).await;
    let team_a = teams.team_a.team_id.clone();

    // One more than the channel holds, so the oldest is gone before the
    // stream reads.
    for sequence in 0..=CHANGE_BUFFER_CAPACITY {
        host.context
            .publish(ChangeEvent::tasks(team_a.clone(), [sequence.to_string()]));
    }

    let mut read = Vec::new();
    for _ in 0..board_reread().len() + 1 {
        read.push(in_a.next().await.expect("the stream carries on"));
    }
    assert_eq!(
        read,
        [
            board_reread(),
            vec![board_wire(
                &ChangeEvent::tasks(team_a, ["1".to_string()]).change
            )]
        ]
        .concat(),
        "one re-read, then the oldest event still buffered"
    );
}

#[tokio::test]
async fn a_lagged_tail_is_dropped_and_the_stream_carries_on() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let mut in_a = opened(&host, as_desktop(&teams.team_a), Tails::All).await;
    let team_a = teams.team_a.team_id.clone();
    let run = teams.team_a.implementation_run.clone();

    for elapsed in 0..=TAIL_CHANNEL_CAPACITY {
        host.context
            .publish_tail(team_a.clone(), tail(&run, elapsed as i64));
    }

    // No re-read: a dropped snapshot is already on disk in the transcript.
    assert_eq!(in_a.next().await, Some(tail_wire(&tail(&run, 1))));
    let sentinel = ChangeEvent::settings(team_a);
    host.context.publish(sentinel.clone());
    assert_eq!(in_a.next().await, Some(board_wire(&sentinel.change)));
}

// ---------------------------------------------------------------------------
// Revalidation, with the fixture's `TestClock`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stream_ends_when_the_callers_teams_change() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let caller = as_desktop(&teams.team_a);
    let script = Script::answering(Some(in_both(&teams)));
    let mut stream = revalidated(&host, caller.clone(), &script).await;

    host.streams.revalidate(&caller.user_id);
    host.context
        .publish(ChangeEvent::settings(teams.team_a.team_id.clone()));

    assert_eq!(
        stream.next().await,
        None,
        "the stream ends before the event"
    );
    assert_eq!(script.calls(), 1);
}

#[tokio::test]
async fn a_stream_ends_when_its_caller_no_longer_resolves() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let caller = as_desktop(&teams.team_a);
    let script = Script::answering(None);
    let mut stream = revalidated(&host, caller.clone(), &script).await;

    host.streams.revalidate(&caller.user_id);
    host.context
        .publish(ChangeEvent::settings(teams.team_a.team_id.clone()));

    assert_eq!(stream.next().await, None);
}

#[tokio::test]
async fn a_stream_survives_revalidation_to_the_same_teams_whatever_the_role() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let caller = as_desktop(&teams.team_a);
    let demoted = Caller {
        teams: vec![TeamGrant {
            team_id: teams.team_a.team_id.clone(),
            role: Role::Member,
        }],
        ..caller.clone()
    };
    let script = Script::answering(Some(demoted));
    let mut stream = revalidated(&host, caller.clone(), &script).await;

    host.streams.revalidate(&caller.user_id);
    let sentinel = ChangeEvent::settings(teams.team_a.team_id.clone());
    host.context.publish(sentinel.clone());

    assert_eq!(stream.next().await, Some(board_wire(&sentinel.change)));
    assert_eq!(script.calls(), 1, "revalidated before the event, once");
}

#[tokio::test]
async fn a_signal_for_another_user_does_not_revalidate() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let script = Script::answering(Some(as_desktop(&teams.team_a)));
    let mut stream = revalidated(&host, as_desktop(&teams.team_a), &script).await;

    host.streams.revalidate(&teams.team_b.owner_id);
    let sentinel = ChangeEvent::settings(teams.team_a.team_id.clone());
    host.context.publish(sentinel.clone());

    assert_eq!(stream.next().await, Some(board_wire(&sentinel.change)));
    assert_eq!(script.calls(), 0);
}

#[tokio::test]
async fn a_lagged_control_receiver_revalidates_at_once() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let script = Script::answering(Some(as_desktop(&teams.team_a)));
    let mut stream = revalidated(&host, as_desktop(&teams.team_a), &script).await;

    // Signals for somebody else, one more than the control channel holds:
    // the lost one may have been this user's, so the stream cannot know.
    for _ in 0..=STREAM_CONTROL_CAPACITY {
        host.streams.revalidate(&teams.team_b.owner_id);
    }
    let sentinel = ChangeEvent::settings(teams.team_a.team_id.clone());
    host.context.publish(sentinel.clone());

    assert_eq!(stream.next().await, Some(board_wire(&sentinel.change)));
    assert_eq!(script.calls(), 1);
}

#[tokio::test]
async fn a_stream_is_not_revalidated_before_the_interval() {
    let teams = TwoTeams::new().await;
    let host = testing_api::host(&teams).await;
    let script = Script::answering(Some(as_desktop(&teams.team_a)));
    let mut stream = revalidated(&host, as_desktop(&teams.team_a), &script).await;
    let interval =
        Duration::from_std(STREAM_REVALIDATE_INTERVAL).expect("the interval fits a duration");
    let team_a = teams.team_a.team_id.clone();

    teams.clock.advance(interval - Duration::seconds(1));
    let first = ChangeEvent::tasks(team_a.clone(), ["first".to_string()]);
    host.context.publish(first.clone());
    assert_eq!(stream.next().await, Some(board_wire(&first.change)));
    assert_eq!(script.calls(), 0, "one second short of the interval");

    teams.clock.advance(Duration::seconds(2));
    let second = ChangeEvent::tasks(team_a, ["second".to_string()]);
    host.context.publish(second.clone());
    assert_eq!(stream.next().await, Some(board_wire(&second.change)));
    assert_eq!(script.calls(), 1, "past the interval, once");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A user in both fixture teams, as an edge would resolve one: its own id,
/// which no fixture row holds, so only team events can reach it, and the
/// user settings this test publishes for it.
fn in_both(teams: &TwoTeams) -> Caller {
    Caller {
        user_id: "3f2b1c00-0000-4000-8000-0000000000e5".to_string(),
        teams: [&teams.team_a, &teams.team_b]
            .into_iter()
            .map(|team| TeamGrant {
                team_id: team.team_id.clone(),
                role: Role::Member,
            })
            .collect(),
        ..as_desktop(&teams.team_a)
    }
}

/// A stream with no revalidation, its opening burst already read: from here
/// on, everything it yields was published after it subscribed.
async fn opened(host: &BoardHost, caller: Caller, tails: Tails) -> Subscription {
    let mut stream = Subscription::open(host, caller, tails, None);
    read_burst(&mut stream).await;
    stream
}

/// A stream revalidating through `script`, its burst already read.
async fn revalidated(host: &BoardHost, caller: Caller, script: &Script) -> Subscription {
    let mut stream = Subscription::open(host, caller, Tails::All, Some(script.closure()));
    read_burst(&mut stream).await;
    stream
}

async fn read_burst(stream: &mut Subscription) {
    for expected in board_reread() {
        assert_eq!(stream.next().await, Some(expected), "the opening burst");
    }
}

/// Everything `stream` yields up to and including `last`.
async fn until(stream: &mut Subscription, last: &WireEvent) -> Vec<WireEvent> {
    let mut read = Vec::new();
    loop {
        let event = stream.next().await.expect("the stream stays open");
        let done = &event == last;
        read.push(event);
        if done {
            return read;
        }
    }
}

fn tail(run_id: &str, elapsed_ms: i64) -> RunTail {
    RunTail {
        run_id: run_id.to_string(),
        elapsed_ms,
        turns: 1,
        current_tool: None,
        last_assistant_text: None,
    }
}

/// A revalidation closure that answers one scripted caller, or refuses as
/// `unauthenticated` for `None`, and counts its calls.
struct Script {
    answer: Arc<Mutex<Option<Caller>>>,
    calls: Arc<AtomicUsize>,
}

impl Script {
    fn answering(answer: Option<Caller>) -> Self {
        Self {
            answer: Arc::new(Mutex::new(answer)),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn closure(&self) -> Revalidate {
        let answer = Arc::clone(&self.answer);
        let calls = Arc::clone(&self.calls);
        Box::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            let answer = answer
                .lock()
                .expect("the script")
                .clone()
                .ok_or_else(Error::unauthenticated);
            Box::pin(async move { answer })
        })
    }
}
