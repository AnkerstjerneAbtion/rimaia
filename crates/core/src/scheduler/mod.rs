//! The run queue: claiming the next task, sequential and parallel execution
//! modes, run windows, retry and backoff (ADR-0010, ADR-0011).
//!
//! Everything here that waits or times out does so against an injected
//! [`crate::Clock`], so its tests are instant and never sleep (ADR-0015).
//!
//! Filled in by task 009.
