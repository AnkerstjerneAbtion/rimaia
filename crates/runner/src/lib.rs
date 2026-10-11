//! The runner: the loop that claims, supervises and reports work, and the
//! store one machine keeps for itself (ADR-0027 point 6, ADR-0028 point 3).
//!
//! It holds the store, `runner.db`, which sits beside `rimaia.db` in solo and
//! will be the only database a headless runner has (task 058); the one-time
//! adoption of this machine's state out of the board; the `MachineStore`
//! implementation on that store, which the host injects into `rimaia-core` as
//! a `MachineContext` (task 041); and the loop, [`queue`] (task 042), which
//! asks the board for work through the port and obeys this machine's rules.
//! [`board::HttpBoard`] is the port's adapter for a board on a server, and
//! [`relay::ShellRelay`] answers solo's Run now and Cancel on this machine
//! (task 052).
//!
//! "Runner" also names `rimaia_core::runner`, the agent-process layer that
//! spawns, streams and classifies one run. ADR-0027 point 6 accepts the
//! overlap: the paths never collide, and this crate is the loop around that
//! layer.
//!
//! Errors are `rimaia_core::Error` (seam-contract D8), so the shell keeps one
//! type across its boundary.

pub mod adopt;
pub mod board;
pub mod identity;
pub mod machine;
pub mod queue;
pub mod relay;
pub mod store;

pub use store::RunnerStore;
