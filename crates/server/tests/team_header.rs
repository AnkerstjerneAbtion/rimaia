//! `Rimaia-Team`: the team on screen, narrowing a caller at the edge (task 050
//! Scope 3, ADR-0029 Consequences, seam-contract D32 point 7).
//!
//! The people are real: a user in both fixture teams, signed up the way the
//! product signs anyone up and authenticated by the production
//! `SessionsAndTokens` over a real desktop token. The stream half of the
//! header is in `events.rs`, which owns the stream-reading harness.

mod common;

use std::sync::Arc;

use pretty_assertions::assert_eq;
use rimaia_core::api::PROTOCOL_VERSION;
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::secret::{csrf_for, Secret};
use rimaia_core::identity::{create_personal_team, sessions, tokens, Role};
use rimaia_core::testing::api as testing_api;
use rimaia_core::testing::shared::add_member;
use rimaia_core::testing::teams::TwoTeams;
use serde_json::{json, Value};

use common::{state_over, Answer, TestServer};

struct Fixture {
    server: TestServer,
    teams: TwoTeams,
    /// The desktop token of a user in team A, team B and their own.
    both: Secret,
    both_user: String,
}

impl Fixture {
    async fn new() -> Self {
        let teams = TwoTeams::new().await;
        let (both_user, both) = member_of_both(&teams).await;
        let auth = SessionsAndTokens {
            pool: teams.a.pool.clone(),
            clock: Arc::new(teams.clock.clone()),
        };
        let (host, relayed) = testing_api::relayed_host(&teams).await;
        let server = TestServer::spawn(state_over(host, relayed, auth)).await;
        Self {
            server,
            teams,
            both,
            both_user,
        }
    }

    /// `name` as the user in both teams, with `Rimaia-Team: team` when given.
    async fn command(&self, name: &str, team: Option<&str>, args: &Value) -> Answer {
        let mut request = self
            .server
            .client()
            .post(format!("{}/api/v1/{name}", self.server.base_url()))
            .header("authorization", format!("Bearer {}", self.both.expose()))
            .header("rimaia-protocol", PROTOCOL_VERSION)
            .body(args.to_string());
        if let Some(team) = team {
            request = request.header("rimaia-team", team);
        }
        Answer::of(
            request
                .send()
                .await
                .expect("the request reaches the server"),
        )
        .await
    }
}

/// A user in both fixture teams, with their own personal team and a desktop
/// token, who joined each team through an invitation its owner made
/// (`testing::shared::add_member`).
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

