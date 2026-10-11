//! Every rule about a secret, in one place (ADR-0030 point 3, seam-contract
//! D28 and D34).
//!
//! A token, a session secret, a PKCE verifier, an OAuth `state` and a pairing
//! code are all minted here, hashed here and compared here. Nothing else in the
//! codebase calls `rand`, `sha2` or `subtle` for a credential, so "hash at rest,
//! compare in constant time, never print" is implemented once and holds for
//! every kind (ADR-0030 point 3's reason for one token mechanism).
//!
//! The stored form of every secret is [`hash`]: the lowercase hex SHA-256 of
//! the whole string, prefix included. A secret is 256 random bits, so its hash
//! finds the row and teaches nothing about the secret; the one short secret,
//! the pairing code, is single use, lives ten minutes and is rate limited.

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngExt;
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{Error, Result};

/// How many random bytes a token, a session secret and a verifier carry: 256
/// bits, which base64url spells in 43 characters.
const SECRET_BYTES: usize = 32;

/// The domain separator [`csrf_for`] hashes in front of the session secret,
/// so the CSRF token is never the secret's plain hash, which is what the
/// `sessions` row stores.
const CSRF_DOMAIN: &str = "rimaia-csrf-v1:";

/// Crockford's base32 alphabet: no `I`, `L`, `O` or `U`, so a code read off
/// one screen and typed on another machine survives the usual misreadings.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The characters a pairing code has, before its display hyphen.
const PAIRING_CODE_LEN: usize = 8;

/// A value that must never be printed: a token, a session secret, a pairing
/// code, a client secret.
///
/// Its `Debug` is `Secret(…)` and it has no `Display`, so neither a `{:?}` in a
/// log line nor a `format!` in an error message can carry it out by accident.
/// It serializes to its value, because the one response that mints a secret
/// has to deliver it, and that is the only place one is serialized.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret that came from outside, such as a configured client
    /// secret.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that has to send or hash it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// The three kinds of bearer token (ADR-0030 point 3), in the spelling of
/// `api_tokens.kind`'s `CHECK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    Desktop,
    Runner,
    Personal,
}

impl TokenKind {
    pub const ALL: [TokenKind; 3] = [TokenKind::Desktop, TokenKind::Runner, TokenKind::Personal];

    /// The readable prefix that names the kind on the token itself.
    pub const fn prefix(self) -> &'static str {
        match self {
            TokenKind::Desktop => "rmd_",
            TokenKind::Runner => "rmr_",
            TokenKind::Personal => "rmp_",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            TokenKind::Desktop => "desktop",
            TokenKind::Runner => "runner",
            TokenKind::Personal => "personal",
        }
    }

    /// The kind `api_tokens.kind` stores, or `None` for a value its `CHECK`
    /// would not have admitted.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// The kind a presented token's prefix names, or `None` for no prefix of
    /// ours.
    pub fn of_token(token: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| token.starts_with(kind.prefix()))
    }
}

/// 32 bytes from `rand`'s OS-seeded CSPRNG, base64url without padding.
fn random_base64url() -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    rand::rng().fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A new bearer token of `kind`: its prefix, then 256 random bits.
pub fn mint_token(kind: TokenKind) -> Secret {
    Secret(format!("{}{}", kind.prefix(), random_base64url()))
}

/// A new session secret: 256 random bits, with no prefix. Also the OAuth
/// `state` and the desktop's one-time code, which need the same strength and
/// nothing to name them.
pub fn mint_session_secret() -> Secret {
    Secret(random_base64url())
}

/// A new PKCE code verifier: 43 base64url characters, inside RFC 7636's
/// 43 to 128.
pub fn mint_verifier() -> Secret {
    Secret(random_base64url())
}

