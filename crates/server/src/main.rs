//! `rimaia-server`: the board over HTTP (ADR-0027 point 6).
//!
//! Reads its two variables ([`config`](rimaia_server::config)), opens and
//! migrates `<RIMAIA_DATA_DIR>/rimaia.db`, and serves the board routes with
//! [`RefuseAll`] until task 047 lands sign-in. A missing or invalid variable, a
//! failed migration or a failed bind exits non-zero with a line naming what
//! failed. It never creates a solo identity: a server board is not solo. It
//! installs no tracing subscriber, because no crate for one is approved yet
//! (task 062's).

use std::process::ExitCode;
use std::sync::Arc;

use rimaia_core::api::{BoardHost, RefuseAll};
use rimaia_core::db::{self, MutationSource};
use rimaia_core::identity::rate_limit::RateLimiter;
use rimaia_core::runner::provider::{ClaudeProvider, ProviderProfile};
use rimaia_core::{ServiceContext, SystemClock, TeamScope};
use rimaia_server::config::{config_from_env, Config};
use rimaia_server::{router, ServerState};

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
    let Config { paths, listen } =
        config_from_env(|name| std::env::var_os(name)).map_err(|error| error.to_string())?;

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
    let rate_limits = Arc::new(RateLimiter::new(context.clock.clone()));
    let state = ServerState {
        host: BoardHost {
            context,
            provider: ProviderProfile::of(&ClaudeProvider),
            paths,
            rate_limits,
        },
        auth: Arc::new(RefuseAll),
    };

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| format!("could not listen on {listen}: {error}"))?;
    axum::serve(listener, router(state))
        .await
        .map_err(|error| format!("stopped serving on {listen}: {error}"))
}
