//! The board port's contract suite, against the HTTP adapter (ADR-0027 point
//! 5, seam-contract D31 point 13, task 052).
//!
//! Every case `board_contract!` holds runs here unedited: `HttpBoard` talking
//! to the real `rimaia-server` router on an ephemeral loopback port, over a
//! board in a database file, authenticated by the real `SessionsAndTokens`
//! with each runner's real `rmr_` token. `crates/core/tests/board_port_in_process.rs`
//! runs the same cases against `InProcessBoard`; a case that passes there and
//! fails here is a difference between the adapters, which is the bug this
//! suite exists to find.
//!
//! # Each constructor builds its in-process twin's fixture
//!
//! The same people, teams, roles, runner owners and providers, made through
//! the services a sign-up, an invitation and a pairing go through, never
//! through SQL. Two differences, both forced by the server verifying what an
//! in-process adapter takes on trust:
//!
//! - **A runner needs a token.** An in-process runner is a row; here each is
//!   given its `rmr_` token through 047's `tokens::create_runner`, or paired
//!   with one through `pairing::pair_own_runner`.
//! - **A runner's provider is the one it was paired as.** In process,
//!   `runner_on(B, Ledger)` builds an adapter for runner `B`'s row with
//!   another provider. The server refuses a body naming a provider other than
//!   the row's, so here that port is a runner of `B`'s owner paired as
//!   Ledger. The cases that ask for it never compare it with `runner_id(B)`.
//!
//! Nothing sleeps. A wait is a clock advance on the one `TestClock` the
//! server reads, and [`Harness::board`] is the server host's own context, so
//! a case's writes publish on the channels the routes wait on.

use std::sync::Arc;

use rimaia_core::api::tail::SERVER_TAIL_RELAY_CAPACITY;
use rimaia_core::api::{BoardHost, StreamControl, TailRelay};
use rimaia_core::board::{BoardPort, LeaseTerm, RelayedRequests};
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::identity::secret::Secret;
use rimaia_core::identity::{create_personal_team, pairing, tokens, Role};
use rimaia_core::runner::provider::{ClaudeProvider, ProviderId, ProviderProfile};
use rimaia_core::testing::board_contract::{Harness, Which};
use rimaia_core::testing::identity::FakeIdentityProvider;
use rimaia_core::testing::shared::{add_member, SharedTeam};
use rimaia_core::testing::{test_epoch, TestClock, TestContext};
use rimaia_core::{AppPaths, ServiceContext, TeamScope};
use rimaia_runner::board::HttpBoard;
use rimaia_server::ServerState;
use tempfile::TempDir;

/// A runner the harness built: its id and its ports.
struct Served {
    owner: String,
    runner_id: String,
    /// Paired as Claude Code, as every in-process runner's row is.
    port: Arc<dyn BoardPort>,
    /// A runner of the same owner, paired as Ledger (see the module doc).
    ledger: Arc<dyn BoardPort>,
}

struct HttpHarness {
    /// The server host's own context, scoped to the board's team: what cases
    /// arrange and inspect through.
    board: ServiceContext,
    clock: TestClock,
    /// The team a second member joins.
    team_id: String,
    runners: [Served; 2],
    server: tokio::task::JoinHandle<()>,
    /// Held for their `Drop`: the database file and the transcript paths live
    /// in the directory, and the fixtures hold clones and senders.
    _solo: Option<TestContext>,
    _shared: Option<SharedTeam>,
    _data: TempDir,
}

impl Drop for HttpHarness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// What a constructor made before the server exists.
struct Fixture {
    context: ServiceContext,
    clock: TestClock,
    team_id: String,
    term: LeaseTerm,
    tokens: [Tokens; 2],
    data: TempDir,
    solo: Option<TestContext>,
    shared: Option<SharedTeam>,
}

/// One runner's owner, id and tokens.
struct Tokens {
    owner: String,
    runner_id: String,
    token: Secret,
    ledger: Secret,
}

impl HttpHarness {
    fn data_dir() -> TempDir {
        tempfile::Builder::new()
            .prefix("rimaia-board-http-")
            .tempdir()
            .expect("a data directory")
    }

    /// A token for the runner row `runner_id` already has, and a second
    /// runner of `owner`'s paired as Ledger, through the identity services.
    async fn tokens(ctx: &ServiceContext, owner: &str, runner_id: &str, label: &str) -> Tokens {
        let token = {
            let mut tx = ctx.pool.begin().await.expect("a transaction");
            let token = tokens::create_runner(&mut tx, ctx.clock.as_ref(), owner, runner_id, label)
                .await
                .expect("a runner token");
            tx.commit().await.expect("commit the token");
            token.token
        };
        let as_owner = ServiceContext {
            actor: owner.to_string(),
            ..ctx.clone()
        };
        let ledger = pairing::pair_own_runner(&as_owner, &format!("{label} (Ledger)"), "ledger")
            .await
            .expect("a Ledger runner");
        Tokens {
            owner: owner.to_string(),
            runner_id: runner_id.to_string(),
            token,
            ledger: ledger.token,
        }
    }