/// What is stored for `secret`: the lowercase hex SHA-256 of the whole string.
pub fn hash(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// The CSRF token of a session: `BASE64URL_NOPAD(SHA256("rimaia-csrf-v1:" ||
/// secret))`.
///
/// Nothing stores it. The server recomputes it from the cookie on every
/// request, a page can read it from the `rimaia_csrf` cookie to send it back
/// as a header, and knowing it reveals nothing about the secret it came from.
pub fn csrf_for(session_secret: &str) -> String {
    let digest = Sha256::new()
        .chain_update(CSRF_DOMAIN.as_bytes())
        .chain_update(session_secret.as_bytes())
        .finalize();
    URL_SAFE_NO_PAD.encode(digest)
}

/// Whether `header` is the CSRF token of `session_secret`, compared in
/// constant time.
pub fn csrf_matches(session_secret: &str, header: &str) -> bool {
    constant_time_eq(&csrf_for(session_secret), header)
}

/// PKCE's S256 challenge of `verifier`: `BASE64URL_NOPAD(SHA256(verifier))`.
/// This is the whole of hand-rolled PKCE (D34).
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Whether two secrets are equal, in time that depends on their lengths and
/// not on where they first differ. For every secret compared in memory: the
/// CSRF header, the OAuth `state` against its cookie, and the desktop's PKCE
/// challenge.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// A new pairing code: eight characters drawn uniformly from Crockford's
/// base32 alphabet (40 bits), displayed as `XXXX-XXXX`.
pub fn mint_pairing_code() -> Secret {
    let mut rng = rand::rng();
    let mut code = String::with_capacity(PAIRING_CODE_LEN + 1);
    for position in 0..PAIRING_CODE_LEN {
        if position == PAIRING_CODE_LEN / 2 {
            code.push('-');
        }
        code.push(char::from(CROCKFORD[rng.random_range(0..CROCKFORD.len())]));
    }
    Secret(code)
}

/// A typed pairing code as it is hashed: uppercase, without hyphens or spaces,
/// with `O` read as `0` and `I` and `L` as `1`, the mistakes Crockford's
/// alphabet exists to forgive.
pub fn normalize_pairing_code(input: &str) -> String {
    input
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}

/// Whether `code` has a pairing code's shape: `XXXX-XXXX` over Crockford's
/// alphabet. For a test that checks a minted code without knowing it.
pub fn is_pairing_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == PAIRING_CODE_LEN + 1
        && bytes.iter().enumerate().all(|(at, byte)| {
            if at == PAIRING_CODE_LEN / 2 {
                *byte == b'-'
            } else {
                CROCKFORD.contains(byte)
            }
        })
}

/// Whether `token` has the shape of a token of `kind`: its prefix, then 43
/// base64url characters.
pub fn is_token_of(kind: TokenKind, token: &str) -> bool {
    token.strip_prefix(kind.prefix()).is_some_and(|rest| {
        rest.len() == 43
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    })
}

/// Text from a request header, cut to at most 256 characters, for the
/// `user_agent` and `last_used_from` columns: enough to tell two browsers
/// apart, and a bound on what a client can make the server store.
pub fn truncate_user_agent(user_agent: Option<&str>) -> Option<String> {
    user_agent
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .map(|agent| agent.chars().take(256).collect())
}