fn ids_of(answer: &Answer) -> Vec<String> {
    let mut ids: Vec<String> = answer
        .body
        .as_array()
        .expect("a list")
        .iter()
        .map(|task| task["id"].as_str().expect("an id").to_string())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn a_two_team_caller_lists_one_board_with_the_team_header() {
    let f = Fixture::new().await;
    let (a, b) = (&f.teams.team_a, &f.teams.team_b);
    let list = json!({ "filter": {} });

    let unnarrowed = f.command("list_tasks", None, &list).await;
    let in_a = f.command("list_tasks", Some(&a.team_id), &list).await;
    let in_b = f.command("list_tasks", Some(&b.team_id), &list).await;

    assert_eq!(unnarrowed.status, 400);
    assert_eq!(unnarrowed.code(), Some("invalid"));
    let message = unnarrowed.body["message"].as_str().expect("a message");
    assert!(
        message.contains(&a.team_id) && message.contains(&b.team_id),
        "{message}"
    );
    assert_eq!(in_a.status, 200, "{:?}", in_a.body);
    let mut team_a_tasks = vec![
        a.not_ready.clone(),
        a.ready.clone(),
        a.in_review.clone(),
        a.done.clone(),
    ];
    team_a_tasks.sort();
    assert_eq!(ids_of(&in_a), team_a_tasks);
    assert!(
        !in_a.body.to_string().contains(&b.ready),
        "team A's board names nothing of team B's"
    );
    assert_eq!(in_b.status, 200, "{:?}", in_b.body);
    assert!(ids_of(&in_b).contains(&b.ready));
    assert!(!ids_of(&in_b).contains(&a.ready));
}

#[tokio::test]
async fn a_team_the_caller_is_not_in_is_answered_as_a_team_nobody_made() {
    let f = Fixture::new().await;
    let stranger = f.teams.a.pool.clone();
    // A third team the user is not in.
    let mut conn = stranger.acquire().await.expect("a connection");
    let other = create_personal_team(&mut conn, &f.teams.clock, "grace")
        .await
        .expect("sign up a fourth user");
    drop(conn);
    let list = json!({ "filter": {} });

    let foreign = f.command("list_tasks", Some(&other.team_id), &list).await;
    let never_issued = f
        .command("list_tasks", Some(&TwoTeams::never_issued()), &list)
        .await;

    assert_eq!(foreign.status, 404);
    assert_eq!(foreign.code(), Some("not_found"));
    assert_eq!(foreign.body, never_issued.body);
}

#[tokio::test]
async fn a_header_without_a_credential_is_unauthenticated_and_names_no_team() {
    let f = Fixture::new().await;

    let answer = Answer::of(
        f.server
            .client()
            .post(format!("{}/api/v1/list_tasks", f.server.base_url()))
            .header("rimaia-protocol", PROTOCOL_VERSION)
            .header("rimaia-team", &f.teams.team_a.team_id)
            .body("{}")
            .send()
            .await
            .expect("the request reaches the server"),
    )
    .await;

    assert_eq!(answer.status, 401);
    assert_eq!(answer.code(), Some("unauthenticated"));
}

#[tokio::test]
async fn an_empty_team_header_is_invalid() {
    let f = Fixture::new().await;

    let answer = f
        .command("list_tasks", Some(""), &json!({ "filter": {} }))
        .await;

    assert_eq!(answer.status, 400);
    assert_eq!(answer.code(), Some("invalid"));
}

#[tokio::test]
async fn person_scoped_commands_ignore_the_team_header() {
    let f = Fixture::new().await;
    let (a, b) = (&f.teams.team_a.team_id, &f.teams.team_b.team_id);

    for name in ["get_account", "list_teams", "list_runners"] {
        let without = f.command(name, None, &json!({})).await;
        let with = f.command(name, Some(a), &json!({})).await;

        assert_eq!(without.status, 200, "{name}: {:?}", without.body);
        assert_eq!(with.status, 200, "{name}: {:?}", with.body);
        assert_eq!(with.body, without.body, "{name}");
    }
    let account = f.command("get_account", Some(a), &json!({})).await;
    let listed: Vec<&str> = account.body["teams"]
        .as_array()
        .expect("teams")
        .iter()
        .map(|team| team["id"].as_str().expect("an id"))
        .collect();
    assert!(
        listed.contains(&a.as_str()) && listed.contains(&b.as_str()),
        "{listed:?}"
    );
    let teams = f.command("list_teams", Some(a), &json!({})).await;
    assert_eq!(
        teams.body.as_array().map(Vec::len),
        Some(3),
        "{:?}",
        teams.body
    );

    // A token for a team other than the one on screen.
    for team in [None, Some(a.as_str())] {
        let minted = f
            .command(
                "create_personal_access_token",
                team,
                &json!({ "label": "Claude Code", "teamIds": [b] }),
            )
            .await;

        assert_eq!(minted.status, 200, "{team:?}: {:?}", minted.body);
        assert_eq!(minted.body["summary"]["teamIds"], json!([b]));
    }
}

#[tokio::test]
async fn a_task_is_copied_across_teams_whatever_team_the_request_names() {
    // Task 051: `copy_task_to_team` ignores `Rimaia-Team`, because its target
    // is a repository in another team than the one on screen.
    let f = Fixture::new().await;
    let (a, b) = (&f.teams.team_a, &f.teams.team_b);

    let copied = f
        .command(
            "copy_task_to_team",
            Some(&a.team_id),
            &json!({ "taskId": a.ready, "repositoryId": b.repository.id }),
        )
        .await;

    assert_eq!(copied.status, 200, "{:?}", copied.body);
    assert_eq!(copied.body["repositoryId"], json!(b.repository.id));
    assert_eq!(copied.body["column"], "not_ready");
    assert_ne!(copied.body["id"], json!(a.ready));
}

#[tokio::test]
async fn a_removed_member_is_refused_on_their_next_request() {
    // A real session for the member, and the owner's real desktop token: the
    // removal takes effect on the member's very next request, because
    // `Authenticate` reads memberships on every request (ADR-0030 point 2).
    let f = Fixture::new().await;
    let a = &f.teams.team_a;
    let (_, session) = sessions::create(&f.teams.a.pool, &f.teams.clock, &f.both_user, None)
        .await
        .expect("a session for the member");
    let as_member = || {
        f.server
            .client()
            .post(format!("{}/api/v1/list_tasks", f.server.base_url()))
            .header("cookie", format!("rimaia_session={}", session.expose()))
            .header("x-rimaia-csrf", csrf_for(session.expose()))
            .header("rimaia-protocol", PROTOCOL_VERSION)
            .header("rimaia-team", &a.team_id)
            .body(json!({ "filter": {} }).to_string())
    };

    let before = Answer::of(as_member().send().await.expect("the request")).await;
    assert_eq!(before.status, 200, "{:?}", before.body);

    let removed = f
        .server
        .command(
            "remove_member",
            a.credentials.desktop_token.expose(),
            &json!({ "teamId": a.team_id, "userId": f.both_user }),
        )
        .await;
    assert_eq!(removed.status, 200, "{:?}", removed.body);

    let after = Answer::of(as_member().send().await.expect("the request")).await;
    assert_eq!(after.status, 404);
    assert_eq!(
        after.body,
        json!({ "code": "not_found", "message": "There is no team with that id." })
    );
}
