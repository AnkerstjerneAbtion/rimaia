//! The identity-provider seam (ADR-0030 point 1).
//!
//! Signing in is three things a provider answers: where to send the browser,
//! how to turn the code it comes back with into a person, and that person's
//! stable subject. Only GitHub ships, and its implementation lives in
//! `rimaia-server`, because it needs HTTPS and D34 gives reqwest's TLS feature
//! to the server, not to core, whose `reqwest` stays plain HTTP to `127.0.0.1`.
//! A second provider is another implementation of this trait.

use crate::api::BoxFuture;
use crate::error::Result;

pub trait IdentityProvider: Send + Sync + 'static {
    /// `users.identity_provider`: "github".
    fn id(&self) -> &'static str;

    /// Where the browser is sent to sign in, carrying the OAuth `state`, the
    /// PKCE S256 challenge and the callback the provider returns to.
    fn authorize_url(&self, state: &str, code_challenge: &str, redirect_uri: &str) -> String;

    /// Exchanges the code, reads the stable subject, and drops the provider's
    /// access token before returning. Nothing the provider issues is stored or
    /// returned.
    fn exchange<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<ProviderIdentity>>;
}

/// Who signed in, as the provider says: the subject keys the `users` row, the
/// login and avatar are for display and change when the person renames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIdentity {
    /// The provider's stable id, never the login, which can be renamed.
    pub subject: String,
    pub login: String,
    pub avatar_url: Option<String>,
}