/// `label`, trimmed, if it is 1 to 100 characters: the rule every token and
/// runner label is held to (ADR-0030 points 3 and 5).
pub fn label(label: &str) -> Result<String> {
    let trimmed = label.trim();
    let length = trimmed.chars().count();
    if (1..=100).contains(&length) {
        Ok(trimmed.to_string())
    } else {
        Err(Error::invalid("A label is 1 to 100 characters."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_token_carries_the_prefix_of_its_kind_and_256_bits_after_it() {
        for kind in TokenKind::ALL {
            let token = mint_token(kind);
            let rest = token
                .expose()
                .strip_prefix(kind.prefix())
                .expect("the kind's prefix");

            let bits = URL_SAFE_NO_PAD.decode(rest).expect("base64url").len() * 8;
            assert_eq!(bits, 256, "{kind:?}");
            assert!(is_token_of(kind, token.expose()), "{kind:?}");
            assert_eq!(TokenKind::of_token(token.expose()), Some(kind));
        }
        assert_ne!(mint_token(TokenKind::Personal), mint_token(TokenKind::Personal));
        assert_eq!(mint_session_secret().expose().len(), 43);
    }

    #[test]
    fn a_stored_hash_is_the_hex_sha256_of_the_whole_token() {
        // `printf 'rmp_abc' | shasum -a 256`: the prefix is hashed with the rest.
        assert_eq!(
            hash("rmp_abc"),
            "ad23c440482b50b25f93dac79b8c6673afd8ff05511e689229033e95785b41f6"
        );
        assert_ne!(hash("rmp_abc"), hash("abc"));

        let token = mint_token(TokenKind::Runner);
        let stored = hash(token.expose());
        assert_eq!(stored.len(), 64);
        assert!(stored
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    }

    #[test]
    fn the_pkce_challenge_matches_rfc_7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let verifier = mint_verifier();
        assert!((43..=128).contains(&verifier.expose().len()));
    }

    #[test]
    fn the_csrf_token_is_derived_from_the_session_secret_and_is_neither_it_nor_its_hash() {
        let secret = mint_session_secret();
        let csrf = csrf_for(secret.expose());

        assert_eq!(csrf, csrf_for(secret.expose()), "derived, not random");
        assert_ne!(csrf, secret.expose());
        assert_ne!(csrf, hash(secret.expose()));
        assert_ne!(csrf, URL_SAFE_NO_PAD.encode(Sha256::digest(secret.expose())));
        assert_ne!(csrf, csrf_for(mint_session_secret().expose()));
        assert!(csrf_matches(secret.expose(), &csrf));
        assert!(!csrf_matches(secret.expose(), "a-wrong-header"));
        assert!(!csrf_matches(secret.expose(), ""));
    }

    #[test]
    fn a_typed_pairing_code_is_forgiven_case_hyphens_and_lookalikes() {
        let minted = mint_pairing_code();
        assert!(is_pairing_code(minted.expose()), "{}", minted.expose());

        assert_eq!(normalize_pairing_code("AB1C-0DEF"), "AB1C0DEF");
        assert_eq!(normalize_pairing_code("ab1c-0def"), "AB1C0DEF");
        assert_eq!(normalize_pairing_code(" abIc 0def "), "AB1C0DEF");
        assert_eq!(normalize_pairing_code("abLc-Odef"), "AB1C0DEF");
        assert_eq!(normalize_pairing_code("ablc-odef"), "AB1C0DEF");
        assert_eq!(
            normalize_pairing_code(minted.expose()),
            minted.expose().replace('-', "")
        );
    }

    #[test]
    fn a_secret_prints_as_a_placeholder() {
        let secret = mint_token(TokenKind::Desktop);
        let printed = format!("{secret:?}");

        assert_eq!(printed, "Secret(…)");
        assert_eq!(
            serde_json::to_value(&secret).expect("serialize"),
            serde_json::Value::String(secret.expose().to_string())
        );
    }

    #[test]
    fn a_label_is_trimmed_and_held_to_one_to_a_hundred_characters() {
        assert_eq!(label("  My laptop ").expect("valid"), "My laptop");
        assert_eq!(label(&"é".repeat(100)).expect("100 characters").chars().count(), 100);
        for refused in ["", "   ", &"x".repeat(101)] {
            let error = label(refused).expect_err("out of range");
            assert_eq!(error.to_string(), "A label is 1 to 100 characters.");
        }
    }

    #[test]
    fn a_user_agent_is_cut_to_256_characters() {
        assert_eq!(truncate_user_agent(None), None);
        assert_eq!(truncate_user_agent(Some("  ")), None);
        assert_eq!(
            truncate_user_agent(Some(&"a".repeat(300))).map(|agent| agent.len()),
            Some(256)
        );
    }
}
