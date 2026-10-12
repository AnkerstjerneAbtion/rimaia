//! Sign-in, sessions, tokens and pairing, through the store (task 047,
//! ADR-0030).
//!
//! Every time is a `TestClock`'s, nothing sleeps, and nothing is mocked: each
//! test signs people up with `upsert_user`, mints credentials with the
//! identity services, and asks the production `Authenticate` who they are.

use std::sync::Arc;

use chrono::Duration;
use pretty_assertions::assert_eq;
use rimaia_core::api::{self, Authenticate, BoardHost, Caller, Credential, Door, TeamGrant};
use rimaia_core::db::{self, MutationSource};
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::pairing::{self, PairedRunner, PairingCode, INVALID_CODE};
use rimaia_core::identity::pending::{DesktopCode, PendingSignIn, SignInFlow};
use rimaia_core::identity::provider::ProviderIdentity;
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::identity::secret::{self, Secret, TokenKind};
use rimaia_core::identity::tokens::{self, NewApiToken};
use rimaia_core::identity::{runners, sessions, sign_in, Role};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderProfile};
use rimaia_core::tasks;
use rimaia_core::testing::{test_epoch, test_pool, TestClock};
use rimaia_core::{AppPaths, Clock, Error, ErrorCode, ServiceContext, TeamScope};
use serde_json::{json, Value};
use sqlx::SqlitePool;

/// A server's board: people sign up on it, and nothing else is on it.
struct Board {
    pool: SqlitePool,
    clock: TestClock,
    auth: SessionsAndTokens,
    /// A context that reaches nothing, which `for_caller` re-scopes.
    base: ServiceContext,
}

impl Board {
    async fn new() -> Self {
        let pool = test_pool().await;
        let clock = TestClock::new(test_epoch());
        let auth = SessionsAndTokens {
            pool: pool.clone(),
            clock: Arc::new(clock.clone()),
        };
        let base = ServiceContext::new(
            pool.clone(),
            Arc::new(clock.clone()),
            MutationSource::Ui,
            TeamScope::one(db::new_id()),
            db::new_id(),
        );
        Self {
            pool,
            clock,
            auth,
            base,
        }
    }

    /// A person signing up with the subject `subject`.
    async fn sign_up(&self, login: &str, subject: &str) -> String {
        sign_in::upsert_user(
            &self.pool,
            &self.clock,
            "github",
            &ProviderIdentity {
                subject: subject.to_string(),
                login: login.to_string(),
                avatar_url: None,
            },
        )
        .await
        .expect("sign up")
    }

    /// `user_id`'s context, as a request through a desktop token builds it.
    async fn ctx(&self, user_id: &str) -> ServiceContext {
        let token = self.desktop(user_id).await;
        let caller = self
            .bearer(token.token.expose())
            .await
            .expect("the desktop");
        self.base.for_caller(&caller).expect("a context")
    }

    async fn desktop(&self, user_id: &str) -> NewApiToken {
        tokens::create_desktop(&self.pool, &self.clock, user_id, "Desktop app")
            .await
            .expect("a desktop token")
    }

    async fn session(&self, user_id: &str) -> (String, Secret) {
        sessions::create(&self.pool, &self.clock, user_id, Some("Firefox"))
            .await
            .expect("a session")
    }

    /// The cookie and its matching CSRF header.
    async fn present(&self, session_secret: &Secret) -> rimaia_core::Result<Caller> {
        let csrf = secret::csrf_for(session_secret.expose());
        self.auth
            .authenticate(Credential::Session {
                secret: session_secret.expose(),
                csrf: Some(&csrf),
            })
            .await
    }

    async fn bearer(&self, token: &str) -> rimaia_core::Result<Caller> {
        self.auth.authenticate(Credential::Bearer(token)).await
    }

    async fn personal_team(&self, user_id: &str) -> String {
        sqlx::query_scalar("SELECT id FROM teams WHERE personal_user_id = ?1")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .expect("the personal team")
    }

    /// A shared team with `members`, each a `member` and none an owner: a
    /// state no service makes (task 051 always leaves a team an owner), so
    /// it is written here by hand.
    async fn shared_team(&self, members: &[&str]) -> String {
        let team_id = db::new_id();
        sqlx::query("INSERT INTO teams (id, name, created_at) VALUES (?1, 'Acme', ?2)")
            .bind(&team_id)
            .bind(test_epoch())
            .execute(&self.pool)
            .await
            .expect("a shared team");
        for member in members {
            sqlx::query(
                "INSERT INTO team_memberships (team_id, user_id, role, created_at)
                 VALUES (?1, ?2, 'member', ?3)",
            )
            .bind(&team_id)
            .bind(member)
            .bind(test_epoch())
            .execute(&self.pool)
            .await
            .expect("a membership");
        }
        team_id
    }

    async fn leave(&self, team_id: &str, user_id: &str) {
        sqlx::query("DELETE FROM team_memberships WHERE team_id = ?1 AND user_id = ?2")
            .bind(team_id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .expect("leave the team");
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&self.pool)
            .await
            .expect("a count")
    }
}

fn team_ids(caller: &Caller) -> Vec<String> {
    caller
        .teams
        .iter()
        .map(|grant| grant.team_id.clone())
        .collect()
}

