//! The HTTP contract suite (task 046, seam-contract D32 point 5).
//!
//! The routes are built from the registry at runtime, so this, not the wiring
//! script, is what holds them to it. Every case is `testing::api::BOARD_CASES`,
//! the table `crates/core/tests/tenant_isolation.rs` runs through `dispatch`:
//! one table, two doors.

mod common;

use std::collections::BTreeSet;

use pretty_assertions::assert_eq;
use rimaia_core::api::{self, Effect, COMMANDS};
use rimaia_core::testing::api::{self as testing_api, FixedCaller, Foreign, BOARD_CASES};
use rimaia_core::testing::api::{TEAM_A_TOKEN, TEAM_B_TOKEN};
use rimaia_core::testing::teams::TwoTeams;
use serde_json::{json, Value};

use common::{state_over, two_teams_state, Answer, TestServer};

#[test]
fn every_board_command_has_a_case() {
    // `(name, writes)`, so a case whose effect differs from its row's is both
    // a row with no case and a case with no row.
    let rows: BTreeSet<(&str, bool)> = COMMANDS
        .iter()
        .filter_map(|command| {
            command
                .effect()
                .map(|effect| (command.name, effect == Effect::Write))
        })
        .collect();
    let cases: BTreeSet<(&str, bool)> = BOARD_CASES
        .iter()
        .map(|case| (case.name, case.effect == Effect::Write))
        .collect();

    assert_eq!(
        rows.difference(&cases).collect::<Vec<_>>(),
        Vec::<&(&str, bool)>::new(),
        "a board row with no case, or a case of another effect, in testing/api.rs"
    );
    assert_eq!(
        cases.difference(&rows).collect::<Vec<_>>(),
        Vec::<&(&str, bool)>::new(),
        "a case for no board row"
    );
    assert_eq!(BOARD_CASES.len(), cases.len(), "a board row with two cases");
}

