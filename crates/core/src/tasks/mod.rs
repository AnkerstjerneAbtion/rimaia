//! The task service layer: CRUD, board columns, ordering, run-state
//! transitions, and the dependency graph (ADR-0007, ADR-0008).
//!
//! `position` is a fractional float and ordering *is* the priority mechanism —
//! there is no separate priority field. A dependency is satisfied when its run
//! succeeds, not when a human marks it done; that is deliberate and
//! load-bearing.
//!
//! Every function here takes `&SqlitePool` and no `AppHandle`, so the MCP server
//! (ADR-0006) enforces the same invariants as the UI by calling the same code.
//!
//! Filled in by task 004.