fn assert_unauthenticated(answer: rimaia_core::Result<Caller>, what: &str) {
    let refused = answer.expect_err(what);
    assert_eq!(
        serde_json::to_string(&refused).expect("serialize"),
        r#"{"code":"unauthenticated","message":"Sign in to continue."}"#,
        "{what}"
    );
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_expires_after_thirty_idle_days_and_its_row_is_deleted() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let (session_id, live) = board.session(&ada).await;
    let (_, idle) = board.session(&ada).await;

    board
        .clock
        .advance(Duration::days(30) - Duration::seconds(1));
    let caller = board
        .present(&live)
        .await
        .expect("a second short of thirty days");
    assert_eq!(caller.door, Door::Browser { session_id });

    board.clock.advance(Duration::seconds(1));
    assert_unauthenticated(board.present(&idle).await, "exactly thirty idle days");
    assert_eq!(
        board.count("SELECT count(*) FROM sessions").await,
        1,
        "the idle row is gone, the one used a second ago stays"
    );
}

#[tokio::test]
async fn a_session_is_touched_at_most_once_an_hour() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let (session_id, secret) = board.session(&ada).await;
    let last_used = || async {
        let text: String = sqlx::query_scalar("SELECT last_used_at FROM sessions WHERE id = ?1")
            .bind(&session_id)
            .fetch_one(&board.pool)
            .await
            .expect("the session");
        text
    };
    let created = last_used().await;

    board.clock.advance(Duration::minutes(59));
    board.present(&secret).await.expect("used");
    assert_eq!(last_used().await, created, "under an hour writes nothing");

    board.clock.advance(Duration::minutes(1));
    board.present(&secret).await.expect("used");
    let touched = last_used().await;
    assert_ne!(touched, created, "an hour on, it is written");

    board.clock.advance(Duration::minutes(30));
    board.present(&secret).await.expect("used");
    assert_eq!(last_used().await, touched);

    // Touched at the hour, so thirty days from then, not from creation.
    board
        .clock
        .advance(Duration::days(30) - Duration::minutes(31));
    board
        .present(&secret)
        .await
        .expect("thirty days from the last touch, less a minute");
}

#[tokio::test]
async fn an_idle_expired_session_is_not_listed_and_the_next_sign_in_deletes_it() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let grace = board.sign_up("grace", "2").await;
    let (abandoned, _) = board.session(&ada).await;
    let (_, graces) = board.session(&grace).await;
    let ctx = board.ctx(&ada).await;

    board.clock.advance(Duration::days(30));
    assert_eq!(
        sessions::list(&ctx, None).await.expect("list"),
        vec![],
        "an expired session is not shown as live"
    );
    assert_eq!(
        board
            .count(&format!(
                "SELECT count(*) FROM sessions WHERE id = '{abandoned}'"
            ))
            .await,
        1,
        "listing deletes nothing"
    );

    let (fresh, _) = board.session(&ada).await;
    let ids: Vec<String> = sessions::list(&ctx, Some(&fresh))
        .await
        .expect("list")
        .into_iter()
        .map(|session| {
            assert!(session.current);
            assert_eq!(session.user_agent.as_deref(), Some("Firefox"));
            session.id
        })
        .collect();
    assert_eq!(ids, vec![fresh]);
    assert_eq!(
        board
            .count(&format!(
                "SELECT count(*) FROM sessions WHERE id = '{abandoned}'"
            ))
            .await,
        0,
        "the next sign-in deleted the abandoned row"
    );
    let _ = graces;
    assert_eq!(
        board
            .count(&format!(
                "SELECT count(*) FROM sessions WHERE user_id = '{grace}'"
            ))
            .await,
        1,
        "another user's sessions are theirs to expire"
    );
}

#[tokio::test]
async fn a_revoked_session_is_refused_on_its_next_request() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let (session_id, secret) = board.session(&ada).await;
    board.present(&secret).await.expect("live");

    let ctx = board.ctx(&ada).await;
    sessions::revoke(&ctx, &session_id).await.expect("revoke");

    assert_unauthenticated(board.present(&secret).await, "revoked");
}

#[tokio::test]
async fn a_cookie_request_without_the_csrf_header_is_refused_even_for_a_read() {
    // `Authenticate` runs before any command is looked up, so it cannot know a
    // read from a write: the header is required of every cookie request.
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let (_, secret) = board.session(&ada).await;

    assert_unauthenticated(
        board
            .auth
            .authenticate(Credential::Session {
                secret: secret.expose(),
                csrf: None,
            })
            .await,
        "no header",
    );
    let other = board.session(&ada).await.1;
    for wrong in [
        String::new(),
        "a-guess".to_string(),
        secret.expose().to_string(),
        secret::hash(secret.expose()),
        secret::csrf_for(other.expose()),
    ] {
        assert_unauthenticated(
            board
                .auth
                .authenticate(Credential::Session {
                    secret: secret.expose(),
                    csrf: Some(&wrong),
                })
                .await,
            &format!("header {wrong:?}"),
        );
    }
    board.present(&secret).await.expect("the right header");
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn each_token_kind_answers_its_own_door() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;
    let team = board.personal_team(&ada).await;

    let desktop = board.desktop(&ada).await;
    let personal = tokens::create_personal(&ctx, "Claude Code", &[], None)
        .await
        .expect("a personal access token");
    let runner = pairing::pair_own_runner(&ctx, "laptop", "claude-code")
        .await
        .expect("a runner");

    let owner = vec![TeamGrant {
        team_id: team,
        role: Role::Owner,
    }];
    let answers = [
        (
            desktop.token.expose(),
            Door::Desktop {
                token_id: desktop.summary.id.clone(),
            },
        ),
        (
            personal.token.expose(),
            Door::Mcp {
                token_id: Some(personal.summary.id.clone()),
            },
        ),
        (
            runner.token.expose(),
            Door::Runner {
                runner_id: runner.runner_id.clone(),
            },
        ),
    ];
    for (token, door) in answers {
        assert_eq!(
            board.bearer(token).await.expect("authenticates"),
            Caller {
                user_id: ada.clone(),
                teams: owner.clone(),
                door,
            }
        );
    }
}

