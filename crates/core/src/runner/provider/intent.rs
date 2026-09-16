//! What Rimaia wants from one attempt, in a vocabulary no provider owns
//! (ADR-0026, seam-contract D27.3).
//!
//! This is the type that replaced `Invocation`, and the two fields that changed
//! are the whole point of the exercise. `disallowed_tools: Vec<String>` carried
//! eleven Claude permission-rule patterns — a provider with coarse sandbox modes
//! and no deny list cannot read one, and handing it the strings anyway is how
//! ADR-0012's mitigations evaporate quietly. `mcp_config: Option<String>` was an
//! inline JSON document in Claude's own shape, which a provider that reads its
//! MCP servers from a config home cannot be handed at all.
//!
//! Everything else here is a concept Rimaia would have had whatever it drives: a
//! conversation to open or continue, a posture, an isolation choice, orchestrator
//! facts, a prompt, a model, an effort, a turn budget and a directory to run in.

use std::path::Path;

use crate::db::settings::RunEnvironment;

use super::ProviderId;

// ---------------------------------------------------------------------------
// What a run is allowed to do
// ---------------------------------------------------------------------------

/// ADR-0012's two postures, and there is no third.
///
/// The ADR is emphatic that this is "the decision with the largest blast radius
/// in the product", so it is an enum rather than a string threaded through a
/// call chain: a mode is chosen once, from a `RunTrigger`, and the provider's
/// `init` event is checked against it rather than trusted.
///
/// **It carries no spelling.** `"bypassPermissions"` is one provider's word for
/// this and lives in that provider's module; shared code compares two values of
/// this type and never two strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionMode {
    /// Unattended: no prompts, behind a per-repository opt-in.
    BypassPermissions,
    /// A run the operator started with the app in front of them. ADR-0012's
    /// "conservative default for interactive runs".
    AcceptEdits,
}

impl PermissionMode {
    /// Whether anybody is there to be asked.
    ///
    /// The distinction [`negotiate`](super::negotiate) turns on: a capability a
    /// provider is missing refuses a run nobody is watching and is merely
    /// recorded on a run somebody started by hand.
    pub const fn autonomy(self) -> Autonomy {
        match self {
            Self::BypassPermissions => Autonomy::NoQuestions,
            Self::AcceptEdits => Autonomy::AskBeforeCommands,
        }
    }
}

/// Whether a human is present to answer for this run.
///
/// Derived from [`PermissionMode`] rather than carried beside it, so the two
/// cannot be set to disagree — which is the same reason `RunTrigger` derives the
/// permission mode instead of taking one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Autonomy {
    /// Unattended. A missing safety capability is a refusal.
    NoQuestions,
    /// Somebody pressed the button and is looking at the screen. A missing
    /// capability is recorded on the run and the run proceeds.
    AskBeforeCommands,
}

// ---------------------------------------------------------------------------
// What a run may not do
// ---------------------------------------------------------------------------

/// An operation ADR-0012 takes away from a run, named as the *operation* rather
/// than as one provider's rule string.
///
/// The three git arms are ADR-0012 point 3's three operations. `AnyFileMutation`
/// and `AnyShellCommand` are the planner's extra denials (ADR-0016: it reads a
/// repository and writes one MCP call). `RimaiaToolSurface` is the denial that
/// is not the operator's to turn off — it closes the hole that would otherwise
/// let an implementation run move its own card.
///
/// How each is *spelled* is the provider's business, and whether it can be
/// spelled at all is [`Capabilities::enforceable`](super::Capabilities::enforceable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForbiddenOperation {
    /// `git push --force` / `-f` / `--force-with-lease`.
    RemoteHistoryRewrite,
    /// `git push --delete` / `-d` / `git push origin :branch`.
    RemoteBranchDeletion,
    /// `git reset --hard origin/…`.
    HardResetToRemote,
    /// The planner writes nothing.
    AnyFileMutation,
    /// The planner runs nothing.
    AnyShellCommand,
    /// Every `mcp__rimaia__*` tool, as an intent rather than as a list.
    RimaiaToolSurface,
    /// An operator-authored rule in one provider's vocabulary.
    ///
    /// Tagged with the provider whose vocabulary it is, so it is never handed to
    /// another one — and never silently dropped either. A rule tagged for a
    /// different provider is dropped with a recorded warning, which is the one
    /// arm of this axis that is deliberately not a refusal: it was never a
    /// statement about the provider now being asked, and letting one operator's
    /// rule strings brick every run of another would be a bug wearing a safety
    /// hat.
    ProviderRule { provider: ProviderId, rule: String },
}

impl ForbiddenOperation {
    pub fn kind(&self) -> ForbiddenKind {
        match self {
            Self::RemoteHistoryRewrite => ForbiddenKind::RemoteHistoryRewrite,
            Self::RemoteBranchDeletion => ForbiddenKind::RemoteBranchDeletion,
            Self::HardResetToRemote => ForbiddenKind::HardResetToRemote,
            Self::AnyFileMutation => ForbiddenKind::AnyFileMutation,
            Self::AnyShellCommand => ForbiddenKind::AnyShellCommand,
            Self::RimaiaToolSurface => ForbiddenKind::RimaiaToolSurface,
            Self::ProviderRule { .. } => ForbiddenKind::ProviderRule,
        }
    }

