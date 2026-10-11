//! The runner protocol's request bodies (seam-contract D31 point 10, task
//! 052): what `HttpBoard` sends to `POST /api/v1/runner/<method>` and what the
//! server's handler reads, one type for both ends so the two cannot drift.
//!
//! Each is the trait method's arguments, `camelCase`, with two differences:
//!
//! - **No runner id.** The runner is the token's, and nothing else (D31
//!   point 3).
//! - **A provider on `preview`, `claim` and `run_context`.** The board needs
//!   the claiming runner's provider for the catalogue and ADR-0031 point 1's
//!   model rule. It is a field of the wire body, not of the trait's
//!   arguments, and the server holds it against the provider the runner was
//!   paired as (`board::runners::paired_provider`).
//!
//! `append_transcript` has no body type: its body is the raw bytes, and
//! [`TranscriptQuery`] is its query string (ADR-0034 point 2).

use serde::{Deserialize, Serialize};

use crate::review::findings::NewReviewFinding;
use crate::runner::events::RunTail;
use crate::runner::provider::ProviderId;
use crate::tasks::strategy::StrategyPlan;

use super::types::{ClaimTarget, FinishRun, LeaseRef, PreviewOf, StartRun, TranscriptChunk};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewBody {
    pub task_id: String,
    pub of: PreviewOf,
    pub provider: ProviderId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimBody {
    pub target: ClaimTarget,
    pub provider: ProviderId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatBody {
    pub held: Vec<LeaseRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunContextBody {
    pub lease: LeaseRef,
    pub provider: ProviderId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordBranchBody {
    pub lease: LeaseRef,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRunBody {
    pub lease: LeaseRef,
    pub run: StartRun,
}

/// `append_transcript`'s query string: the whole [`LeaseRef`], the run and the
/// offset. Ids only, never the token (ADR-0034 point 2).
///
/// The whole lease rather than D31 point 10's `run_id`, `generation` and
/// `offset`: the service takes a `LeaseRef`, and deriving the task and team
/// from the run would give this one method a scope no other method has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptQuery {
    pub task_id: String,
    pub team_id: String,
    pub generation: i64,
    pub run_id: String,
    pub offset: u64,
}

impl TranscriptQuery {
    pub fn new(lease: &LeaseRef, run_id: &str, offset: u64) -> Self {
        Self {
            task_id: lease.task_id.clone(),
            team_id: lease.team_id.clone(),
            generation: lease.generation,
            run_id: run_id.to_string(),
            offset,
        }
    }

    pub fn lease(&self) -> LeaseRef {
        LeaseRef::new(self.task_id.clone(), self.generation, self.team_id.clone())
    }

    /// The chunk the query and the body make together.
    pub fn chunk(&self, bytes: Vec<u8>) -> TranscriptChunk {
        TranscriptChunk {
            run_id: self.run_id.clone(),
            offset: self.offset,
            bytes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishTailBody {
    pub lease: LeaseRef,
    pub tail: RunTail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishRunBody {
    pub lease: LeaseRef,
    pub run_id: String,
    pub finish: FinishRun,
}

/// `release`'s body: the lease alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseBody {
    pub lease: LeaseRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordStrategyBody {
    pub lease: LeaseRef,
    pub plan: StrategyPlan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordReviewFindingsBody {
    pub lease: LeaseRef,
    pub run_id: String,
    pub findings: Vec<NewReviewFinding>,
}