#[tokio::test]
async fn a_token_whose_prefix_disagrees_with_its_row_is_refused() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let desktop = board.desktop(&ada).await;

    // The row says personal; the token says desktop.
    sqlx::query("UPDATE api_tokens SET kind = 'personal' WHERE id = ?1")
        .bind(&desktop.summary.id)
        .execute(&board.pool)
        .await
        .expect("relabel the row");
    assert_unauthenticated(board.bearer(desktop.token.expose()).await, "kind disagrees");

    // A prefix that is not ours, and the same secret under another prefix.
    let rest = desktop.token.expose().trim_start_matches("rmd_");
    for token in [
        format!("ghp_{rest}"),
        format!("rmp_{rest}"),
        rest.to_string(),
    ] {
        assert_unauthenticated(board.bearer(&token).await, &token);
    }
}

#[tokio::test]
async fn an_expired_personal_access_token_is_refused() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;
    let token = tokens::create_personal(&ctx, "a week", &[], Some(7))
        .await
        .expect("a token");
    assert_eq!(
        token.summary.expires_at,
        Some(board.clock.now() + Duration::days(7))
    );

    board
        .clock
        .advance(Duration::days(7) - Duration::seconds(1));
    board
        .bearer(token.token.expose())
        .await
        .expect("a second left");
    board.clock.advance(Duration::seconds(1));
    assert_unauthenticated(board.bearer(token.token.expose()).await, "expired");

    for days in [0, 366, -1] {
        let refused = tokens::create_personal(&ctx, "bad", &[], Some(days))
            .await
            .expect_err("out of range");
        assert_eq!(refused.code(), ErrorCode::Invalid, "{days}");
    }
}

#[tokio::test]
async fn a_restricted_personal_access_token_sees_only_the_teams_it_names_and_still_belongs_to() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let grace = board.sign_up("grace", "2").await;
    let personal = board.personal_team(&ada).await;
    let acme = board.shared_team(&[&ada, &grace]).await;
    let ctx = board.ctx(&ada).await;

    let restricted = tokens::create_personal(&ctx, "Acme only", std::slice::from_ref(&acme), None)
        .await
        .expect("a restricted token");
    let unrestricted = tokens::create_personal(&ctx, "Everything", &[], None)
        .await
        .expect("an unrestricted token");
    assert_eq!(restricted.summary.team_ids, vec![acme.clone()]);

    let caller = board
        .bearer(restricted.token.expose())
        .await
        .expect("restricted");
    assert_eq!(team_ids(&caller), vec![acme.clone()]);
    let mut every = vec![personal.clone(), acme.clone()];
    every.sort();
    let caller = board
        .bearer(unrestricted.token.expose())
        .await
        .expect("unrestricted");
    assert_eq!(team_ids(&caller), every);

    board.leave(&acme, &ada).await;
    assert_unauthenticated(
        board.bearer(restricted.token.expose()).await,
        "its user belongs to none of the teams it names",
    );
    let caller = board
        .bearer(unrestricted.token.expose())
        .await
        .expect("unrestricted");
    assert_eq!(team_ids(&caller), vec![personal]);

    // A team the caller does not belong to is not found, not confirmed.
    let elsewhere = board.personal_team(&grace).await;
    let refused = tokens::create_personal(&ctx, "probe", &[elsewhere], None)
        .await
        .expect_err("not a team of ada's");
    assert_eq!(refused.code(), ErrorCode::NotFound);
    let never = tokens::create_personal(&ctx, "probe", &[db::new_id()], None)
        .await
        .expect_err("never issued");
    assert_eq!(refused.to_string(), never.to_string());
}

#[tokio::test]
async fn removing_a_membership_takes_effect_on_the_next_request() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let personal = board.personal_team(&ada).await;
    let acme = board.shared_team(&[&ada]).await;
    let (_, session) = board.session(&ada).await;
    let desktop = board.desktop(&ada).await;

    let mut both = vec![personal.clone(), acme.clone()];
    both.sort();
    assert_eq!(
        team_ids(&board.present(&session).await.expect("session")),
        both
    );
    assert_eq!(
        team_ids(&board.bearer(desktop.token.expose()).await.expect("desktop")),
        both
    );

    board.leave(&acme, &ada).await;

    assert_eq!(
        team_ids(&board.present(&session).await.expect("session")),
        vec![personal.clone()]
    );
    assert_eq!(
        team_ids(&board.bearer(desktop.token.expose()).await.expect("desktop")),
        vec![personal]
    );
}

