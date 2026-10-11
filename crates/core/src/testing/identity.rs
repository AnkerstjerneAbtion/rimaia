//! An identity provider the test fills (task 047).
//!
//! [`FakeIdentityProvider`] stands where GitHub stands in production, behind
//! the same [`IdentityProvider`] seam: its authorize URL names the `state` and
//! the challenge it was given, and its exchange answers from a table of codes
//! the test fills. It records every verifier it is handed, so a test can check
//! PKCE end to end: the verifier the server kept is the one whose challenge it
//! sent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::api::BoxFuture;
use crate::error::{Error, Result};
use crate::identity::provider::{IdentityProvider, ProviderIdentity};

/// The fake's `users.identity_provider`.
pub const FAKE_PROVIDER_ID: &str = "fake";

/// What one call to `exchange` was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub code: String,
    pub verifier: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Default)]
pub struct FakeIdentityProvider {
    codes: Arc<Mutex<HashMap<String, ProviderIdentity>>>,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
}

impl FakeIdentityProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// From now on, `code` exchanges for `identity`.
    pub fn answer(&self, code: &str, identity: ProviderIdentity) {
        self.codes
            .lock()
            .expect("the code table")
            .insert(code.to_string(), identity);
    }

    /// Every exchange, in order.
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.exchanges.lock().expect("the exchanges").clone()
    }

    /// Every verifier the exchanges were given, in order.
    pub fn verifiers(&self) -> Vec<String> {
        self.exchanges()
            .into_iter()
            .map(|exchange| exchange.verifier)
            .collect()
    }
}

impl IdentityProvider for FakeIdentityProvider {
    fn id(&self) -> &'static str {
        FAKE_PROVIDER_ID
    }

    fn authorize_url(&self, state: &str, code_challenge: &str, _redirect_uri: &str) -> String {
        format!("https://idp.test/authorize?state={state}&challenge={code_challenge}")
    }

    fn exchange<'a>(
        &'a self,
        code: &'a str,
        code_verifier: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<ProviderIdentity>> {
        self.exchanges
            .lock()
            .expect("the exchanges")
            .push(Exchange {
                code: code.to_string(),
                verifier: code_verifier.to_string(),
                redirect_uri: redirect_uri.to_string(),
            });
        let answer = self
            .codes
            .lock()
            .expect("the code table")
            .get(code)
            .cloned()
            .ok_or_else(|| Error::internal("the fake identity provider knows no such code"));
        Box::pin(async move { answer })
    }
}
