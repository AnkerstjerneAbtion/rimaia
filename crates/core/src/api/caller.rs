//! Who is asking, and through which door (seam-contract D32 point 7, ADR-0030).
//!
//! Every board request carries a [`Caller`] from the first route onward. The
//! solo shell builds one in process with [`Caller::solo`]; a server resolves one
//! from a credential through [`Authenticate`]. Turning a caller into the
//! context a service runs under is [`ServiceContext::for_caller`]'s, and only
//! its.
//!
//! [`ServiceContext::for_caller`]: crate::ServiceContext::for_caller

use std::fmt;

use super::BoxFuture;
use crate::db::MutationSource;
use crate::error::{Error, Result};
use crate::events::{TeamId, UserId};
use crate::identity::{Role, SoloIdentity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Who: `users.id` (ADR-0030 point 8). Solo: the installation's solo user
    /// (D28).
    pub user_id: UserId,
    /// Which teams, with the caller's role in each. Read from
    /// `team_memberships` on this request, never cached on a session or a
    /// token (ADR-0030 point 2).
    pub teams: Vec<TeamGrant>,
    /// Which door the request came through.
    pub door: Door,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamGrant {
    pub team_id: TeamId,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Door {
    /// The solo desktop's own window, through `invoke`. No credential
    /// (ADR-0030 point 7).
    Shell,
    /// A browser session: the `sessions` row id, never the cookie value.
    Browser { session_id: String },
    /// A connected desktop's `rmd_` token: the token row id, never the secret.
    Desktop { token_id: String },
    /// An `rmp_` token on the hosted `/mcp`; `None` on the solo loopback
    /// endpoint.
    Mcp { token_id: Option<String> },
    /// A paired runner's `rmr_` token.
    Runner { runner_id: String },
}

impl Door {
    /// ADR-0019's three sources. `tasks.source` keeps them; the finer door
    /// goes to the span.
    pub fn source(&self) -> MutationSource {
        match self {
            Door::Shell | Door::Browser { .. } | Door::Desktop { .. } => MutationSource::Ui,
            Door::Mcp { .. } => MutationSource::Mcp,
            Door::Runner { .. } => MutationSource::System,
        }
    }

    /// The variant's name, which is all of a door a span records: the ids
    /// inside are a session's or a token's row, not something a log needs.
    pub fn name(&self) -> &'static str {
        match self {
            Door::Shell => "shell",
            Door::Browser { .. } => "browser",
            Door::Desktop { .. } => "desktop",
            Door::Mcp { .. } => "mcp",
            Door::Runner { .. } => "runner",
        }
    }
}

impl Caller {
    /// The solo shell's caller: the installation's own user, owning its own
    /// team, through the window (D32 point 7, D28 part 3).
    pub fn solo(identity: &SoloIdentity) -> Self {
        Self {
            user_id: identity.user_id.clone(),
            teams: vec![TeamGrant {
                team_id: identity.team_id.clone(),
                role: Role::Owner,
            }],
            door: Door::Shell,
        }
    }
}

/// What a request presented, before anything has checked it.
///
/// Borrowed out of the request, so a secret is never copied into anything that
/// outlives it, and printed redacted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Credential<'a> {
    /// The `rimaia_session` cookie's value, whose hash alone is stored (D28),
    /// and the `X-Rimaia-CSRF` header, which a session carries on every
    /// request (D32 point 7).
    Session {
        secret: &'a str,
        csrf: Option<&'a str>,
    },
    /// `Authorization: Bearer <token>`.
    Bearer(&'a str),
}

impl fmt::Debug for Credential<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credential::Session { csrf, .. } => f
                .debug_struct("Session")
                .field("secret", &"<redacted>")
                .field("csrf", &csrf.map(|_| "<redacted>"))
                .finish(),
            Credential::Bearer(_) => f.debug_tuple("Bearer").field(&"<redacted>").finish(),
        }
    }
}

/// Resolves a credential to a caller, or refuses it as `Unauthenticated`.
///
/// One method, so a server's whole notion of who may ask is one value it can
/// swap: [`RefuseAll`] until task 047, sessions and hashed tokens after.
pub trait Authenticate: Send + Sync {
    fn authenticate<'a>(&'a self, credential: Credential<'a>) -> BoxFuture<'a, Result<Caller>>;
}

/// The only production [`Authenticate`] before task 047: nobody.
///
/// A server built in between therefore answers every board command
/// `unauthenticated`, and fails closed rather than serving a board to anyone
/// (D32 point 7).
#[derive(Debug, Clone, Copy, Default)]
pub struct RefuseAll;

impl Authenticate for RefuseAll {
    fn authenticate<'a>(&'a self, _credential: Credential<'a>) -> BoxFuture<'a, Result<Caller>> {
        Box::pin(async { Err(Error::unauthenticated()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ensure_solo;
    use crate::testing::{test_epoch, test_pool, TestClock};
    use crate::ErrorCode;
    use pretty_assertions::assert_eq;

    #[test]
    fn every_door_maps_to_one_of_adr_0019s_three_sources() {
        let doors = [
            (Door::Shell, MutationSource::Ui),
            (
                Door::Browser {
                    session_id: "session".to_string(),
                },
                MutationSource::Ui,
            ),
            (
                Door::Desktop {
                    token_id: "token".to_string(),
                },
                MutationSource::Ui,
            ),
            (Door::Mcp { token_id: None }, MutationSource::Mcp),
            (
                Door::Mcp {
                    token_id: Some("token".to_string()),
                },
                MutationSource::Mcp,
            ),
            (
                Door::Runner {
                    runner_id: "runner".to_string(),
                },
                MutationSource::System,
            ),
        ];

        for (door, source) in doors {
            assert_eq!(door.source(), source, "{door:?}");
        }
    }

    #[tokio::test]
    async fn the_solo_caller_owns_the_solo_team_through_the_shell() {
        let pool = test_pool().await;
        let solo = ensure_solo(&pool, &TestClock::new(test_epoch()))
            .await
            .expect("a fresh board gets a solo identity");

        assert_eq!(
            Caller::solo(&solo),
            Caller {
                user_id: solo.user_id.clone(),
                teams: vec![TeamGrant {
                    team_id: solo.team_id.clone(),
                    role: Role::Owner,
                }],
                door: Door::Shell,
            }
        );
    }

    #[tokio::test]
    async fn refuse_all_refuses_every_credential() {
        let credentials = [
            Credential::Session {
                secret: "a-session",
                csrf: Some("a-csrf-token"),
            },
            Credential::Bearer("rmd_a-token"),
        ];

        for credential in credentials {
            let refused = RefuseAll
                .authenticate(credential)
                .await
                .expect_err("nobody authenticates before task 047");
            assert_eq!(refused.code(), ErrorCode::Unauthenticated, "{credential:?}");
        }
    }

    #[test]
    fn a_credential_never_prints_its_secret() {
        let printed = format!(
            "{:?} {:?}",
            Credential::Session {
                secret: "cookie-secret",
                csrf: Some("csrf-secret"),
            },
            Credential::Bearer("bearer-secret"),
        );

        for secret in ["cookie-secret", "csrf-secret", "bearer-secret"] {
            assert!(!printed.contains(secret), "{printed}");
        }
    }
}