#[tokio::test]
async fn a_token_records_where_it_was_last_used_at_most_once_an_hour() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let desktop = board.desktop(&ada).await;
    let ctx = board.ctx(&ada).await;
    let used = || async {
        let listed = tokens::list(&ctx, None).await.expect("list");
        let row = listed
            .into_iter()
            .find(|token| token.id == desktop.summary.id)
            .expect("the token");
        (row.last_used_at, row.last_used_from)
    };

    let first = board.clock.now();
    board
        .auth
        .authenticate_from(
            Credential::Bearer(desktop.token.expose()),
            Some("Rimaia/1.0"),
        )
        .await
        .expect("used");
    assert_eq!(used().await, (Some(first), Some("Rimaia/1.0".to_string())));

    board.clock.advance(Duration::minutes(10));
    board
        .auth
        .authenticate_from(
            Credential::Bearer(desktop.token.expose()),
            Some("Rimaia/1.0"),
        )
        .await
        .expect("used");
    assert_eq!(used().await, (Some(first), Some("Rimaia/1.0".to_string())));

    // Somewhere new is written at once.
    board
        .auth
        .authenticate_from(Credential::Bearer(desktop.token.expose()), Some("curl/8"))
        .await
        .expect("used");
    assert_eq!(
        used().await,
        (Some(board.clock.now()), Some("curl/8".to_string()))
    );
}

#[tokio::test]
async fn revoking_a_runner_token_unpairs_its_runner_and_keeps_the_row() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;
    let paired = pairing::pair_own_runner(&ctx, "laptop", "claude-code")
        .await
        .expect("a runner");
    board.bearer(paired.token.expose()).await.expect("paired");
    let token_id = tokens::list(&ctx, None)
        .await
        .expect("list")
        .into_iter()
        .find(|token| token.kind == TokenKind::Runner)
        .expect("the runner token")
        .id;

    board.clock.advance(Duration::minutes(5));
    tokens::revoke(&ctx, &token_id).await.expect("revoke");

    assert_unauthenticated(board.bearer(paired.token.expose()).await, "unpaired");
    let runner: (String, Option<String>) =
        sqlx::query_as("SELECT label, unpaired_at FROM runners WHERE id = ?1")
            .bind(&paired.runner_id)
            .fetch_one(&board.pool)
            .await
            .expect("the row stays");
    assert_eq!(runner.0, "laptop");
    assert!(runner.1.is_some(), "unpaired_at is set");
    assert_eq!(
        board
            .count(&format!(
                "SELECT count(*) FROM api_tokens WHERE runner_id = '{}'",
                paired.runner_id
            ))
            .await,
        0
    );
}

#[tokio::test]
async fn another_users_session_and_token_ids_are_not_found() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let grace = board.sign_up("grace", "2").await;
    let (graces_session, graces_secret) = board.session(&grace).await;
    let graces_token = board.desktop(&grace).await;
    let ctx = board.ctx(&ada).await;

    let session = sessions::revoke(&ctx, &graces_session)
        .await
        .expect_err("grace's");
    let never = sessions::revoke(&ctx, &db::new_id())
        .await
        .expect_err("never issued");
    assert_eq!(
        (session.code(), session.to_string()),
        (ErrorCode::NotFound, never.to_string())
    );
    let token = tokens::revoke(&ctx, &graces_token.summary.id)
        .await
        .expect_err("grace's");
    let never = tokens::revoke(&ctx, &db::new_id())
        .await
        .expect_err("never issued");
    assert_eq!(
        (token.code(), token.to_string()),
        (ErrorCode::NotFound, never.to_string())
    );

    board
        .present(&graces_secret)
        .await
        .expect("grace's session is untouched");
    board
        .bearer(graces_token.token.expose())
        .await
        .expect("grace's token is untouched");
}

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_pairing_code_redeems_once_within_ten_minutes() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;
    let code = pairing::create_code(&ctx).await.expect("a code");
    assert!(secret::is_pairing_code(code.code.expose()));
    assert_eq!(code.expires_at, board.clock.now() + Duration::minutes(10));

    board
        .clock
        .advance(Duration::minutes(10) - Duration::seconds(1));
    // Typed by hand, lowercase and without its hyphen.
    let typed = code.code.expose().to_lowercase().replace('-', "");
    let paired = pairing::redeem(
        &board.pool,
        &board.clock,
        &typed,
        "build-box",
        "claude-code",
    )
    .await
    .expect("redeemed");

    let caller = board
        .bearer(paired.token.expose())
        .await
        .expect("the runner");
    assert_eq!(
        caller.door,
        Door::Runner {
            runner_id: paired.runner_id.clone()
        }
    );
    assert_eq!(caller.user_id, ada);
    let runner: (String, String, String) =
        sqlx::query_as("SELECT user_id, label, provider FROM runners WHERE id = ?1")
            .bind(&paired.runner_id)
            .fetch_one(&board.pool)
            .await
            .expect("the runner row");
    assert_eq!(
        runner,
        (
            ada.clone(),
            "build-box".to_string(),
            "claude-code".to_string()
        )
    );

    let again = pairing::redeem(
        &board.pool,
        &board.clock,
        &typed,
        "build-box",
        "claude-code",
    )
    .await
    .expect_err("used");
    assert_eq!(again.to_string(), INVALID_CODE);
}

#[tokio::test]
async fn an_unknown_a_used_and_an_expired_code_get_the_same_answer() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;
    let used = pairing::create_code(&ctx).await.expect("a code");
    pairing::redeem(
        &board.pool,
        &board.clock,
        used.code.expose(),
        "one",
        "claude-code",
    )
    .await
    .expect("redeemed");
    let expired = pairing::create_code(&ctx).await.expect("a code");
    board.clock.advance(Duration::minutes(10));

    let mut answers = Vec::new();
    for code in ["ZZZZ-ZZZZ", used.code.expose(), expired.code.expose()] {
        let refused = pairing::redeem(&board.pool, &board.clock, code, "two", "claude-code")
            .await
            .expect_err(code);
        answers.push(serde_json::to_value(&refused).expect("serialize"));
    }
    assert_eq!(
        answers,
        vec![json!({ "code": "invalid", "message": INVALID_CODE }); 3]
    );
}

