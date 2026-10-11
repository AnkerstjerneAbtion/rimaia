//! `GET /api/v1/events` over a loopback listener (task 048, ADR-0034 point 3,
//! ADR-0029 point 5, ADR-0030 point 2).
//!
//! **The reason this file exists: two teams share one server, and neither
//! stream ever carries the other team's ids.** The fixture is
//! `testing::teams::TwoTeams`, each team with a repository, tasks and runs,
//! plus a user in team A, a user in team B and a user in both, every one of
//! them authenticated by the production `SessionsAndTokens` over a real token.
//!
//! The stream is read with `Response::chunk()`, ignoring every line that
//! starts with `:` (axum's keep-alive). **Absence is proven by ordering,
//! never by a timeout**: every "must not arrive" is followed by a sentinel
//! the stream must receive, published on the same channel after the
//! forbidden event. No test sleeps or waits on a timer.

mod common;

use std::sync::Arc;

use pretty_assertions::assert_eq;
use rimaia_core::api::events::{board_reread, board_wire, tail_wire, WireEvent};
use rimaia_core::api::{BoardHost, PROTOCOL_VERSION};
use rimaia_core::events::ChangeEvent;
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::secret::{csrf_for, Secret};
use rimaia_core::identity::{create_personal_team, tokens, Role};
use rimaia_core::runner::events::RunTail;
use rimaia_core::scheduler::{state, QueueState};
use rimaia_core::tasks;
use rimaia_core::testing::api as testing_api;
use rimaia_core::testing::shared::add_member;
use rimaia_core::testing::teams::TwoTeams;
use serde_json::{json, Value};

use common::{state_over, Answer, TestServer};

/// The server, the board it serves, and the three people on it.
struct Fixture {
    server: TestServer,
    host: BoardHost,
    teams: TwoTeams,
    /// Team A's owner's desktop token.
    a: Secret,
    /// Team B's owner's desktop token.
    b: Secret,
    /// The desktop token of a user in both teams (and their own).
    both: Secret,
    both_user: String,
}

impl Fixture {
    async fn new() -> Self {
        let teams = TwoTeams::new().await;
        let host = testing_api::host(&teams).await;
        let (both_user, both) = member_of_both(&teams).await;
        let auth = SessionsAndTokens {
            pool: teams.a.pool.clone(),
            clock: Arc::new(teams.clock.clone()),
        };
        let server = TestServer::spawn(state_over(host.clone(), auth)).await;
        Self {
            server,
            host,
            a: teams.team_a.credentials.desktop_token.clone(),
            b: teams.team_b.credentials.desktop_token.clone(),
            both,
            both_user,
            teams,
        }
    }

    fn url(&self, query: &str) -> String {
        format!("{}/api/v1/events{query}", self.server.base_url())
    }

    /// `GET /api/v1/events<query>` as `token`, at this build's protocol.
    async fn get(&self, token: &Secret, query: &str) -> reqwest::Response {
        self.server
            .client()
            .get(self.url(query))
            .header("authorization", format!("Bearer {}", token.expose()))
            .header("rimaia-protocol", PROTOCOL_VERSION)
            .send()
            .await
            .expect("the request reaches the server")
    }

    /// A stream as `token`, its opening burst read and checked: from here on
    /// it is subscribed, and everything it yields was published after.
    async fn open(&self, token: &Secret, query: &str) -> Stream {
        let response = self.get(token, query).await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let mut stream = Stream::new(response);
        for expected in board_reread() {
            assert_eq!(stream.next().await, Some(expected), "the opening burst");
        }
        stream
    }

    fn publish(&self, event: &ChangeEvent) {
        self.host.context.publish(event.clone());
    }

    fn team_a(&self) -> String {
        self.teams.team_a.team_id.clone()
    }

    fn team_b(&self) -> String {
        self.teams.team_b.team_id.clone()
    }
}

/// A user in both fixture teams: signed up the way the product signs anyone
/// up, then given a membership in each team by hand, because creating shared
/// teams and inviting is task 051's. Their own desktop token, made the way the
/// desktop's sign-in makes one.
async fn member_of_both(teams: &TwoTeams) -> (String, Secret) {
    let pool = &teams.a.pool;
    let mut conn = pool.acquire().await.expect("a connection");
    let user = create_personal_team(&mut conn, &teams.clock, "linus")
        .await
        .expect("sign up a third user");
    for team in [&teams.team_a, &teams.team_b] {
        add_member(
            &mut conn,
            &teams.clock,
            &team.team_id,
            &user.user_id,
            Role::Member,
        )
        .await;
    }
    drop(conn);
    let token = tokens::create_desktop(pool, &teams.clock, &user.user_id, "Desktop app")
        .await
        .expect("a desktop token");
    (user.user_id, token.token)
}

