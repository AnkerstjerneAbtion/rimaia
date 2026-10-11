//! The built web bundle, served at `/` beside the API (ADR-0034 point 6, task
//! 050 Scope 1).
//!
//! `RIMAIA_WEB_ROOT` names the directory `npm run build` writes. This module
//! serves it and decides nothing else about it:
//!
//! - **An unknown path is `404`, not the app.** The app has route state and no
//!   URLs, so there is no deep link to fall back for. A path under `/api/` never
//!   reaches the bundle at all: [`router`] answers it in the error shape.
//! - **`index.html` is `no-cache`; hashed assets are immutable.** Without the
//!   first, the reload an `upgrade_required` asks for would fetch the stale
//!   bundle it is trying to replace. Vite puts a content hash in every name
//!   under `assets/`, so a year is safe.
//! - **Every bundle response carries `nosniff` and `frame-ancestors 'none'`.**
//!   The account page has revoke buttons, and a page that can be framed can
//!   have them clicked for it. They are set with axum's own middleware,
//!   because D34 approves `tower-http`'s `fs`, `cors` and `trace` and not
//!   `set-header`. A full script and style policy is out of scope.

use std::path::Path;

use axum::http::{header, HeaderValue, Uri};
use axum::middleware;
use axum::response::Response;
use axum::Router;
use tower_http::services::ServeDir;

/// The page itself: always revalidated.
const INDEX_CACHE: &str = "no-cache";
/// A file under `assets/`, whose name carries its hash.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
const FRAME_POLICY: &str = "frame-ancestors 'none'";

/// A router serving `root`, to be the fallback of the whole server.
pub fn router<S>(root: &Path) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .fallback_service(ServeDir::new(root))
        .layer(middleware::map_response(headers))
}

async fn headers(uri: Uri, mut response: Response) -> Response {
    let path = uri.path();
    let found = response.status().is_success();
    let cache = if !found {
        None
    } else if path == "/" || path == "/index.html" {
        Some(INDEX_CACHE)
    } else if path.starts_with("/assets/") {
        Some(ASSET_CACHE)
    } else {
        None
    };
    let headers = response.headers_mut();
    if let Some(cache) = cache {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(FRAME_POLICY),
    );
    response
}

/// Why `root` cannot be served, or `None` when it can: a directory holding
/// `index.html`. Checked at startup, so a server never starts and serves a
/// blank page.
pub fn problem_with(root: &Path) -> Option<&'static str> {
    if !root.is_dir() {
        Some("is not a directory")
    } else if !root.join("index.html").is_file() {
        Some("has no index.html")
    } else {
        None
    }
}