#[tokio::test]
async fn a_code_is_spent_even_when_the_redemption_fails_after_the_lookup() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;
    let code = pairing::create_code(&ctx).await.expect("a code");

    // A label or provider mistake touches no row: the code stays redeemable.
    for (label, provider) in [("", "claude-code"), ("box", "no-such-provider")] {
        let refused = pairing::redeem(
            &board.pool,
            &board.clock,
            code.code.expose(),
            label,
            provider,
        )
        .await
        .expect_err("invalid input");
        assert_eq!(refused.code(), ErrorCode::Invalid);
        assert_ne!(refused.to_string(), INVALID_CODE);
    }
    assert_eq!(board.count("SELECT count(*) FROM pairing_codes").await, 1);

    // A failure after the lookup: the code's delete committed on its own.
    sqlx::query(
        "CREATE TRIGGER refuse_runners BEFORE INSERT ON runners
         BEGIN SELECT RAISE(ABORT, 'no runners today'); END",
    )
    .execute(&board.pool)
    .await
    .expect("a test-only trigger");
    let failed = pairing::redeem(
        &board.pool,
        &board.clock,
        code.code.expose(),
        "box",
        "claude-code",
    )
    .await
    .expect_err("the insert fails");
    assert_eq!(failed.code(), ErrorCode::Database);
    sqlx::query("DROP TRIGGER refuse_runners")
        .execute(&board.pool)
        .await
        .expect("drop the trigger");

    let spent = pairing::redeem(
        &board.pool,
        &board.clock,
        code.code.expose(),
        "box",
        "claude-code",
    )
    .await
    .expect_err("spent");
    assert_eq!(spent.to_string(), INVALID_CODE);
    assert_eq!(board.count("SELECT count(*) FROM runners").await, 0);
}

#[tokio::test]
async fn creating_a_pairing_code_deletes_the_users_expired_ones() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let grace = board.sign_up("grace", "2").await;
    let adas = board.ctx(&ada).await;
    let graces = board.ctx(&grace).await;
    pairing::create_code(&adas).await.expect("a code");
    pairing::create_code(&adas).await.expect("a code");
    pairing::create_code(&graces).await.expect("grace's code");
    board.clock.advance(Duration::minutes(5));
    let live = pairing::create_code(&adas).await.expect("a code");

    board.clock.advance(Duration::minutes(5));
    pairing::create_code(&adas).await.expect("a new code");

    assert_eq!(
        board
            .count(&format!(
                "SELECT count(*) FROM pairing_codes WHERE user_id = '{ada}'"
            ))
            .await,
        2,
        "the two ten-minute-old codes are gone; the five-minute-old one and the new one stay"
    );
    assert_eq!(
        board
            .count(&format!(
                "SELECT count(*) FROM pairing_codes WHERE user_id = '{grace}'"
            ))
            .await,
        1,
        "another user's expired code is theirs to clear"
    );
    pairing::redeem(
        &board.pool,
        &board.clock,
        live.code.expose(),
        "box",
        "claude-code",
    )
    .await
    .expect("the live code still redeems");
}

// ---------------------------------------------------------------------------
// What every refusal shares
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_authentication_failure_says_the_same_thing() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let acme = board.shared_team(&[&ada]).await;
    let ctx = board.ctx(&ada).await;

    let grace = board.sign_up("grace", "2").await;
    let (revoked_id, revoked) = board.session(&ada).await;
    sessions::revoke(&ctx, &revoked_id).await.expect("revoke");
    // Grace's, so ada's next sign-in below does not delete it first.
    let (_, idle) = board.session(&grace).await;
    let relabelled = board.desktop(&ada).await;
    sqlx::query("UPDATE api_tokens SET kind = 'personal' WHERE id = ?1")
        .bind(&relabelled.summary.id)
        .execute(&board.pool)
        .await
        .expect("relabel");
    let expiring = tokens::create_personal(&ctx, "a day", &[], Some(1))
        .await
        .expect("a token");
    let unpaired = pairing::pair_own_runner(&ctx, "laptop", "claude-code")
        .await
        .expect("a runner");
    let mut conn = board.pool.acquire().await.expect("a connection");
    tokens::unpair_runner(&mut conn, &board.clock, &unpaired.runner_id)
        .await
        .expect("unpair");
    drop(conn);
    let stranded = tokens::create_personal(&ctx, "Acme", std::slice::from_ref(&acme), None)
        .await
        .expect("a token");
    board.leave(&acme, &ada).await;

    board.clock.advance(Duration::days(30));
    let (_, live) = board.session(&ada).await;
    let revoked_csrf = secret::csrf_for(revoked.expose());
    let idle_csrf = secret::csrf_for(idle.expose());
    let failures: Vec<(&str, Credential<'_>)> = vec![
        (
            "an unknown session",
            Credential::Session {
                secret: "no-such-secret",
                csrf: Some("x"),
            },
        ),
        (
            "a revoked session",
            Credential::Session {
                secret: revoked.expose(),
                csrf: Some(&revoked_csrf),
            },
        ),
        (
            "an idle session",
            Credential::Session {
                secret: idle.expose(),
                csrf: Some(&idle_csrf),
            },
        ),
        (
            "no CSRF header",
            Credential::Session {
                secret: live.expose(),
                csrf: None,
            },
        ),
        (
            "a wrong CSRF header",
            Credential::Session {
                secret: live.expose(),
                csrf: Some(&idle_csrf),
            },
        ),
        ("an unknown token", Credential::Bearer("rmd_no-such-token")),
        ("no prefix", Credential::Bearer("no-prefix")),
        (
            "a kind that disagrees",
            Credential::Bearer(relabelled.token.expose()),
        ),
        (
            "an expired token",
            Credential::Bearer(expiring.token.expose()),
        ),
        (
            "an unpaired runner",
            Credential::Bearer(unpaired.token.expose()),
        ),
        (
            "no reachable team",
            Credential::Bearer(stranded.token.expose()),
        ),
    ];
    for (what, credential) in failures {
        let refused = board.auth.authenticate(credential).await.expect_err(what);
        assert_eq!(
            serde_json::to_string(&refused).expect("serialize"),
            r#"{"code":"unauthenticated","message":"Sign in to continue."}"#,
            "{what}"
        );
    }
}