/// An open SSE response, read frame by frame.
struct Stream {
    response: reqwest::Response,
    buffer: String,
}

impl Stream {
    fn new(response: reqwest::Response) -> Self {
        Self {
            response,
            buffer: String::new(),
        }
    }

    /// The next event, or `None` once the server ended the stream.
    ///
    /// A frame of comment lines only is axum's keep-alive and is skipped. A
    /// frame with an `id:` line, or naming an event no board source maps to,
    /// fails the test: there is no log to resume from, and no local event
    /// ever reaches a server.
    async fn next(&mut self) -> Option<WireEvent> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let frame: String = self.buffer[..end].to_string();
                self.buffer.drain(..end + 2);
                if let Some(event) = parse(&frame) {
                    return Some(event);
                }
                continue;
            }
            let chunk = self.response.chunk().await.expect("the stream reads")?;
            self.buffer
                .push_str(std::str::from_utf8(&chunk).expect("the stream is UTF-8"));
        }
    }

    /// Everything up to and including `last`.
    async fn until(&mut self, last: &WireEvent) -> Vec<WireEvent> {
        let mut read = Vec::new();
        loop {
            let event = self.next().await.expect("the stream stays open");
            let done = &event == last;
            read.push(event);
            if done {
                return read;
            }
        }
    }
}

/// One frame as a [`WireEvent`], or `None` for a keep-alive.
fn parse(frame: &str) -> Option<WireEvent> {
    let mut name = None;
    let mut data = None;
    for line in frame.lines() {
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => name = Some(value.to_string()),
            "data" => data = Some(value.to_string()),
            other => panic!("a frame carries a `{other}:` line: {frame:?}"),
        }
    }
    let name = name?;
    let name: &'static str = match name.as_str() {
        "tasks:changed" => "tasks:changed",
        "repositories:changed" => "repositories:changed",
        "runs:changed" => "runs:changed",
        "settings:changed" => "settings:changed",
        "runs:tail" => "runs:tail",
        other => panic!("a stream carried `{other}`, which no board source maps to"),
    };
    let data: Value = serde_json::from_str(&data.expect("every event has data"))
        .expect("an event's data is JSON");
    Some(WireEvent { name, data })
}

fn wire(events: &[ChangeEvent]) -> Vec<WireEvent> {
    events
        .iter()
        .map(|event| board_wire(&event.change))
        .collect()
}

fn sentinel(team_id: String, name: &str) -> ChangeEvent {
    ChangeEvent::tasks(team_id, [format!("sentinel-{name}")])
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

// ---------------------------------------------------------------------------
// Isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_teams_on_one_server_never_see_each_others_events() {
    let f = Fixture::new().await;
    let mut in_a = f.open(&f.a, "").await;
    let mut in_b = f.open(&f.b, "").await;
    let mut in_both = f.open(&f.both, "").await;

    let (a, b) = (&f.teams.team_a, &f.teams.team_b);
    let published = [
        ChangeEvent::tasks(f.team_a(), [a.ready.clone()]),
        ChangeEvent::repositories(f.team_a(), [a.repository.id.clone()]),
        ChangeEvent::runs(f.team_a(), [a.implementation_run.clone()]),
        ChangeEvent::settings(f.team_a()),
        ChangeEvent::tasks(f.team_b(), [b.ready.clone()]),
        ChangeEvent::repositories(f.team_b(), [b.repository.id.clone()]),
        ChangeEvent::runs(f.team_b(), [b.implementation_run.clone()]),
        ChangeEvent::settings(f.team_b()),
    ];
    for event in &published {
        f.publish(event);
    }
    let (sentinel_a, sentinel_b) = (sentinel(f.team_a(), "a"), sentinel(f.team_b(), "b"));
    f.publish(&sentinel_a);
    f.publish(&sentinel_b);

    let last_a = board_wire(&sentinel_a.change);
    let last_b = board_wire(&sentinel_b.change);
    let read_a = in_a.until(&last_a).await;
    let read_b = in_b.until(&last_b).await;
    assert_eq!(
        read_a,
        wire(&[&published[..4], std::slice::from_ref(&sentinel_a)].concat())
    );
    assert_eq!(
        read_b,
        wire(&[&published[4..], std::slice::from_ref(&sentinel_b)].concat())
    );
    assert_eq!(
        in_both.until(&last_b).await,
        wire(&[&published[..], &[sentinel_a, sentinel_b]].concat())
    );

    // And by the ids themselves: nothing A read names B, and the reverse.
    let text = |events: &[WireEvent]| -> String {
        events.iter().map(|event| event.data.to_string()).collect()
    };
    for id in b.ids() {
        assert!(!text(&read_a).contains(&id), "team A's stream carries {id}");
    }
    for id in a.ids() {
        assert!(!text(&read_b).contains(&id), "team B's stream carries {id}");
    }
}

