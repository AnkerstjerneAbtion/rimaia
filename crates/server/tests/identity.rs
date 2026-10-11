//! Signing in over HTTP (task 047, ADR-0030).
//!
//! The real router on `127.0.0.1:0` through `rimaia_server::serve`, a
//! temporary board, a `TestClock` and a `FakeIdentityProvider`, authenticated
//! by the production `SessionsAndTokens`. The client follows no redirect, and
//! cookies are read from `Set-Cookie` and sent in `Cookie` by hand: reqwest's
//! `cookies` feature is not approved (seam-contract D34).

mod common;

use std::sync::Arc;

use pretty_assertions::assert_eq;
use reqwest::Url;
use rimaia_core::api::{BoardHost, PROTOCOL_VERSION};
use rimaia_core::db::{self, MutationSource};
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::pending::pending_lifetime;
use rimaia_core::identity::provider::ProviderIdentity;
use rimaia_core::identity::rate_limit::{Bucket, RateLimiter};
use rimaia_core::identity::secret::{self, TokenKind};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderProfile};
use rimaia_core::testing::identity::FakeIdentityProvider;
use rimaia_core::testing::{test_epoch, test_pool, TestClock};
use rimaia_core::{AppPaths, ServiceContext, TeamScope};
use rimaia_server::ServerState;
use serde_json::{json, Value};
use tempfile::TempDir;

use common::{Answer, TestServer};

/// The desktop's loopback listener, as task 059's will be.
const DESKTOP_REDIRECT: &str = "http://127.0.0.1:5555/callback";

/// A server that signs people in, over a board with nobody on it yet.
struct Signing {
    server: TestServer,
    fake: FakeIdentityProvider,
    clock: TestClock,
    /// The server's own `Authenticate`, for asking who a token is.
    auth: SessionsAndTokens,
    _data: TempDir,
}

/// A browser after signing in: its two cookies.
struct Browser {
    session: String,
    csrf: String,
}

impl Browser {
    fn cookie(&self) -> String {
        format!("rimaia_session={}; rimaia_csrf={}", self.session, self.csrf)
    }
}

/// One `Set-Cookie`, split into its name, value and attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SetCookie {
    name: String,
    value: String,
    /// Lowercased, in order, such as `httponly` and `max-age=600`.
    attributes: Vec<String>,
}

fn set_cookies(response: &reqwest::Response) -> Vec<SetCookie> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|line| {
            let mut parts = line.split(';').map(str::trim);
            let (name, value) = parts
                .next()
                .and_then(|pair| pair.split_once('='))
                .expect("a name=value pair");
            SetCookie {
                name: name.to_string(),
                value: value.to_string(),
                attributes: parts.map(str::to_ascii_lowercase).collect(),
            }
        })
        .collect()
}

fn cookie_named<'a>(cookies: &'a [SetCookie], name: &str) -> &'a SetCookie {
    cookies
        .iter()
        .find(|cookie| cookie.name == name)
        .unwrap_or_else(|| panic!("no {name} in {cookies:?}"))
}

fn location(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .expect("a redirect")
        .to_string()
}

