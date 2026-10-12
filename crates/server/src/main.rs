//! `rimaia-server`: the board over HTTP (ADR-0027 point 6).
//!
//! Reads its five variables ([`config`](rimaia_server::config)), opens and
//! migrates `<RIMAIA_DATA_DIR>/rimaia.db`, and serves the board and sign-in
//! routes, authenticated by sessions and hashed tokens and signing people in
//! with GitHub (task 047). A missing or invalid variable, a failed migration or
//! a failed bind exits non-zero with a line naming what failed, and never the
//! client secret. It never creates a solo identity: a server board is not solo. It
//! installs no tracing subscriber, because no crate for one is approved yet
//! (task 062's).

use std::process::ExitCode;
use std::sync::Arc;

use rimaia_core::api::tail::SERVER_TAIL_RELAY_CAPACITY;
use rimaia_core::api::{BoardHost, StreamControl, TailRelay};
use rimaia_core::board::RelayedRequests;
use rimaia_core::db::{self, MutationSource};
use rimaia_core::identity::authenticate::SessionsAndTokens;
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::runner::provider::{ClaudeProvider, ProviderProfile};
use rimaia_core::{ServiceContext, SystemClock, TeamScope};
use rimaia_server::config::{config_from_env, Config};
use rimaia_server::github::{GitHub, GitHubEndpoints};
use rimaia_server::{router_with, RouterOptions, ServerState};

#[tokio::main]
async fn main() -> ExitCode {
    match serve().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("rimaia-server: {failure}");
            ExitCode::FAILURE
        }
    }
}

async fn serve() -> Result<(), String> {
    let Config {
        paths,
        listen,
        public_url,
        github_client_id,
        github_client_secret,
        web_root,
    } = config_from_env(|name| std::env::var_os(name)).map_err(|error| error.to_string())?;
    let github = GitHub::new(
        github_client_id,
        github_client_secret,
        GitHubEndpoints::github(),
    )
    .map_err(|error| error.to_string())?;

    let db_file = paths.db_file();
    std::fs::create_dir_all(paths.data_dir()).map_err(|error| {
        format!(
            "could not create the data directory {}: {error}",
            paths.data_dir().display()
        )
    })?;
    let pool = db::connect(&db_file)
        .await
        .map_err(|error| format!("could not open the database {}: {error}", db_file.display()))?;
    db::migrate(&pool).await.map_err(|error| {
        format!(
            "could not apply the migrations to {}: {error}",
            db_file.display()
        )
    })?;

    // `dispatch` re-scopes every request from its caller, so the base context
    // reaches nothing: its one team and its actor are ids no row holds.
    let context = ServiceContext::new(
        pool,
        Arc::new(SystemClock),
        MutationSource::Ui,
        TeamScope::one(db::new_id()),
        db::new_id(),
    );
    let auth = SessionsAndTokens {
        pool: context.pool.clone(),
        clock: context.clock.clone(),
    };
    // One limiter, which the board host hands the minting commands and the
    // state hands the sign-in routes.
    let rate_limits = Arc::new(RateLimiter::new(context.clock.clone()));
    // The relay follows the live channel from here, before anything can
    // publish to it (task 048).
    let tails = TailRelay::new(SERVER_TAIL_RELAY_CAPACITY);
    tokio::spawn(tails.follow(context.subscribe_tail()));
    // The Run nows and Cancels held for runners (task 052): one value, which
    // the board commands record into and the runner routes collect from.
    let relayed = Arc::new(RelayedRequests::new());
    let host = BoardHost {
        context,
        provider: ProviderProfile::of(&ClaudeProvider),
        paths,
        rate_limits,
        tails,
        streams: StreamControl::new(),
        relay: relayed.clone(),
    };
    // Leases granted over HTTP expire unless renewed: `ServerState::new`'s
    // `LeaseTerm::Renewable(LEASE_LIFETIME)`.
    let state = ServerState::new(host, relayed, Arc::new(auth), Arc::new(github), public_url);

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| format!("could not listen on {listen}: {error}"))?;
    let options = RouterOptions {
        dev: cfg!(debug_assertions),
        web_root,
    };
    rimaia_server::serve(listener, router_with(state, &options))
        .await
        .map_err(|error| format!("stopped serving on {listen}: {error}"))
}
