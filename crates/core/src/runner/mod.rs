//! Claude Code process supervision: spawning the headless CLI, parsing its
//! `stream-json` events, and classifying how a run ended (ADR-0004, ADR-0011).
//!
//! The CLI is a prerequisite, never bundled. Parsing is tolerant by rule —
//! unknown event types are persisted and ignored, never fatal, so a Claude Code
//! update cannot break an overnight queue. Outcome is classified on the `result`
//! event's `terminal_reason` and `subtype`, not on the exit code alone: a
//! SIGTERM-killed run still emits a `result` and exits 143.
//!
//! [`prompt`] landed first (task 006): task 008 needs a prompt to send, and
//! composing it is unit-testable without spawning anything.
//!
//! # The three stages, and why they are separate modules
//!
//! [`events`] knows what a *line* means, [`outcome`] knows what an *ending*
//! means, and neither has ever seen a process — which is what makes every
//! recorded scenario in `tests/fixtures/cli/` replayable without spawning
//! anything or spending a token (ADR-0015). [`process`] is the one place a real
//! child exists, and it is the only one of the three that a test cannot exercise
//! by replaying bytes alone.
//!
//! [`run_task`] is the entry point that ties them together: one task in, one
//! finished `runs` row out.

pub mod events;
pub mod outcome;
pub mod process;
pub mod prompt;
pub mod strategy;

pub use process::{
    execute, probe_cli, run_task, Attempt, CancelSignal, Invocation, PermissionMode, RunRequest,
    RunTrigger, RunnerConfig,
};
pub use strategy::{Resolution, STRATEGY_TRANSCRIPT_PREFIX};
