//! The line between a runner and the board (ADR-0027 point 5, seam-contract
//! D31).
//!
//! Runner code reaches the board only through [`BoardPort`]: a claim, the
//! context a run is composed from, the reports a run makes and the decisions
//! the board answers them with. Solo answers in process through
//! [`InProcessBoard`]; task 052 answers the same trait over HTTP
//! (`rimaia_runner::board::http`, served by `rimaia-server`'s runner routes
//! with the bodies in [`wire`]), and the contract suite in
//! `testing::board_contract` binds the two. [`relay`] is how a person's Run
//! now and Cancel reach a runner.
//!
//! What stays off the port is runner-owned state (D31 point 14): the run
//! window, the usage-limit pause, capacity, the queue switch and the run
//! environment are this machine's, read through
//! [`MachineContext`](crate::machine::MachineContext) since task 041, and never
//! cross it.

pub mod cancel;
pub mod in_process;
pub mod lease;
pub mod port;
pub mod relay;
pub mod runners;
pub mod service;
pub mod types;
pub mod wire;

pub use cancel::CancelRequests;
pub use in_process::InProcessBoard;
pub use lease::{LeaseTerm, CLAIM_WAIT_MAX, HEARTBEAT_INTERVAL, LEASE_LIFETIME};
pub use port::{BoardFuture, BoardPort};
pub use relay::{RelayedRequests, RunNowRequest, RunnerRelay};
pub use service::{authorize_start, OwnerPresence, BASE_REF_RULE};
pub use types::{
    BaseDependency, BoardMethod, ChangeSummary, Claim, ClaimTarget, FinishReceipt, FinishRun,
    FreeCapacity, Heartbeat, ImplementationBase, LeasePurpose, LeaseRef, NextStep, PreviewOf,
    ReviewContext, RunAuthorship, RunBase, RunContext, StartRun, TeamLimits, TranscriptAck,
    TranscriptChunk, TranscriptEnd,
};
