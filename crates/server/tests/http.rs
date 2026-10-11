//! What the board routes do at the HTTP layer, each behaviour once (task 046,
//! seam-contract D32 points 3 and 7, ADR-0037 point 4).

mod common;

use pretty_assertions::assert_eq;
use rimaia_core::api::{Caller, Door, PROTOCOL_VERSION};
use rimaia_core::testing::api::{self as testing_api, FixedCaller, TEAM_A_TOKEN};
use rimaia_core::testing::teams::TwoTeams;
use serde_json::{json, Value};

use common::{state_over, two_teams_state, Answer, TestServer};

async fn serving() -> (TwoTeams, TestServer) {
    let teams = TwoTeams::new().await;
    let server = TestServer::spawn(two_teams_state(&teams)).await;
    (teams, server)
}

#[tokio::test]
async fn an_empty_body_is_an_empty_object() {
    let (teams, server) = serving().await;

    let answer = Answer::of(
        server
            .post(
                "list_repositories",
                Some(TEAM_A_TOKEN),
                Some(PROTOCOL_VERSION),
                "",
            )
            .await,
    )
    .await;

    assert_eq!(answer.status, 200);
    assert_eq!(answer.body[0]["id"], json!(teams.team_a.repository.id));
}

#[tokio::test]
async fn malformed_json_is_invalid_in_the_error_shape() {
    let (_teams, server) = serving().await;

    let answer = Answer::of(
        server
            .post(
                "get_task",
                Some(TEAM_A_TOKEN),
                Some(PROTOCOL_VERSION),
                "{not json",
            )
            .await,
    )
    .await;

    assert_eq!(answer.status, 400);
    assert_eq!(answer.code(), Some("invalid"));
    assert!(
        answer.body["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("the request body is not JSON")),
        "{}",
        answer.body
    );
}

#[tokio::test]
async fn a_unit_output_is_null_with_200() {
    let (_teams, server) = serving().await;

    let response = server
        .post(
            "set_base_instructions",
            Some(TEAM_A_TOKEN),
            Some(PROTOCOL_VERSION),
            json!({ "value": "Open a draft PR." }).to_string(),
        )
        .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().await.expect("a body"), "null");
}

#[tokio::test]
async fn a_wrong_method_is_invalid_in_the_error_shape() {
    let (_teams, server) = serving().await;

    for method in [
        reqwest::Method::GET,
        reqwest::Method::PUT,
        reqwest::Method::DELETE,
    ] {
        let response = server
            .client()
            .request(
                method.clone(),
                format!("{}/api/v1/get_task", server.base_url()),
            )
            .header("authorization", format!("Bearer {TEAM_A_TOKEN}"))
            .send()
            .await
            .expect("the request reaches the server");
        let answer = Answer::of(response).await;

        assert_eq!(
            (answer.status, answer.code()),
            (400, Some("invalid")),
            "{method}"
        );
    }
}

#[tokio::test]
async fn an_unknown_name_is_a_json_not_found_and_never_html() {
    let (_teams, server) = serving().await;

    for name in ["no_such_command", "get_app_info", "get_task/extra"] {
        let answer = Answer::of(
            server
                .post(name, Some(TEAM_A_TOKEN), Some(PROTOCOL_VERSION), "{}")
                .await,
        )
        .await;

        assert_eq!(
            (answer.status, answer.code()),
            (404, Some("not_found")),
            "{name}"
        );
        assert_eq!(
            answer.content_type.as_deref(),
            Some("application/json"),
            "{name}"
        );
    }
}

/// A server whose `FixedCaller` answers `token` as team A's owner through
/// `door`.
async fn serving_a_door(token: &str, door: Door) -> (TwoTeams, TestServer) {
    let teams = TwoTeams::new().await;
    let caller = Caller {
        door,
        ..testing_api::owner(&teams.team_a, testing_api::desktop("unused"))
    };
    let server = TestServer::spawn(state_over(
        testing_api::host(&teams),
        FixedCaller::two_teams(&teams).with(token, caller),
    ))
    .await;
    (teams, server)
}