fn query_param(url: &str, name: &str) -> Option<String> {
    Url::parse(url)
        .expect("a URL")
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn identity(login: &str, subject: &str) -> ProviderIdentity {
    ProviderIdentity {
        subject: subject.to_string(),
        login: login.to_string(),
        avatar_url: None,
    }
}

impl Signing {
    async fn new() -> Self {
        let pool = test_pool().await;
        let clock = TestClock::new(test_epoch());
        let data = tempfile::Builder::new()
            .prefix("rimaia-server-identity-")
            .tempdir()
            .expect("a data directory");
        let context = ServiceContext::new(
            pool.clone(),
            Arc::new(clock.clone()),
            MutationSource::Ui,
            TeamScope::one(db::new_id()),
            db::new_id(),
        );
        let host = BoardHost {
            rate_limits: Arc::new(RateLimiter::new(context.clock.clone())),
            context,
            provider: ProviderProfile::of(&ClaudeProvider),
            paths: AppPaths::new(data.path()),
        };
        let auth = SessionsAndTokens {
            pool: pool.clone(),
            clock: Arc::new(clock.clone()),
        };
        let fake = FakeIdentityProvider::new();
        let state = ServerState::new(
            host,
            Arc::new(auth.clone()),
            Arc::new(fake.clone()),
            "https://rimaia.test",
        );
        Self {
            server: TestServer::spawn(state).await,
            fake,
            clock,
            auth,
            _data: data,
        }
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> reqwest::Response {
        let mut request = self
            .server
            .client()
            .get(format!("{}{path}", self.server.base_url()))
            .header("user-agent", "Firefox/140");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request
            .send()
            .await
            .expect("the request reaches the server")
    }

    /// `/auth/github`, answered: the provider URL and the sign-in cookie.
    async fn start(&self) -> (String, SetCookie) {
        let response = self.get("/auth/github", None).await;
        assert_eq!(response.status().as_u16(), 303);
        let cookies = set_cookies(&response);
        (
            location(&response),
            cookie_named(&cookies, "rimaia_signin").clone(),
        )
    }

    /// The provider's redirect back, with this browser's sign-in cookie.
    async fn callback(&self, code: &str, state: &str, signin: Option<&str>) -> reqwest::Response {
        let cookie = signin.map(|value| format!("rimaia_signin={value}"));
        self.get(
            &format!("/auth/github/callback?code={code}&state={state}"),
            cookie.as_deref(),
        )
        .await
    }

    /// A whole browser sign-in as `login`.
    async fn sign_in(&self, login: &str, subject: &str) -> Browser {
        let code = format!("code-{login}-{}", db::new_id());
        self.fake.answer(&code, identity(login, subject));
        let (to, signin) = self.start().await;
        let state = query_param(&to, "state").expect("a state");
        let response = self.callback(&code, &state, Some(&signin.value)).await;
        assert_eq!(location(&response), "/");
        let cookies = set_cookies(&response);
        Browser {
            session: cookie_named(&cookies, "rimaia_session").value.clone(),
            csrf: cookie_named(&cookies, "rimaia_csrf").value.clone(),
        }
    }

    /// A board command over the browser's cookies, with or without the CSRF
    /// header.
    async fn as_browser(
        &self,
        name: &str,
        browser: &Browser,
        csrf: Option<&str>,
        args: &Value,
    ) -> Answer {
        let mut request = self
            .server
            .client()
            .post(format!("{}/api/v1/{name}", self.server.base_url()))
            .header("cookie", browser.cookie())
            .header("rimaia-protocol", PROTOCOL_VERSION)
            .body(args.to_string());
        if let Some(csrf) = csrf {
            request = request.header("x-rimaia-csrf", csrf);
        }
        Answer::of(
            request
                .send()
                .await
                .expect("the request reaches the server"),
        )
        .await
    }

    async fn browser_command(&self, name: &str, browser: &Browser, args: &Value) -> Answer {
        self.as_browser(name, browser, Some(&browser.csrf), args)
            .await
    }

    /// A JSON auth route, at this build's protocol.
    async fn post(&self, path: &str, body: &Value) -> Answer {
        Answer::of(
            self.server
                .client()
                .post(format!("{}/api/v1/{path}", self.server.base_url()))
                .header("rimaia-protocol", PROTOCOL_VERSION)
                .body(body.to_string())
                .send()
                .await
                .expect("the request reaches the server"),
        )
        .await
    }

    /// A desktop's whole sign-in: its redirect's code and state.
    async fn desktop_sign_in(&self, login: &str, label: Option<&str>, challenge: &str) -> String {
        let code = format!("code-{login}-{}", db::new_id());
        self.fake.answer(&code, identity(login, "7"));
        let mut path = format!(
            "/auth/desktop?redirect_uri={}&state=desktop-state&code_challenge={challenge}\
             &code_challenge_method=S256",
            DESKTOP_REDIRECT.replace(':', "%3A").replace('/', "%2F"),
        );
        if let Some(label) = label {
            path.push_str(&format!("&label={}", label.replace(' ', "%20")));
        }
        let response = self.get(&path, None).await;
        assert_eq!(response.status().as_u16(), 303, "{path}");
        let to = location(&response);
        let state = query_param(&to, "state").expect("the server's state");
        let signin = cookie_named(&set_cookies(&response), "rimaia_signin")
            .value
            .clone();

        let back = self.callback(&code, &state, Some(&signin)).await;
        let to = location(&back);
        assert!(
            set_cookies(&back)
                .iter()
                .all(|cookie| cookie.name != "rimaia_session"),
            "a desktop sign-in creates no browser session"
        );
        assert!(to.starts_with(DESKTOP_REDIRECT), "{to}");
        assert_eq!(query_param(&to, "state").as_deref(), Some("desktop-state"));
        query_param(&to, "code").expect("a one-time code")
    }
}

#[tokio::test]
async fn a_browser_signs_in_and_calls_a_board_command() {
    let signing = Signing::new().await;
    signing.fake.answer("code-1", identity("ada", "583231"));

    let (to, signin) = signing.start().await;
    let state = query_param(&to, "state").expect("a state");
    let challenge = query_param(&to, "challenge").expect("a challenge");
    assert!(to.starts_with("https://idp.test/authorize?"), "{to}");
    assert_eq!(signin.value, state);
    assert_eq!(
        signin.attributes,
        vec![
            "httponly",
            "samesite=lax",
            "secure",
            "path=/",
            "max-age=600"
        ]
    );

    let response = signing
        .callback("code-1", &state, Some(&signin.value))
        .await;
    assert_eq!(response.status().as_u16(), 303);
    assert_eq!(location(&response), "/");
    let cookies = set_cookies(&response);
    let session = cookie_named(&cookies, "rimaia_session");
    let csrf = cookie_named(&cookies, "rimaia_csrf");
    assert_eq!(
        session.attributes,
        vec![
            "httponly",
            "samesite=lax",
            "secure",
            "path=/",
            "max-age=34560000"
        ]
    );
    assert_eq!(
        csrf.attributes,
        vec!["samesite=lax", "secure", "path=/", "max-age=34560000"],
        "readable by the web app: not HttpOnly"
    );
    assert_eq!(csrf.value, secret::csrf_for(&session.value));
    let cleared = cookie_named(&cookies, "rimaia_signin");
    assert!(
        cleared.attributes.contains(&"max-age=0".to_string()),
        "{cleared:?}"
    );
    for cookie in &cookies {
        assert!(
            cookie
                .attributes
                .iter()
                .all(|attribute| !attribute.starts_with("domain")),
            "{cookie:?}"
        );
    }

    let browser = Browser {
        session: session.value.clone(),
        csrf: csrf.value.clone(),
    };
    let answer = signing
        .browser_command("list_repositories", &browser, &json!({}))
        .await;
    assert_eq!((answer.status, answer.body.clone()), (200, json!([])));
    let account = signing
        .browser_command("get_account", &browser, &json!({}))
        .await;
    assert_eq!(account.body["login"], "ada");
    assert_eq!(account.body["current"]["kind"], "session");

    let exchanges = signing.fake.exchanges();
    assert_eq!(exchanges.len(), 1);
    assert_eq!(secret::pkce_challenge(&exchanges[0].verifier), challenge);
    assert_eq!(
        exchanges[0].redirect_uri,
        "https://rimaia.test/auth/github/callback"
    );
}

#[tokio::test]
async fn a_callback_without_its_sign_in_cookie_is_refused() {
    let signing = Signing::new().await;
    signing.fake.answer("code-1", identity("ada", "1"));
    let (to, _) = signing.start().await;
    let state = query_param(&to, "state").expect("a state");

    for cookie in [None, Some("someone-elses-state")] {
        let response = signing.callback("code-1", &state, cookie).await;
        assert_eq!(location(&response), "/?sign_in=failed", "{cookie:?}");
        assert!(set_cookies(&response)
            .iter()
            .all(|cookie| cookie.name != "rimaia_session"));
    }
    assert!(
        signing.fake.exchanges().is_empty(),
        "the provider was never asked"
    );
}

#[tokio::test]
async fn a_callback_state_is_single_use() {
    let signing = Signing::new().await;
    signing.fake.answer("code-1", identity("ada", "1"));
    let (to, signin) = signing.start().await;
    let state = query_param(&to, "state").expect("a state");

    let first = signing
        .callback("code-1", &state, Some(&signin.value))
        .await;
    assert_eq!(location(&first), "/");
    let again = signing
        .callback("code-1", &state, Some(&signin.value))
        .await;
    assert_eq!(location(&again), "/?sign_in=expired");
    assert!(set_cookies(&again)
        .iter()
        .all(|cookie| cookie.name != "rimaia_session"));
    assert_eq!(signing.fake.exchanges().len(), 1, "exchanged once");
}

#[tokio::test]
async fn a_callback_after_ten_minutes_is_refused() {
    let signing = Signing::new().await;
    signing.fake.answer("code-1", identity("ada", "1"));
    let (to, signin) = signing.start().await;
    let state = query_param(&to, "state").expect("a state");

    signing.clock.advance(pending_lifetime());
    let response = signing
        .callback("code-1", &state, Some(&signin.value))
        .await;

    assert_eq!(location(&response), "/?sign_in=expired");
    assert!(set_cookies(&response)
        .iter()
        .all(|cookie| cookie.name != "rimaia_session"));
    assert!(signing.fake.exchanges().is_empty());
}

#[tokio::test]
async fn a_declined_sign_in_redirects_to_denied() {
    let signing = Signing::new().await;
    let (to, signin) = signing.start().await;
    let state = query_param(&to, "state").expect("a state");

    let response = signing
        .get(
            &format!("/auth/github/callback?error=access_denied&state={state}"),
            Some(&format!("rimaia_signin={}", signin.value)),
        )
        .await;

    assert_eq!(location(&response), "/?sign_in=denied");
}

#[tokio::test]
async fn revoking_the_current_session_signs_out() {
    let signing = Signing::new().await;
    let browser = signing.sign_in("ada", "1").await;
    let account = signing
        .browser_command("get_account", &browser, &json!({}))
        .await;
    let current = account.body["current"]["id"].clone();

    let revoked = signing
        .browser_command("revoke_session", &browser, &json!({ "id": current }))
        .await;
    assert_eq!(revoked.status, 200, "{}", revoked.body);

    let after = signing
        .browser_command("get_account", &browser, &json!({}))
        .await;
    assert_eq!(
        (after.status, after.body, after.www_authenticate.as_deref()),
        (
            401,
            json!({ "code": "unauthenticated", "message": "Sign in to continue." }),
            Some("Bearer")
        )
    );
}

#[tokio::test]
async fn the_desktop_signs_in_through_a_loopback_redirect() {
    let signing = Signing::new().await;
    let verifier = secret::mint_verifier();
    let challenge = secret::pkce_challenge(verifier.expose());

    let code = signing
        .desktop_sign_in("ada", Some("Ada's Mac"), &challenge)
        .await;
    let token = signing
        .post(
            "auth/desktop_token",
            &json!({
                "code": code,
                "codeVerifier": verifier.expose(),
                "redirectUri": DESKTOP_REDIRECT,
            }),
        )
        .await;
    assert_eq!(token.status, 200, "{}", token.body);
    let minted = token.body["token"].as_str().expect("a token").to_string();
    assert!(secret::is_token_of(TokenKind::Desktop, &minted), "{minted}");
    assert_eq!(token.body["summary"]["label"], "Ada's Mac");
    assert_eq!(token.body["summary"]["kind"], "desktop");

    let account = signing
        .server
        .command("get_account", &minted, &json!({}))
        .await;
    assert_eq!(account.status, 200, "{}", account.body);
    assert_eq!(
        account.body["current"],
        json!({ "kind": "desktop", "id": token.body["summary"]["id"] })
    );

    // The same code twice.
    let again = signing
        .post(
            "auth/desktop_token",
            &json!({ "code": code, "codeVerifier": verifier.expose(), "redirectUri": DESKTOP_REDIRECT }),
        )
        .await;
    assert_eq!((again.status, again.code()), (400, Some("invalid")));

    // A wrong verifier, and a different redirect, each on a fresh code.
    let wrong_verifier = signing.desktop_sign_in("ada", None, &challenge).await;
    let refused = signing
        .post(
            "auth/desktop_token",
            &json!({ "code": wrong_verifier, "codeVerifier": secret::mint_verifier().expose(), "redirectUri": DESKTOP_REDIRECT }),
        )
        .await;
    assert_eq!((refused.status, refused.code()), (400, Some("invalid")));
    let other_redirect = signing.desktop_sign_in("ada", None, &challenge).await;
    let refused = signing
        .post(
            "auth/desktop_token",
            &json!({ "code": other_redirect, "codeVerifier": verifier.expose(), "redirectUri": "http://127.0.0.1:6666/callback" }),
        )
        .await;
    assert_eq!((refused.status, refused.code()), (400, Some("invalid")));

    // No label asked for: the default.
    let unlabelled = signing.desktop_sign_in("ada", None, &challenge).await;
    let token = signing
        .post(
            "auth/desktop_token",
            &json!({ "code": unlabelled, "codeVerifier": verifier.expose(), "redirectUri": DESKTOP_REDIRECT }),
        )
        .await;
    assert_eq!(token.status, 200, "{}", token.body);
    assert_eq!(token.body["summary"]["label"], "Desktop app");
}

#[tokio::test]
async fn a_desktop_redirect_that_is_not_loopback_is_refused_before_the_provider() {
    let signing = Signing::new().await;
    let challenge = secret::pkce_challenge(secret::mint_verifier().expose());
    let ok = |redirect: &str| {
        format!(
            "redirect_uri={redirect}&state=s&code_challenge={challenge}&code_challenge_method=S256"
        )
    };
    let long = "x".repeat(101);

    for query in [
        ok("https%3A%2F%2F127.0.0.1%3A5555%2Fcallback"),
        ok("http%3A%2F%2Flocalhost%3A5555%2Fcallback"),
        ok("http%3A%2F%2Fevil.test%3A5555%2Fcallback"),
        format!(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A5555%2Fcallback&state=s\
             &code_challenge={challenge}&code_challenge_method=plain"
        ),
        format!(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A5555%2Fcallback&state=s\
             &code_challenge={challenge}"
        ),
        format!(
            "{}&label=%20%20",
            ok("http%3A%2F%2F127.0.0.1%3A5555%2Fcallback")
        ),
        format!(
            "{}&label={long}",
            ok("http%3A%2F%2F127.0.0.1%3A5555%2Fcallback")
        ),
    ] {
        let response = signing.get(&format!("/auth/desktop?{query}"), None).await;
        assert!(
            response.headers().get("location").is_none(),
            "redirected for {query}"
        );
        let answer = Answer::of(response).await;
        assert_eq!(
            (answer.status, answer.code()),
            (400, Some("invalid")),
            "{query}"
        );
    }
}

#[tokio::test]
async fn a_headless_runner_pairs_with_a_code() {
    let signing = Signing::new().await;
    let browser = signing.sign_in("ada", "1").await;
    let code = signing
        .browser_command("create_pairing_code", &browser, &json!({}))
        .await;
    assert_eq!(code.status, 200, "{}", code.body);
    let code = code.body["code"].as_str().expect("a code").to_string();

    let paired = signing
        .post(
            "auth/pair",
            &json!({ "code": code, "label": "build-box", "provider": "claude-code" }),
        )
        .await;
    assert_eq!(paired.status, 200, "{}", paired.body);
    let runner_id = paired.body["runnerId"]
        .as_str()
        .expect("a runner id")
        .to_string();
    let token = paired.body["token"].as_str().expect("a token").to_string();
    assert!(secret::is_token_of(TokenKind::Runner, &token));

    // It is that runner's token: the board's own `Authenticate` says so, and
    // the board routes refuse it as a runner's.
    let caller = {
        use rimaia_core::api::{Authenticate, Credential, Door};
        let caller = signing
            .auth
            .authenticate(Credential::Bearer(&token))
            .await
            .expect("the runner");
        assert_eq!(
            caller.door,
            Door::Runner {
                runner_id: runner_id.clone()
            }
        );
        caller
    };
    assert!(!caller.teams.is_empty());

    let again = signing
        .post(
            "auth/pair",
            &json!({ "code": code, "label": "build-box", "provider": "claude-code" }),
        )
        .await;
    assert_eq!((again.status, again.code()), (400, Some("invalid")));
}

#[tokio::test]
async fn only_browser_and_desktop_may_call_a_board_route() {
    let signing = Signing::new().await;
    let browser = signing.sign_in("ada", "1").await;
    let personal = signing
        .browser_command(
            "create_personal_access_token",
            &browser,
            &json!({ "label": "Claude Code" }),
        )
        .await;
    let personal = personal.body["token"]
        .as_str()
        .expect("a token")
        .to_string();
    let code = signing
        .browser_command("create_pairing_code", &browser, &json!({}))
        .await;
    let runner = signing
        .post(
            "auth/pair",
            &json!({ "code": code.body["code"], "label": "box", "provider": "claude-code" }),
        )
        .await;
    let runner = runner.body["token"].as_str().expect("a token").to_string();

    for token in [personal, runner] {
        let answer = signing
            .server
            .command("list_repositories", &token, &json!({}))
            .await;
        assert_eq!(
            (
                answer.status,
                answer.code(),
                answer.www_authenticate.as_deref()
            ),
            (401, Some("unauthenticated"), Some("Bearer")),
            "{token:.4}"
        );
    }
}

#[tokio::test]
async fn a_request_with_both_a_cookie_and_a_bearer_token_is_refused() {
    let signing = Signing::new().await;
    let browser = signing.sign_in("ada", "1").await;
    let verifier = secret::mint_verifier();
    let code = signing
        .desktop_sign_in("ada", None, &secret::pkce_challenge(verifier.expose()))
        .await;
    let token = signing
        .post(
            "auth/desktop_token",
            &json!({ "code": code, "codeVerifier": verifier.expose(), "redirectUri": DESKTOP_REDIRECT }),
        )
        .await;
    let token = token.body["token"].as_str().expect("a token").to_string();

    let response = signing
        .server
        .client()
        .post(format!(
            "{}/api/v1/list_repositories",
            signing.server.base_url()
        ))
        .header("authorization", format!("Bearer {token}"))
        .header("cookie", browser.cookie())
        .header("x-rimaia-csrf", &browser.csrf)
        .header("rimaia-protocol", PROTOCOL_VERSION)
        .body("{}")
        .send()
        .await
        .expect("the request reaches the server");
    let answer = Answer::of(response).await;

    assert_eq!(
        (answer.status, answer.code()),
        (401, Some("unauthenticated"))
    );
}

#[tokio::test]
async fn a_cookie_without_its_csrf_header_is_unauthenticated() {
    let signing = Signing::new().await;
    let browser = signing.sign_in("ada", "1").await;

    for csrf in [None, Some("a-guess"), Some(browser.session.as_str())] {
        let answer = signing
            .as_browser("list_repositories", &browser, csrf, &json!({}))
            .await;
        assert_eq!(
            (answer.status, answer.code()),
            (401, Some("unauthenticated")),
            "{csrf:?}"
        );
    }
    let answer = signing
        .browser_command("list_repositories", &browser, &json!({}))
        .await;
    assert_eq!(answer.status, 200);
}

#[tokio::test]
async fn the_sign_in_endpoints_are_rate_limited_per_peer() {
    let signing = Signing::new().await;

    // The three sign-in navigations share one bucket of 30.
    for n in 0..30 {
        let path = ["/auth/github", "/auth/desktop", "/auth/github/callback"][n % 3];
        let response = signing.get(path, None).await;
        assert_ne!(response.status().as_u16(), 429, "{path}");
        if response.status().as_u16() == 400 {
            let answer = Answer::of(response).await;
            assert!(
                !answer.body["message"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("Too many attempts"),
                "attempt {n}: {}",
                answer.body
            );
        }
    }
    for path in ["/auth/github", "/auth/desktop", "/auth/github/callback"] {
        let answer = Answer::of(signing.get(path, None).await).await;
        assert_eq!(
            (answer.status, answer.body),
            (
                400,
                json!({ "code": "invalid", "message": "Too many attempts. Try again in 600 seconds." })
            ),
            "{path}"
        );
    }

    // The two JSON routes have ten each.
    for path in ["auth/desktop_token", "auth/pair"] {
        for _ in 0..10 {
            let answer = signing.post(path, &json!({})).await;
            assert_eq!(answer.code(), Some("invalid"));
            assert!(
                !answer.body["message"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("Too many attempts"),
                "{path}: {}",
                answer.body
            );
        }
        let answer = signing.post(path, &json!({})).await;
        assert_eq!(
            answer.body["message"], "Too many attempts. Try again in 600 seconds.",
            "{path}"
        );
    }

    signing.clock.advance(Bucket::SignIn.window());
    let response = signing.get("/auth/github", None).await;
    assert_eq!(response.status().as_u16(), 303, "a new window");
    let answer = signing.post("auth/pair", &json!({})).await;
    assert!(
        !answer.body["message"]
            .as_str()
            .unwrap_or_default()
            .starts_with("Too many attempts"),
        "{}",
        answer.body
    );
}

#[tokio::test]
async fn a_minted_secret_appears_in_exactly_one_response() {
    let signing = Signing::new().await;
    let browser = signing.sign_in("ada", "1").await;

    let minted = signing
        .browser_command(
            "create_personal_access_token",
            &browser,
            &json!({ "label": "Claude Code", "expiresInDays": 30 }),
        )
        .await;
    assert_eq!(minted.status, 200, "{}", minted.body);
    let token = minted.body["token"].as_str().expect("a token").to_string();
    let hash = secret::hash(&token);
    assert!(secret::is_token_of(TokenKind::Personal, &token));

    for name in ["list_api_tokens", "get_account", "list_sessions"] {
        let answer = signing.browser_command(name, &browser, &json!({})).await;
        assert_eq!(answer.status, 200, "{name}: {}", answer.body);
        let text = answer.body.to_string();
        assert!(!text.contains(&token), "{name} carries the token");
        assert!(!text.contains(&hash), "{name} carries the hash");
        assert!(
            !text.contains(&browser.session),
            "{name} carries the session secret"
        );
    }
    let listed = signing
        .browser_command("list_api_tokens", &browser, &json!({}))
        .await;
    assert_eq!(listed.body[0]["id"], minted.body["summary"]["id"]);
    assert_eq!(listed.body[0]["label"], "Claude Code");
}