#[tokio::test]
async fn no_secret_type_prints_its_value_in_debug() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;

    let token: NewApiToken = tokens::create_personal(&ctx, "Claude Code", &[], None)
        .await
        .expect("a token");
    let paired: PairedRunner = pairing::pair_own_runner(&ctx, "laptop", "claude-code")
        .await
        .expect("a runner");
    let code: PairingCode = pairing::create_code(&ctx).await.expect("a code");
    let secret = Secret::new("a-configured-client-secret");
    let pending = PendingSignIn {
        verifier: Secret::new("a-pkce-verifier"),
        flow: SignInFlow::Desktop {
            redirect_uri: "http://127.0.0.1:5555/callback".to_string(),
            desktop_state: "a-desktop-state".to_string(),
            desktop_challenge: "a-desktop-challenge".to_string(),
            label: "Desktop app".to_string(),
        },
        created_at: board.clock.now(),
    };
    let desktop_code = DesktopCode {
        user_id: ada.clone(),
        desktop_challenge: "a-desktop-challenge".to_string(),
        redirect_uri: "http://127.0.0.1:5555/callback".to_string(),
        label: "Desktop app".to_string(),
        created_at: board.clock.now(),
    };

    let printed =
        format!("{token:?}\n{paired:?}\n{code:?}\n{secret:?}\n{pending:?}\n{desktop_code:?}");
    for value in [
        token.token.expose(),
        paired.token.expose(),
        code.code.expose(),
        secret.expose(),
        "a-pkce-verifier",
        "a-desktop-state",
        "a-desktop-challenge",
    ] {
        assert!(!printed.contains(value), "{value} in:\n{printed}");
    }
    assert!(printed.contains("Secret(…)"), "{printed}");
}

// ---------------------------------------------------------------------------
// The account commands
// ---------------------------------------------------------------------------

/// Every account command, with arguments it would accept on a server.
fn account_commands() -> Vec<(&'static str, Value)> {
    vec![
        ("get_account", json!({})),
        ("list_sessions", json!({})),
        ("revoke_session", json!({ "id": "a-session" })),
        ("list_api_tokens", json!({})),
        (
            "create_personal_access_token",
            json!({ "label": "Claude Code" }),
        ),
        ("revoke_api_token", json!({ "id": "a-token" })),
        ("create_pairing_code", json!({})),
        (
            "pair_own_runner",
            json!({ "label": "laptop", "provider": "claude-code" }),
        ),
    ]
}

#[tokio::test]
async fn account_commands_refuse_the_solo_shell() {
    let harness = rimaia_core::testing::TestContext::new().await;
    let data = tempfile::Builder::new()
        .prefix("rimaia-data-")
        .tempdir()
        .expect("a data directory");
    let host = BoardHost {
        context: harness.context.clone(),
        provider: ProviderProfile::of(&ClaudeProvider),
        paths: AppPaths::new(data.path()),
        rate_limits: Arc::new(RateLimiter::new(Arc::new(harness.clock.clone()))),
        tails: rimaia_core::api::TailRelay::new(rimaia_core::api::tail::SOLO_TAIL_RELAY_CAPACITY),
        streams: rimaia_core::api::StreamControl::new(),
        relay: Arc::new(rimaia_core::board::RelayedRequests::new()),
    };
    let caller = Caller::solo(&harness.solo);

    let commands = account_commands();
    assert_eq!(commands.len(), 8);
    for (name, args) in commands {
        let refused = api::dispatch(&host, caller.clone(), name, args)
            .await
            .expect_err(name);
        assert_eq!(
            serde_json::to_value(&refused).expect("serialize"),
            json!({ "code": "invalid", "message": "Accounts exist only on a Rimaia server." }),
            "{name}"
        );
    }
}

#[tokio::test]
async fn only_a_desktop_pairs_its_own_runner() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let (session_id, secret) = board.session(&ada).await;
    let browser = board.present(&secret).await.expect("a browser");
    assert_eq!(browser.door, Door::Browser { session_id });
    let host = BoardHost {
        context: board.base.clone(),
        provider: ProviderProfile::of(&ClaudeProvider),
        paths: AppPaths::new(std::env::temp_dir()),
        rate_limits: Arc::new(RateLimiter::new(Arc::new(board.clock.clone()))),
        tails: rimaia_core::api::TailRelay::new(rimaia_core::api::tail::SOLO_TAIL_RELAY_CAPACITY),
        streams: rimaia_core::api::StreamControl::new(),
        relay: Arc::new(rimaia_core::board::RelayedRequests::new()),
    };

    let refused = api::dispatch(
        &host,
        browser,
        "pair_own_runner",
        json!({ "label": "laptop", "provider": "claude-code" }),
    )
    .await
    .expect_err("a browser cannot");
    assert_eq!(refused.code(), ErrorCode::Invalid);
    assert!(refused.to_string().contains("desktop app"), "{refused}");
    assert_eq!(board.count("SELECT count(*) FROM runners").await, 0);
}

