//! Claude Code process supervision: spawning the headless CLI, parsing its
//! `stream-json` events, and classifying how a run ended (ADR-0004, ADR-0011).
//!
//! The CLI is a prerequisite, never bundled. Parsing is tolerant by rule —
//! unknown event types are persisted and ignored, never fatal, so a Claude Code
//! update cannot break an overnight queue. Outcome is classified on the `result`
//! event's `terminal_reason` and `subtype`, not on the exit code alone: a
//! SIGTERM-killed run still emits a `result` and exits 143.
//!
//! Filled in by task 008, against the recorded fixtures task 019 promotes.
//! [`prompt`] landed first (task 006): task 008 needs a prompt to send, and
//! composing it is unit-testable without spawning anything.

pub mod prompt;