#[tokio::test]
async fn a_foreign_tail_id_opens_the_same_stream_as_an_unknown_one_and_delivers_nothing() {
    let f = Fixture::new().await;
    let foreign = f.teams.team_b.implementation_run.clone();
    let unknown = TwoTeams::never_issued();
    let mut naming_foreign = f.open(&f.a, &format!("?tail={foreign}")).await;
    let mut naming_unknown = f.open(&f.a, &format!("?tail={unknown}")).await;

    f.host.context.publish_tail(f.team_b(), tail(&foreign, 1));
    f.host.context.publish_tail(f.team_b(), tail(&foreign, 2));
    let last = sentinel(f.team_a(), "a");
    f.publish(&last);

    for stream in [&mut naming_foreign, &mut naming_unknown] {
        assert_eq!(
            stream.until(&board_wire(&last.change)).await,
            wire(std::slice::from_ref(&last))
        );
    }
}

#[tokio::test]
async fn a_stale_tail_id_does_not_end_the_stream() {
    let f = Fixture::new().await;
    let task = f.teams.team_a.in_review.clone();
    let run = f.teams.team_a.implementation_run.clone();
    tasks::delete_task(&f.teams.a, &task)
        .await
        .expect("delete the task, and its runs with it");

    let mut stream = f.open(&f.a, &format!("?tail={run}")).await;
    let event = ChangeEvent::tasks(f.team_a(), [f.teams.team_a.ready.clone()]);
    f.publish(&event);

    assert_eq!(stream.next().await, Some(board_wire(&event.change)));
}

#[tokio::test]
async fn a_stream_never_carries_another_teams_tail() {
    let f = Fixture::new().await;
    let own = f.teams.team_a.implementation_run.clone();
    let foreign = f.teams.team_b.implementation_run.clone();
    let mut stream = f.open(&f.a, &format!("?tail={own}&tail={foreign}")).await;

    f.host.context.publish_tail(f.team_b(), tail(&foreign, 1));
    f.host.context.publish_tail(f.team_a(), tail(&own, 2));

    assert_eq!(stream.next().await, Some(tail_wire(&tail(&own, 2))));
}

#[tokio::test]
async fn a_user_setting_reaches_only_that_users_streams() {
    let f = Fixture::new().await;
    let mut in_a = f.open(&f.a, "").await;
    let mut in_b = f.open(&f.b, "").await;
    let mut in_both = f.open(&f.both, "").await;

    let written = f
        .server
        .command(
            "set_subscription_cost",
            f.both.expose(),
            &json!({ "value": 30.0 }),
        )
        .await;
    assert_eq!(written.status, 200, "{:?}", written.body);
    let (sentinel_a, sentinel_b) = (sentinel(f.team_a(), "a"), sentinel(f.team_b(), "b"));
    f.publish(&sentinel_a);
    f.publish(&sentinel_b);

    let setting = board_wire(&ChangeEvent::user_settings(f.both_user.clone()).change);
    assert_eq!(
        in_both.until(&board_wire(&sentinel_b.change)).await,
        [
            vec![setting],
            wire(&[sentinel_a.clone(), sentinel_b.clone()])
        ]
        .concat()
    );
    assert_eq!(
        in_a.until(&board_wire(&sentinel_a.change)).await,
        wire(&[sentinel_a])
    );
    assert_eq!(
        in_b.until(&board_wire(&sentinel_b.change)).await,
        wire(&[sentinel_b])
    );
}

#[tokio::test]
async fn a_closed_client_releases_its_subscriptions() {
    let f = Fixture::new().await;
    let changes = || f.host.context.changes.receiver_count();
    let live = || f.host.context.tail.receiver_count();
    let baseline = (changes(), live());

    let stream = f.open(&f.a, "").await;
    assert_eq!((changes(), live()), (baseline.0 + 1, baseline.1 + 1));

    drop(stream);
    while (changes(), live()) != baseline {
        tokio::task::yield_now().await;
    }
}