    /// The operation in a sentence a refusal or a card can carry.
    pub fn describe(&self) -> String {
        match self {
            Self::RemoteHistoryRewrite => "rewriting history on a remote".to_string(),
            Self::RemoteBranchDeletion => "deleting a branch on a remote".to_string(),
            Self::HardResetToRemote => "resetting hard onto a remote ref".to_string(),
            Self::AnyFileMutation => "changing any file".to_string(),
            Self::AnyShellCommand => "running any shell command".to_string(),
            Self::RimaiaToolSurface => "calling Rimaia's own MCP tools".to_string(),
            Self::ProviderRule { provider, rule } => {
                format!("the {} rule `{rule}`", provider.as_str())
            }
        }
    }
}

/// [`ForbiddenOperation`] without its payload, so a provider can declare what it
/// is able to enforce as a `const` slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForbiddenKind {
    RemoteHistoryRewrite,
    RemoteBranchDeletion,
    HardResetToRemote,
    AnyFileMutation,
    AnyShellCommand,
    RimaiaToolSurface,
    ProviderRule,
}

// ---------------------------------------------------------------------------
// The conversation
// ---------------------------------------------------------------------------

/// Which conversation this attempt belongs to, and where that provider keeps its
/// conversations (ADR-0026 point 5).
///
/// `conversation` is **Rimaia's** id, minted before anything is spawned. A
/// provider that takes a session id up front uses it as one; a provider that
/// mints its own ignores it and announces something else, which is never
/// persisted. Either way `runs.session_id` means "the conversation this attempt
/// belongs to", which is what `scheduler::attempts` has always counted a retry
/// budget against.
///
/// `home` is `<app-data>/providers/<provider>/<task-id>/`, created and owned by
/// Rimaia. A `PreMinted` provider ignores it. It is what makes "continue the last
/// conversation here" exact by construction: ADR-0005 already gives every task
/// its own worktree, so *the* last conversation in this home is *the* previous
/// attempt of this task, with no id to read back and nothing to persist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIntent<'a> {
    /// Start a new conversation.
    Open {
        conversation: &'a str,
        home: &'a Path,
    },
    /// Continue the one an earlier attempt started (ADR-0011: "retries resume,
    /// they do not restart").
    ///
    /// `last_announced` is the id the previous attempt's *provider* reported, as
    /// recovered from that attempt's transcript. `None` means there was nothing
    /// to recover, which starts a fresh conversation — the same answer the queue
    /// already gives for a pruned history.
    Continue {
        conversation: &'a str,
        home: &'a Path,
        last_announced: Option<&'a str>,
    },
}

impl<'a> SessionIntent<'a> {
    /// The id `runs.session_id` carries, whichever arm this is.
    pub const fn conversation(&self) -> &'a str {
        match self {
            Self::Open { conversation, .. } | Self::Continue { conversation, .. } => conversation,
        }
    }

    pub const fn home(&self) -> &'a Path {
        match self {
            Self::Open { home, .. } | Self::Continue { home, .. } => home,
        }
    }

    pub const fn is_continuation(&self) -> bool {
        matches!(self, Self::Continue { .. })
    }
}

// ---------------------------------------------------------------------------
// Rimaia's own address
// ---------------------------------------------------------------------------

/// The scoped handle a run may reach Rimaia through (seam-contract D17.4, D27.4).
///
/// A URL and a server name, never a document: `RunHandles` mints the URL, and how
/// that becomes something the agent can call — a flag, a config file, an
/// environment variable — is the provider's private business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RimaiaHandle {
    pub url: String,
    pub server: &'static str,
}

// ---------------------------------------------------------------------------
// The intent
// ---------------------------------------------------------------------------

/// Everything one attempt is a pure function of.
///
/// Assembled once and then only read, so the exact thing a run was spawned with
/// is a value a test can hold — which matters more here than almost anywhere
/// else in the codebase, because the flags this product is most dangerous to get
/// wrong are the ones a test can pin byte for byte without spawning anything.
///
/// **Never put the provider on this.** The argv tests construct it as a plain
/// value and an `Arc<dyn AgentProvider>` field would cost the `Eq` that makes
/// that possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIntent<'a> {
    pub session: SessionIntent<'a>,
    pub permission_mode: PermissionMode,
    pub run_environment: RunEnvironment,
    /// Orchestrator facts the agent may not weigh against the task (ADR-0012
    /// point 4). Delivered out of band — never folded into the prompt.
    pub system_append: String,
    /// Delivered on stdin, which is then closed.
    pub prompt: &'a str,
    /// `None` lets the provider pick, which is what a task with no explicit
    /// strategy means (ADR-0016: the column is nullable precisely so "not set"
    /// is expressible).
    pub model: Option<String>,
    pub effort: Option<String>,
    /// ADR-0011 bounds a runaway loop with this.
    pub max_turns: Option<u32>,
    /// The child's working directory (ADR-0005). Never the user's own checkout.
    pub workspace: &'a Path,
    /// What this run may not do. Operations, not rule strings.
    pub forbidden: Vec<ForbiddenOperation>,
    /// Rimaia tool names this run must be able to call without stopping to ask
    /// — `set_task_strategy` for a planner, nothing for an implementation run.
    ///
    /// Rimaia's names, which the provider spells in its own convention. Empty
    /// for an implementation run, which gets its blanket approval from
    /// `bypassPermissions` (ADR-0012) and needs no list.
    pub required_tools: Vec<&'static str>,
    /// `None` for an implementation run, which reaches Rimaia through nothing
    /// and has no reason to.
    pub rimaia_handle: Option<RimaiaHandle>,
}
