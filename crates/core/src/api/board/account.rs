//! The account commands: the caller's own user, sessions and tokens (task 047
//! Scope 12, ADR-0030 points 2, 3, 5 and 6).
//!
//! They are the caller's own, so another user's session or token id is
//! `not_found`, and they read the door in core, as D32 point 7 has an account
//! command do: `current` is computed from `request.caller.door`, and only a
//! connected desktop pairs its own runner. They publish no `ChangeEvent`:
//! sessions and tokens belong to a user, not a team, and a second open tab
//! sees a revocation on its next read.
//!
//! **Solo has no accounts** (ADR-0030 point 7). The rows exist there because
//! the registry is one list, and a `Door::Shell` caller is refused.

use serde::Deserialize;

use super::{ById, NoArgs};
use crate::api::{BoardRequest, Door};
use crate::error::{Error, Result};
use crate::identity::account::{self, Account, CurrentCredential, CurrentKind};
use crate::identity::pairing::{self, PairedRunner, PairingCode};
use crate::identity::rate_limit::Bucket;
use crate::identity::sessions::{self, SessionSummary};
use crate::identity::tokens::{self, ApiTokenSummary, NewApiToken};

/// What the solo shell is told, for every account command.
pub const SOLO_HAS_NO_ACCOUNTS: &str = "Accounts exist only on a Rimaia server.";

/// The credential the request came through, or the refusal for a door that
/// has no account to manage: the solo shell, and the two doors that cannot
/// reach the board surface at all (D32 point 7's table).
fn current(request: &BoardRequest) -> Result<CurrentCredential> {
    match &request.caller.door {
        Door::Shell => Err(Error::invalid(SOLO_HAS_NO_ACCOUNTS)),
        Door::Browser { session_id } => Ok(CurrentCredential {
            kind: CurrentKind::Session,
            id: session_id.clone(),
        }),
        Door::Desktop { token_id } => Ok(CurrentCredential {
            kind: CurrentKind::Desktop,
            id: token_id.clone(),
        }),
        Door::Mcp { .. } | Door::Runner { .. } => Err(Error::invalid(
            "An account is managed from the Rimaia web app or the desktop app.",
        )),
    }
}

/// [`current`]'s id, when it is of `kind`.
fn current_of(request: &BoardRequest, kind: CurrentKind) -> Result<Option<String>> {
    let current = current(request)?;
    Ok((current.kind == kind).then_some(current.id))
}

/// Counts one mint against the caller's hourly limit.
fn count_mint(request: &BoardRequest) -> Result<()> {
    request
        .host
        .rate_limits
        .check(Bucket::MintToken, &request.caller.user_id)
}

/// The user, their teams with name and role, and the credential this request
/// used.
pub async fn get_account(request: BoardRequest, _args: NoArgs) -> Result<Account> {
    let current = current(&request)?;
    account::get(&request.ctx, current).await
}

/// The caller's live sessions.
pub async fn list_sessions(request: BoardRequest, _args: NoArgs) -> Result<Vec<SessionSummary>> {
    let current = current_of(&request, CurrentKind::Session)?;
    sessions::list(&request.ctx, current.as_deref()).await
}

/// Deletes one of the caller's sessions. With `get_account`'s `current.id`
/// this is signing out.
pub async fn revoke_session(request: BoardRequest, args: ById) -> Result<()> {
    current(&request)?;
    sessions::revoke(&request.ctx, &args.id).await
}

/// The caller's tokens, never a hash.
pub async fn list_api_tokens(request: BoardRequest, _args: NoArgs) -> Result<Vec<ApiTokenSummary>> {
    let current = current_of(&request, CurrentKind::Desktop)?;
    tokens::list(&request.ctx, current.as_deref()).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePersonalAccessToken {
    pub label: String,
    #[serde(default)]
    pub team_ids: Vec<String>,
    pub expires_in_days: Option<i64>,
}

/// A personal access token, the one time its secret is shown.
pub async fn create_personal_access_token(
    request: BoardRequest,
    args: CreatePersonalAccessToken,
) -> Result<NewApiToken> {
    current(&request)?;
    count_mint(&request)?;
    tokens::create_personal(
        &request.ctx,
        &args.label,
        &args.team_ids,
        args.expires_in_days,
    )
    .await
}

/// Revokes one of the caller's tokens; a runner token unpairs its runner.
pub async fn revoke_api_token(request: BoardRequest, args: ById) -> Result<()> {
    current(&request)?;
    tokens::revoke(&request.ctx, &args.id).await
}

/// A ten-minute, single-use code a headless runner pairs with.
pub async fn create_pairing_code(request: BoardRequest, _args: NoArgs) -> Result<PairingCode> {
    current(&request)?;
    count_mint(&request)?;
    pairing::create_code(&request.ctx).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairOwnRunner {
    pub label: String,
    pub provider: String,
}

/// The connected desktop's own runner, paired with no code. Only a desktop
/// may ask: a browser has no machine of its own to pair.
pub async fn pair_own_runner(request: BoardRequest, args: PairOwnRunner) -> Result<PairedRunner> {
    if current(&request)?.kind != CurrentKind::Desktop {
        return Err(Error::invalid(
            "Only the Rimaia desktop app pairs its own runner. To pair another machine, \
             create a pairing code.",
        ));
    }
    count_mint(&request)?;
    pairing::pair_own_runner(&request.ctx, &args.label, &args.provider).await
}
