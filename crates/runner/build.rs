//! Rebuilds the crate when a runner migration file changes.
//!
//! The same reason as `crates/core/build.rs`: `sqlx::migrate!` embeds the
//! files it found on the previous build, never the directory it looked in, so
//! without this an added migration would not invalidate anything and the
//! store would go on applying the old set.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