#[tokio::test]
async fn a_runner_door_cannot_drive_a_board_command() {
    let (teams, server) = serving_a_door(
        "rmr_a-runner",
        Door::Runner {
            runner_id: "runner".to_string(),
        },
    )
    .await;

    let answer = server
        .command(
            "get_task",
            "rmr_a-runner",
            &json!({ "id": teams.team_a.ready }),
        )
        .await;

    assert_eq!(
        (answer.status, answer.code()),
        (401, Some("unauthenticated"))
    );
}

#[tokio::test]
async fn a_bearer_resolving_to_a_browser_door_is_unauthenticated() {
    let (teams, server) = serving_a_door(
        "rmd_a-browser",
        Door::Browser {
            session_id: "session".to_string(),
        },
    )
    .await;

    let answer = server
        .command(
            "get_task",
            "rmd_a-browser",
            &json!({ "id": teams.team_a.ready }),
        )
        .await;

    assert_eq!(
        (answer.status, answer.code()),
        (401, Some("unauthenticated"))
    );
}

#[tokio::test]
async fn an_unsupported_protocol_can_read_but_not_write() {
    let (teams, server) = serving().await;

    let read = Answer::of(
        server
            .post(
                "get_task",
                Some(TEAM_A_TOKEN),
                Some("0.9"),
                json!({ "id": teams.team_a.ready }).to_string(),
            )
            .await,
    )
    .await;
    assert_eq!(read.status, 200);

    let before = (
        teams.snapshot(&teams.team_a).await,
        teams.snapshot_b().await,
    );
    let write = Answer::of(
        server
            .post(
                "update_task",
                Some(TEAM_A_TOKEN),
                Some("0.9"),
                json!({ "id": teams.team_a.ready, "patch": { "title": "renamed" } }).to_string(),
            )
            .await,
    )
    .await;

    assert_eq!(
        (write.status, write.code()),
        (426, Some("upgrade_required"))
    );
    assert!(
        write.body["message"]
            .as_str()
            .is_some_and(|message| message.contains("0.9") && message.contains("1.0")),
        "names what was sent and the oldest accepted: {}",
        write.body
    );
    assert_eq!(
        (
            teams.snapshot(&teams.team_a).await,
            teams.snapshot_b().await
        ),
        before
    );
}

#[tokio::test]
async fn a_missing_protocol_header_cannot_write() {
    let (teams, server) = serving().await;
    let before = teams.snapshot(&teams.team_a).await;

    let write = Answer::of(
        server
            .post(
                "update_task",
                Some(TEAM_A_TOKEN),
                None,
                json!({ "id": teams.team_a.ready, "patch": { "title": "renamed" } }).to_string(),
            )
            .await,
    )
    .await;

    assert_eq!(
        (write.status, write.code()),
        (426, Some("upgrade_required"))
    );
    assert_eq!(teams.snapshot(&teams.team_a).await, before);

    // A read without the header is still answered.
    let read = Answer::of(
        server
            .post(
                "get_task",
                Some(TEAM_A_TOKEN),
                None,
                json!({ "id": teams.team_a.ready }).to_string(),
            )
            .await,
    )
    .await;
    assert_eq!(read.status, 200);
}

#[tokio::test]
async fn authentication_is_checked_before_the_protocol() {
    let (teams, server) = serving().await;

    let answer = Answer::of(
        server
            .post(
                "update_task",
                None,
                None,
                json!({ "id": teams.team_a.ready, "patch": { "title": "renamed" } }).to_string(),
            )
            .await,
    )
    .await;

    assert_eq!(
        (answer.status, answer.code()),
        (401, Some("unauthenticated"))
    );
}

#[tokio::test]
async fn a_body_that_is_not_an_argument_object_is_invalid_and_names_the_command() {
    let (_teams, server) = serving().await;

    let answer: Answer = server
        .command("get_task", TEAM_A_TOKEN, &Value::from(7))
        .await;

    assert_eq!((answer.status, answer.code()), (400, Some("invalid")));
    assert!(
        answer.body["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("`get_task`")),
        "{}",
        answer.body
    );
}