#[tokio::test]
async fn every_board_command_has_a_route_and_no_local_command_does() {
    let teams = TwoTeams::new().await;
    let server = TestServer::spawn(two_teams_state(&teams).await).await;

    let mut failures = Vec::new();
    for command in COMMANDS.iter() {
        let answer = Answer::of(server.post(command.name, None, None, "{}").await).await;
        let expected = if command.is_board() {
            (401, Some("unauthenticated"), Some("Bearer"))
        } else {
            (404, Some("not_found"), None)
        };
        let got = (
            answer.status,
            answer.code(),
            answer.www_authenticate.as_deref(),
        );
        if got != expected {
            failures.push(format!("{}: {got:?}, expected {expected:?}", command.name));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn a_team_cannot_see_another_teams_ids() {
    let mut teams = TwoTeams::new().await;
    let server = TestServer::spawn(two_teams_state(&teams).await).await;

    let mut failures = Vec::new();
    for case in BOARD_CASES {
        let args = case
            .foreign
            .args(&teams)
            .unwrap_or_else(|| (case.own)(&teams));
        let before = (
            teams.snapshot(&teams.team_a).await,
            teams.snapshot_b().await,
        );
        let answer = server.command(case.name, TEAM_A_TOKEN, &args).await;
        let text = answer.body.to_string();

        for leaked in testing_api::leaks(&teams.team_b, &args, &text) {
            failures.push(format!(
                "{}: the answer carries B's {leaked}: {text}",
                case.name
            ));
        }
        if teams.snapshot_b().await != before.1 {
            failures.push(format!("{}: team B's rows changed", case.name));
        }
        match &case.foreign {
            Foreign::Ids(_) => {
                if (answer.status, answer.code()) != (404, Some("not_found")) {
                    failures.push(format!(
                        "{}: B's ids answer {} {text}",
                        case.name, answer.status
                    ));
                }
            }
            Foreign::Filter(_) => {
                let (fresh_args, substitutions) =
                    testing_api::with_never_issued_ids(&teams.team_b, &args);
                let fresh = server.command(case.name, TEAM_A_TOKEN, &fresh_args).await;
                let restored = testing_api::restore_ids(&fresh.body.to_string(), &substitutions);
                if (fresh.status, restored.as_str()) != (answer.status, text.as_str()) {
                    failures.push(format!(
                        "{}: a foreign filter answers differently from a missing one\n\
                         foreign: {} {text}\n missing: {} {restored}",
                        case.name, answer.status, fresh.status
                    ));
                }
            }
            Foreign::EntityLess => {}
        }
        // A foreign-id call is refused before it writes anything of A's
        // either.
        if matches!(case.foreign, Foreign::Ids(_))
            && teams.snapshot(&teams.team_a).await != before.0
        {
            failures.push(format!(
                "{}: a refused call changed team A's rows",
                case.name
            ));
        }
        teams.drain_changes();
    }

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Each case's `own` arguments through `dispatch`, as the server calls it with
/// no machine, and over the route, each against its own copy of the same
/// fixture state, both as team A's owner through their fixture desktop token.
/// Only ids and the three secrets the call itself minted are normalized;
/// timestamps are not, because both copies read the fixture's one test clock.
#[tokio::test]
async fn both_transports_answer_every_case_identically() {
    let teams = TwoTeams::new().await;
    let known = testing_api::ids_on_the_board(&teams.a).await;
    let caller = testing_api::as_desktop(&teams.team_a);

    let mut failures = Vec::new();
    for case in BOARD_CASES {
        let args = (case.own)(&teams);

        let (in_process, _in_process_dir) = testing_api::twin(&teams).await;
        let direct = api::dispatch(
            &testing_api::host_over(&teams, &in_process).await,
            caller.clone(),
            case.name,
            args.clone(),
        )
        .await;
        let direct = match direct {
            Ok(value) => (200, value),
            Err(error) => (
                rimaia_server::status_for(error.code()).as_u16(),
                serde_json::to_value(&error).expect("an error serializes"),
            ),
        };

        let (over_http, _over_http_dir) = testing_api::twin(&teams).await;
        let server = TestServer::spawn(state_over(
            testing_api::host_over(&teams, &over_http).await,
            FixedCaller::two_teams(&teams),
        ))
        .await;
        let routed = server.command(case.name, TEAM_A_TOKEN, &args).await;

        // Two refusals compare equal too, so a case that quietly stopped
        // succeeding would prove nothing about the success path.
        let refused = testing_api::REFUSED_FOR_TEAM_A
            .iter()
            .any(|(name, _)| *name == case.name);
        if (direct.0 == 200) == refused {
            failures.push(format!(
                "{}: dispatch answered {} {}",
                case.name, direct.0, direct.1
            ));
        }
        // Three rows answer a secret the call minted (task 047), which no two
        // calls share: each is shape-checked, then normalized like a minted id.
        let secrets = (
            testing_api::normalize_minted_secret(case.name, &direct.1),
            testing_api::normalize_minted_secret(case.name, &routed.body),
        );
        let (direct_body, routed_body) = match secrets {
            (Ok(direct), Ok(routed)) => (direct, routed),
            (direct, routed) => {
                for failure in [direct.err(), routed.err()].into_iter().flatten() {
                    failures.push(failure);
                }
                continue;
            }
        };
        let direct = (
            direct.0,
            testing_api::normalize_minted(&direct_body, &known),
        );
        let routed = (
            routed.status,
            testing_api::normalize_minted(&routed_body, &known),
        );
        if direct != routed {
            failures.push(format!(
                "{}:\n  dispatch: {direct:?}\n     route: {routed:?}",
                case.name
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn reads_publish_no_change_event() {
    let mut teams = TwoTeams::new().await;
    let server = TestServer::spawn(two_teams_state(&teams).await).await;

    let mut failures = Vec::new();
    for case in BOARD_CASES
        .iter()
        .filter(|case| case.effect == Effect::Read)
    {
        teams.drain_changes();
        let answer = server
            .command(case.name, TEAM_A_TOKEN, &(case.own)(&teams))
            .await;
        if answer.status != 200 {
            failures.push(format!("{}: {} {}", case.name, answer.status, answer.body));
        }
        let events = teams.drain_changes();
        if !events.is_empty() {
            failures.push(format!("{}: published {events:?}", case.name));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn each_team_reads_its_own_board_through_its_own_token() {
    let teams = TwoTeams::new().await;
    let server = TestServer::spawn(two_teams_state(&teams).await).await;

    for (token, team) in [(TEAM_A_TOKEN, &teams.team_a), (TEAM_B_TOKEN, &teams.team_b)] {
        let answer = server.command("list_repositories", token, &json!({})).await;
        assert_eq!(answer.status, 200);
        let ids: Vec<Value> = answer
            .body
            .as_array()
            .expect("a list")
            .iter()
            .map(|repository| repository["id"].clone())
            .collect();
        assert_eq!(ids, vec![json!(team.repository.id)]);
    }
}