#[tokio::test]
async fn minting_is_rate_limited_per_user() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let grace = board.sign_up("grace", "2").await;
    let host = BoardHost {
        context: board.base.clone(),
        provider: ProviderProfile::of(&ClaudeProvider),
        paths: AppPaths::new(std::env::temp_dir()),
        rate_limits: Arc::new(RateLimiter::new(Arc::new(board.clock.clone()))),
        tails: rimaia_core::api::TailRelay::new(rimaia_core::api::tail::SOLO_TAIL_RELAY_CAPACITY),
        streams: rimaia_core::api::StreamControl::new(),
        relay: Arc::new(rimaia_core::board::RelayedRequests::new()),
    };
    let mut callers = Vec::new();
    for user in [&ada, &grace] {
        let token = board.desktop(user).await;
        callers.push(board.bearer(token.token.expose()).await.expect("a desktop"));
    }
    let [adas, graces]: [Caller; 2] = callers.try_into().expect("two callers");

    let mints = [
        ("create_personal_access_token", json!({ "label": "t" })),
        ("create_pairing_code", json!({})),
        (
            "pair_own_runner",
            json!({ "label": "l", "provider": "claude-code" }),
        ),
    ];
    for n in 0..20 {
        let (name, args) = &mints[n % 3];
        api::dispatch(&host, adas.clone(), name, args.clone())
            .await
            .unwrap_or_else(|error| panic!("mint {n}: {error}"));
    }
    for (name, args) in &mints {
        let refused: Error = api::dispatch(&host, adas.clone(), name, args.clone())
            .await
            .expect_err(name);
        assert_eq!(
            refused.to_string(),
            "Too many attempts. Try again in 3600 seconds.",
            "{name}"
        );
    }
    api::dispatch(&host, graces, "create_pairing_code", json!({}))
        .await
        .expect("another user's window");

    board.clock.advance(Duration::hours(1));
    api::dispatch(&host, adas, "create_pairing_code", json!({}))
        .await
        .expect("a new window");
}

// ---------------------------------------------------------------------------
// The person's teams and runners (task 050)
// ---------------------------------------------------------------------------

/// A host over the board's base context, for dispatching as a caller.
fn host_of(board: &Board) -> BoardHost {
    BoardHost {
        context: board.base.clone(),
        provider: ProviderProfile::of(&ClaudeProvider),
        paths: AppPaths::new(std::env::temp_dir()),
        rate_limits: Arc::new(RateLimiter::new(Arc::new(board.clock.clone()))),
        tails: rimaia_core::api::TailRelay::new(rimaia_core::api::tail::SOLO_TAIL_RELAY_CAPACITY),
        streams: rimaia_core::api::StreamControl::new(),
        relay: Arc::new(rimaia_core::board::RelayedRequests::new()),
    }
}

#[tokio::test]
async fn list_teams_lists_every_membership_whatever_the_header_says() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let personal = board.personal_team(&ada).await;
    let acme = board.shared_team(&[&ada]).await;
    let token = board.desktop(&ada).await;
    let caller = board.bearer(token.token.expose()).await.expect("ada");
    // What the server's extractor hands the command it narrows for `list_tasks`.
    let narrowed = caller.narrow_to(&acme).expect("a team ada is in");
    assert_eq!(team_ids(&narrowed), vec![acme.clone()]);
    let host = host_of(&board);

    for as_caller in [caller, narrowed] {
        let listed = api::dispatch(&host, as_caller, "list_teams", json!({}))
            .await
            .expect("list_teams");

        assert_eq!(
            listed,
            json!([
                {
                    "id": personal, "name": "Personal", "role": "owner", "personal": true,
                    "memberCount": 1, "ownerCount": 1,
                },
                {
                    "id": acme, "name": "Acme", "role": "member", "personal": false,
                    "memberCount": 1, "ownerCount": 0,
                },
            ])
        );
    }
}

#[tokio::test]
async fn list_runners_never_shows_another_users_runner() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let grace = board.sign_up("grace", "2").await;
    board.shared_team(&[&ada, &grace]).await;
    let adas = board.ctx(&ada).await;
    let graces = board.ctx(&grace).await;
    let adas_runner = pairing::pair_own_runner(&adas, "Ada's laptop", "claude-code")
        .await
        .expect("a runner");
    let graces_runner = pairing::pair_own_runner(&graces, "Grace's laptop", "claude-code")
        .await
        .expect("a runner");
    let retired = pairing::pair_own_runner(&adas, "Old laptop", "claude-code")
        .await
        .expect("a runner");
    runners::unpair_own(&adas, &retired.runner_id)
        .await
        .expect("unpair");

    let listed = |ctx: ServiceContext| async move {
        runners::list(&ctx)
            .await
            .expect("list")
            .into_iter()
            .map(|runner| (runner.id, runner.label))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        listed(adas).await,
        vec![(adas_runner.runner_id, "Ada's laptop".to_string())]
    );
    assert_eq!(
        listed(graces).await,
        vec![(graces_runner.runner_id, "Grace's laptop".to_string())]
    );
}

