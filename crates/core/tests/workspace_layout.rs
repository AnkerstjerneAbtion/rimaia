//! What the workspace's layout promises, checked where `cargo test -p
//! rimaia-core` runs it, so CI needs no step CLAUDE.md lacks (ADR-0027 point
//! 6).

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
