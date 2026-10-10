//! The line between a runner and the board (ADR-0027 point 5, seam-contract
//! D31).
//!
//! Runner code reaches the board only through [`BoardPort`]: a claim, the
//! context a run is composed from, the reports a run makes and the decisions
//! the board answers them with. Solo answers in process through
//! [`InProcessBoard`]; task 052 answers the same trait over HTTP, and the
//! contract suite in `testing::board_contract` binds the two.
//!
//! What stays off the port is runner-owned state (D31 point 14): the run
//! window, the usage-limit pause, capacity, the queue switch and the run
//! environment are this machine's, read through
//! [`MachineContext`](crate::machine::MachineContext) since task 041, and never
//! cross it.

pub mod in_process;
pub mod lease;
pub mod port;
pub mod service;
pub mod types;

pub use in_process::InProcessBoard;
pub use lease::{LeaseTerm, LEASE_LIFETIME};
pub use port::{BoardFuture, BoardPort};
pub use types::{
    BaseDependency, BoardMethod, ChangeSummary, Claim, ClaimTarget, FinishReceipt, FinishRun,
    FreeCapacity, Heartbeat, ImplementationBase, LeasePurpose, LeaseRef, NextStep, ReviewContext,
    RunBase, RunContext, StartRun, TeamLimits, TranscriptAck, TranscriptChunk, TranscriptEnd,
};