#[tokio::test]
async fn a_listed_runner_carries_what_the_board_knows_about_it() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;
    let paired = pairing::pair_own_runner(&ctx, "laptop", "claude-code")
        .await
        .expect("a runner");

    let listed = api::dispatch(
        &host_of(&board),
        board
            .bearer(board.desktop(&ada).await.token.expose())
            .await
            .expect("ada"),
        "list_runners",
        json!({}),
    )
    .await
    .expect("list_runners");

    assert_eq!(
        listed,
        json!([{
            "id": paired.runner_id,
            "label": "laptop",
            "provider": "claude-code",
            "appVersion": null,
            "eligibility": "assigned",
            "pairedAt": test_epoch(),
            "lastSeenAt": null,
        }])
    );
}

/// A task in `user_id`'s personal team with one finished run that names
/// `runner_id`, written directly: the test is about what unpairing leaves
/// behind, not about how a run gets claimed. Returns the task's and run's ids.
async fn seed_run_on(board: &Board, user_id: &str, runner_id: &str) -> (String, String) {
    let team_id = board.personal_team(user_id).await;
    let repository_id = db::new_id();
    let task_id = db::new_id();
    let run_id = db::new_id();
    let now = board.clock.now();
    sqlx::query(
        "INSERT INTO repositories (id, team_id, name, default_branch, allow_unattended_runs, created_at)
         VALUES (?1, ?2, 'rimaia', 'main', 0, ?3)",
    )
    .bind(&repository_id)
    .bind(&team_id)
    .bind(now)
    .execute(&board.pool)
    .await
    .expect("a repository");
    sqlx::query(
        "INSERT INTO tasks (id, team_id, repository_id, title, board_column, position, run_state,
                            created_at, updated_at)
         VALUES (?1, ?2, ?3, 'ran on the laptop', 'ready', 1.0, 'idle', ?4, ?4)",
    )
    .bind(&task_id)
    .bind(&team_id)
    .bind(&repository_id)
    .bind(now)
    .execute(&board.pool)
    .await
    .expect("a task");
    sqlx::query(
        "INSERT INTO runs (id, task_id, attempt, status, session_id, prompt, started_at, runner_id)
         VALUES (?1, ?2, 1, 'succeeded', 'session-1', 'a prompt', ?3, ?4)",
    )
    .bind(&run_id)
    .bind(&task_id)
    .bind(now)
    .bind(runner_id)
    .execute(&board.pool)
    .await
    .expect("a run");
    (task_id, run_id)
}

#[tokio::test]
async fn unpairing_a_runner_revokes_its_tokens_and_keeps_its_row() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let ctx = board.ctx(&ada).await;
    let paired = pairing::pair_own_runner(&ctx, "laptop", "claude-code")
        .await
        .expect("a runner");
    board.bearer(paired.token.expose()).await.expect("paired");
    let (task_id, run_id) = seed_run_on(&board, &ada, &paired.runner_id).await;
    board.clock.advance(Duration::minutes(5));

    runners::unpair_own(&ctx, &paired.runner_id)
        .await
        .expect("unpair");

    assert_unauthenticated(board.bearer(paired.token.expose()).await, "unpaired");
    let named: Option<String> = sqlx::query_scalar("SELECT runner_id FROM runs WHERE id = ?1")
        .bind(&run_id)
        .fetch_one(&board.pool)
        .await
        .expect("the run stays");
    assert_eq!(named, Some(paired.runner_id.clone()));
    let detail = tasks::get_task(&ctx, &task_id).await.expect("read");
    assert_eq!(
        detail.worktree_runner,
        Some(tasks::WorktreeRunner {
            id: paired.runner_id.clone(),
            label: "laptop".to_string(),
            unpaired: true,
        })
    );
    let unpaired_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT unpaired_at FROM runners WHERE id = ?1")
            .bind(&paired.runner_id)
            .fetch_one(&board.pool)
            .await
            .expect("the row stays");
    assert_eq!(unpaired_at, Some(board.clock.now()));
    assert_eq!(
        board
            .count(&format!(
                "SELECT count(*) FROM api_tokens WHERE runner_id = '{}'",
                paired.runner_id
            ))
            .await,
        0
    );
}

#[tokio::test]
async fn unpairing_another_users_runner_is_not_found() {
    let board = Board::new().await;
    let ada = board.sign_up("ada", "1").await;
    let grace = board.sign_up("grace", "2").await;
    let adas = board.ctx(&ada).await;
    let graces = board.ctx(&grace).await;
    let graces_runner = pairing::pair_own_runner(&graces, "Grace's laptop", "claude-code")
        .await
        .expect("a runner");
    let retired = pairing::pair_own_runner(&adas, "Old laptop", "claude-code")
        .await
        .expect("a runner");
    runners::unpair_own(&adas, &retired.runner_id)
        .await
        .expect("unpair");

    let never = runners::unpair_own(&adas, &db::new_id())
        .await
        .expect_err("never issued");
    for refused in [
        runners::unpair_own(&adas, &graces_runner.runner_id).await,
        // Already unpaired: answered as a runner that is not there.
        runners::unpair_own(&adas, &retired.runner_id).await,
    ] {
        let refused = refused.expect_err("not ada's to unpair");
        assert_eq!(
            serde_json::to_value(&refused).expect("serialize"),
            serde_json::to_value(&never).expect("serialize")
        );
        assert_eq!(refused.code(), ErrorCode::NotFound);
    }
    board
        .bearer(graces_runner.token.expose())
        .await
        .expect("grace's runner is untouched");
}