    /// The real router over the fixture's board, served on `127.0.0.1:0`,
    /// and one `HttpBoard` per runner pointed at it.
    async fn serve(fixture: Fixture) -> Self {
        let Fixture {
            context,
            clock,
            team_id,
            term,
            tokens,
            data,
            solo,
            shared,
        } = fixture;
        let tails = TailRelay::new(SERVER_TAIL_RELAY_CAPACITY);
        tokio::spawn(tails.follow(context.subscribe_tail()));
        let relayed = Arc::new(RelayedRequests::new());
        let host = BoardHost {
            context: context.clone(),
            // What `BoardHost.provider` holds on a server; the runner routes
            // never read it (D32 point 2).
            provider: ProviderProfile::of(&ClaudeProvider),
            paths: AppPaths::new(data.path()),
            rate_limits: Arc::new(RateLimiter::new(Arc::new(clock.clone()))),
            tails,
            streams: StreamControl::new(),
            relay: relayed.clone(),
        };
        let auth = SessionsAndTokens {
            pool: context.pool.clone(),
            clock: Arc::new(clock.clone()),
        };
        let mut state = ServerState::new(
            host,
            relayed,
            Arc::new(auth),
            Arc::new(FakeIdentityProvider::new()),
            "https://rimaia.test",
        );
        state.lease_term = term;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral port");
        let origin = format!(
            "http://{}",
            listener.local_addr().expect("the bound address")
        );
        let router = rimaia_server::router(state);
        let server = tokio::spawn(async move {
            rimaia_server::serve(listener, router)
                .await
                .expect("the test server serves");
        });

        let port = |token: &Secret, provider| -> Arc<dyn BoardPort> {
            Arc::new(HttpBoard::new(&origin, token.clone(), provider).expect("a loopback origin"))
        };
        let runners = tokens.map(|tokens| Served {
            port: port(&tokens.token, ProviderId::ClaudeCode),
            ledger: port(&tokens.ledger, ProviderId::Ledger),
            owner: tokens.owner,
            runner_id: tokens.runner_id,
        });
        Self {
            board: context.with_scope(TeamScope::one(team_id.clone())),
            clock,
            team_id,
            runners,
            server,
            _solo: solo,
            _shared: shared,
            _data: data,
        }
    }

    fn served(&self, which: Which) -> &Served {
        match which {
            Which::A => &self.runners[0],
            Which::B => &self.runners[1],
        }
    }
}

impl Harness for HttpHarness {
    async fn start_with(term: LeaseTerm) -> Self {
        let data = Self::data_dir();
        let solo = TestContext::over_file(&data.path().join("rimaia.db")).await;
        let context = solo.context.clone();
        let user = solo.solo.user_id.clone();
        // Runner A is the solo runner; runner B a second machine of the same
        // user, paired as a desktop pairs its own (D31 point 13).
        let a = Self::tokens(&context, &user, &solo.solo.runner_id, "Solo runner").await;
        let b = {
            let second = pairing::pair_own_runner(&context, "Runner B", "claude-code")
                .await
                .expect("a second runner");
            let ledger = pairing::pair_own_runner(&context, "Runner B (Ledger)", "ledger")
                .await
                .expect("a Ledger runner");
            Tokens {
                owner: user.clone(),
                runner_id: second.runner_id,
                token: second.token,
                ledger: ledger.token,
            }
        };
        Self::serve(Fixture {
            context,
            clock: solo.clock.clone(),
            team_id: solo.solo.team_id.clone(),
            term,
            tokens: [a, b],
            data,
            solo: Some(solo),
            shared: None,
        })
        .await
    }

    /// `testing::shared`'s two people over a file-backed board, each runner
    /// given its token.
    async fn start_shared() -> Self {
        let data = Self::data_dir();
        let pool = rimaia_core::db::connect(&data.path().join("rimaia.db"))
            .await
            .expect("open a file-backed board");
        rimaia_core::db::migrate(&pool)
            .await
            .expect("migrate a file-backed board");
        let team = SharedTeam::over(pool, TestClock::new(test_epoch())).await;
        let a = Self::tokens(
            &team.alice.ctx,
            &team.alice.user_id,
            &team.alice.runner_id,
            "Alice's laptop",
        )
        .await;
        let b = Self::tokens(
            &team.bob.ctx,
            &team.bob.user_id,
            &team.bob.runner_id,
            "Mac mini",
        )
        .await;
        Self::serve(Fixture {
            context: team.alice.ctx.clone(),
            clock: team.clock.clone(),
            team_id: team.team_id.clone(),
            term: LeaseTerm::Never,
            tokens: [a, b],
            data,
            solo: None,
            shared: Some(team),
        })
        .await
    }

    fn owner(&self, which: Which) -> String {
        self.served(which).owner.clone()
    }

    fn runner_id(&self, which: Which) -> String {
        self.served(which).runner_id.clone()
    }

    fn runner(&self, which: Which) -> Arc<dyn BoardPort> {
        Arc::clone(&self.served(which).port)
    }

    fn runner_on(&self, which: Which, provider: ProviderId) -> Arc<dyn BoardPort> {
        match provider {
            ProviderId::ClaudeCode => self.runner(which),
            ProviderId::Ledger => Arc::clone(&self.served(which).ledger),
        }
    }

    /// A user who signs up and joins the board's team through an invitation
    /// (051), and pairs a runner of their own (047).
    async fn add_member_with_runner(&self) -> (String, String) {
        let member = {
            let mut tx = self.board.pool.begin().await.expect("a transaction");
            let member = create_personal_team(&mut tx, &self.clock, "a-teammate")
                .await
                .expect("the teammate signs up");
            add_member(
                &mut tx,
                &self.clock,
                &self.team_id,
                &member.user_id,
                Role::Member,
            )
            .await;
            tx.commit().await.expect("commit the membership");
            member.user_id
        };
        let as_member = ServiceContext {
            actor: member.clone(),
            ..self.board.clone()
        };
        let runner = pairing::pair_own_runner(&as_member, "Their laptop", "claude-code")
            .await
            .expect("the teammate pairs a runner");
        (member, runner.runner_id)
    }

    fn board(&self) -> &ServiceContext {
        &self.board
    }

    fn clock(&self) -> &TestClock {
        &self.clock
    }
}

rimaia_core::board_contract!(HttpHarness);
