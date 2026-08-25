//! Rebuilds the crate when a migration file changes.
//!
//! `sqlx::migrate!` embeds the corpus with `include_str!`, so rustc's dep-info
//! records the files the macro *found* on the previous build — never the
//! directory it looked in. Without the line below, adding a migration to a
//! directory that was empty would not invalidate anything, and the binary would
//! go on applying the old set with no error anywhere to notice.
//! `proc_macro::tracked_path`, which would let the macro register the directory
//! itself, is nightly-only.
//!
//! Cargo scans a directory named this way for modifications, so an edit to an
//! existing migration counts too — which matters right up until the first
//! release makes them append-only.

fn main() {
    println!("cargo:rerun-if-changed=../../src-tauri/migrations");
}
