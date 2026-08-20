//! Git worktree operations: create, idempotent re-create, remove, reconcile
//! (ADR-0005).
//!
//! One worktree and branch per task, living under the app data directory and
//! never inside the repository. Git is invoked as a subprocess with an argument
//! vector — never `sh -c`, because repository paths contain spaces.
//!
//! Filled in by task 007.
