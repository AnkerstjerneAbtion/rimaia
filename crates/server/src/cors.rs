//! Cross-origin calls, from the Tauri origins and with a bearer token only
//! (ADR-0034 point 6, task 050 Scope 2).
//!
//! A connected desktop's webview is served from its own origin, so its calls to
//! a server are cross-origin. This module is the whole of what is allowed:
//!
//! - **The origins are listed exactly**: never a predicate and never `Any`.
//! - **`allow_credentials` is never set**, so a browser never sends the
//!   `rimaia_session` cookie cross-origin and a Tauri origin can authenticate
//!   only with a bearer token. The cookie and `X-Rimaia-CSRF` are same-origin
//!   only, which is why `x-rimaia-csrf` is not an allowed request header.
//! - It wraps `/api/v1/*`, the event stream included, and nothing else. The
//!   bundle is not an API, and `/api/v1/runner/*` (task 052) is called by
//!   runners, which are not browsers, and is mounted outside this layer.

use std::time::Duration;

use axum::http::{header, HeaderName, HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// The origins Tauri 2's webview uses: `tauri://localhost` on macOS and Linux,
/// and on Windows `http://tauri.localhost`, or `https://tauri.localhost` with
/// `useHttpsScheme`.
const TAURI_ORIGINS: [&str; 3] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

/// `devUrl` in `src-tauri/tauri.conf.json`, so a connected `npm run tauri dev`
/// (task 059) can reach a local server.
const DEV_ORIGIN: &str = "http://localhost:1420";

/// How long a browser may reuse a preflight's answer.
const PREFLIGHT_MAX_AGE: Duration = Duration::from_secs(10 * 60);

/// The origins allowed to call the API. `dev` adds the Vite origin; the binary
/// passes `cfg!(debug_assertions)`, so a release build never allows it, and a
/// test passes either, because that is always true under `cargo test`.
pub fn allowed_origins(dev: bool) -> Vec<&'static str> {
    let mut origins = TAURI_ORIGINS.to_vec();
    if dev {
        origins.push(DEV_ORIGIN);
    }
    origins
}

/// The CORS layer over [`allowed_origins`].
pub fn layer(dev: bool) -> CorsLayer {
    let origins = allowed_origins(dev)
        .into_iter()
        .map(HeaderValue::from_static)
        .collect::<Vec<_>>();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header_name(rimaia_core::api::protocol::HEADER),
            header_name(crate::caller::TEAM_HEADER),
        ])
        .max_age(PREFLIGHT_MAX_AGE)
}

/// A header name from one of this crate's constants, which may be spelled with
/// capitals as the protocol writes them: `from_bytes` lower-cases, where
/// `from_static` would panic.
fn header_name(name: &str) -> HeaderName {
    HeaderName::from_bytes(name.as_bytes()).expect("a header name this crate defines is valid")
}
