//! Registered local git repositories: registration, validation, and read-only
//! git inspection (default branch, remote, dirty state).
//!
//! Repository state on disk is authoritative — the database records paths and
//! branches, and startup reconciles rows against reality rather than trusting
//! them (ADR-0005).
//!
//! Filled in by task 003.
