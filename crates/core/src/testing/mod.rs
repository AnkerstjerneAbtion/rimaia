//! The test harness every later task builds on (ADR-0015, task 019).
//!
//! Four helpers, one per thing that is otherwise slow, flaky or unavailable in a
//! test: a controllable [`clock`], a real git repository in a temporary
//! directory ([`repo`]), a migrated in-memory database ([`db`]), and access to
//! the recorded Claude Code streams under [`fixtures`]. [`context`] assembles
//! the first three into the [`ServiceContext`](crate::ServiceContext) a service
//! actually takes, with a change-event receiver already listening (ADR-0018).
//!
//! Note what is *not* faked. Git and the filesystem are real, because a mocked
//! git only ever proves the mock works. The Claude CLI is replayed from recorded
//! output rather than hidden behind a trait. Only time is synthetic, and only
//! because a fifteen-minute backoff must not cost fifteen minutes.
//!
//! Everything here panics on failure instead of returning [`crate::Result`].
//! These are test scaffolding: a broken fixture or an unavailable `git` is a
//! defect in the test environment, not a condition any caller can handle, and
//! panicking keeps the helpers chainable.
//!
//! Compiled only under the `testing` feature, which `cargo test -p rimaia-core`
//! turns on through the crate's self-referencing dev-dependency.

pub mod clock;
pub mod context;
pub mod db;
pub mod doctor;
pub mod fixtures;
pub mod repo;

pub use clock::TestClock;
pub use context::{test_epoch, TestContext};
pub use db::test_pool;
pub use repo::TempRepo;
