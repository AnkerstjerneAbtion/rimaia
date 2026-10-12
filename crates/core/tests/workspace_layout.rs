//! What the workspace's layout promises, checked where `cargo test -p
//! rimaia-core` runs it, so CI needs no step CLAUDE.md lacks (ADR-0027 point
//! 6, seam-contract D33).

use std::path::Path;

/// `rimaia-runner` depends on `rimaia-core`, never the other way round.
///
/// Cargo refuses a cycle through `[dependencies]`, but it allows one through
/// `[dev-dependencies]`, which is exactly how this crate depends on itself
/// today. So the compiler alone does not keep the arrow pointing one way, and
/// this reads the manifest for either spelling of the runner, in any table.
#[test]
fn rimaia_core_does_not_depend_on_rimaia_runner() {
    let manifest = include_str!("../Cargo.toml");

    for name in ["rimaia-runner", "rimaia_runner", "crates/runner"] {
        let lines: Vec<&str> = manifest
            .lines()
            .filter(|line| line.contains(name))
            .collect();
        assert!(
            lines.is_empty(),
            "crates/core/Cargo.toml names {name}: {lines:?}"
        );
    }
}

/// The macros fall back to `<workspace root>/.sqlx` for any query missing from
/// a crate's own cache, and can answer from the other schema there. A copy at
/// the root is what the old `cargo sqlx prepare --workspace` leaves behind even
/// when it fails, so this turns that silent fallback into a failure (D33
/// point 1).
#[test]
fn no_offline_query_cache_at_the_workspace_root() {
    let root_cache = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.sqlx");

    assert!(
        !root_cache.exists(),
        "{} exists; each crate's cache lives in its own directory (seam-contract D33)",
        root_cache.display()
    );
}