// ---------------------------------------------------------------------------
// The wire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_new_stream_opens_with_one_wholesale_reread_of_each_board_entity() {
    // `open` reads and checks the burst; `Stream::next` fails on an `id:` line
    // and on any name no board source maps to, for every frame read here.
    let f = Fixture::new().await;
    let mut stream = f.open(&f.a, "").await;

    // A machine write announces itself locally, and a server has no machine
    // to hear it from: the sentinel is what arrives next.
    state::set_queue_state(&f.teams.machine, QueueState::Running)
        .await
        .expect("a runner key, written");
    let last = sentinel(f.team_a(), "a");
    f.publish(&last);

    assert_eq!(stream.next().await, Some(board_wire(&last.change)));
}

#[tokio::test]
async fn a_malformed_or_ninth_tail_id_is_invalid() {
    let f = Fixture::new().await;
    let nine: Vec<String> = (0..9)
        .map(|_| format!("tail={}", TwoTeams::never_issued()))
        .collect();

    for query in [
        "?tail=not-a-run".to_string(),
        format!("?{}", nine.join("&")),
    ] {
        let answer = Answer::of(f.get(&f.a, &query).await).await;
        assert_eq!(answer.status, 400, "{query}");
        assert_eq!(answer.code(), Some("invalid"), "{query}");
        assert!(answer.body["message"].is_string(), "{query}");
    }
}

#[tokio::test]
async fn a_refusal_comes_before_the_stream() {
    let f = Fixture::new().await;
    let credentials = &f.teams.team_a.credentials;
    let client = f.server.client();
    let session_cookie = format!("rimaia_session={}", credentials.session_secret.expose());

    let refused = [
        ("no credential", client.get(f.url(""))),
        (
            "a cookie without its CSRF header",
            client.get(f.url("")).header("cookie", &session_cookie),
        ),
        (
            "a personal access token",
            client.get(f.url("")).header(
                "authorization",
                format!("Bearer {}", credentials.personal_token.expose()),
            ),
        ),
        (
            "a runner token",
            client.get(f.url("")).header(
                "authorization",
                format!("Bearer {}", credentials.runner_token.expose()),
            ),
        ),
    ];
    for (what, request) in refused {
        let response = request
            .header("rimaia-protocol", PROTOCOL_VERSION)
            .send()
            .await
            .expect("the request reaches the server");
        let answer = Answer::of(response).await;
        assert_eq!(answer.status, 401, "{what}");
        assert_eq!(answer.www_authenticate.as_deref(), Some("Bearer"), "{what}");
        assert_eq!(
            answer.body,
            json!({ "code": "unauthenticated", "message": "Sign in to continue." }),
            "{what}"
        );
    }

    // The same session with its header is a stream.
    let response = client
        .get(f.url(""))
        .header("cookie", &session_cookie)
        .header(
            "x-rimaia-csrf",
            csrf_for(credentials.session_secret.expose()),
        )
        .header("rimaia-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .expect("the request reaches the server");
    assert_eq!(response.status().as_u16(), 200);
}

// ---------------------------------------------------------------------------
// Revalidation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn revoking_a_session_ends_its_stream_before_the_next_event() {
    let f = Fixture::new().await;
    let credentials = &f.teams.team_a.credentials;
    let response = f
        .server
        .client()
        .get(f.url(""))
        .header(
            "cookie",
            format!("rimaia_session={}", credentials.session_secret.expose()),
        )
        .header(
            "x-rimaia-csrf",
            csrf_for(credentials.session_secret.expose()),
        )
        .header("rimaia-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .expect("the request reaches the server");
    let mut stream = Stream::new(response);
    for expected in board_reread() {
        assert_eq!(stream.next().await, Some(expected), "the opening burst");
    }

    // 047's revoke path, as the same user's desktop: signing the browser out.
    let revoked = f
        .server
        .command(
            "revoke_session",
            f.a.expose(),
            &json!({ "id": credentials.session_id }),
        )
        .await;
    assert_eq!(revoked.status, 200, "{:?}", revoked.body);
    f.publish(&ChangeEvent::settings(f.team_a()));

    assert_eq!(
        stream.next().await,
        None,
        "the stream ends without the event"
    );
}
