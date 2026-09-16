//! The agent CLI behind a seam (ADR-0026, seam-contract D27).
//!
//! **A provider owns two things — how an intent becomes a child process, and
//! what one line of its output means. Everything else stays Rimaia's.**
//!
//! # What that buys, and the rule that keeps it honest
//!
//! Process groups, SIGTERM-then-SIGKILL, the grace period, stdin-write-and-close,
//! the transcript, the tail channel, classification, retry, the board and the
//! scheduler are all written once and never learn what spawned a run. The trait
//! may not return events, outcomes or runs: it returns a [`SpawnPlan`] and a
//! parsed [`RunEvent`](crate::runner::events::RunEvent), both of which are
//! values. The falsification is short — **if an implementation of this trait can
//! be satisfied without any process ever being spawned, the seam is cut in the
//! wrong place.**
//!
//! # Why classification is not on the trait
//!
//! ADR-0011 puts classification "in one module with unit tests over captured CLI
//! output", and a provider that could answer `ExitClass` directly could satisfy
//! that trait with no stream involved at all. The six classes are Rimaia's state
//! machine, not a wire format. A provider answers
//! [`EndReason`](crate::runner::events::EndReason) and
//! [`UsageWindow`](crate::runner::events::UsageWindow); Rimaia decides what they
//! mean.
//!
//! # Why there is a `negotiate` and not a `can_run`
//!
//! [`negotiate`] is a free function over [`Capabilities`], not a trait method, so
//! the rules are Rimaia's and every provider is judged by the same ones. A
//! provider declares what it can express; this module decides what a missing
//! capability costs — which for ADR-0012's blocklist is a refused unattended run
//! and never a warning, because the mitigations are part of why
//! `bypassPermissions` was granted at all.

pub mod claude;
pub mod intent;

use std::path::Path;

use crate::db::settings::RunEnvironment;
use crate::error::{Error, Result};
use crate::runner::events::RunEvent;

pub use claude::ClaudeProvider;
pub use intent::{
    Autonomy, ForbiddenKind, ForbiddenOperation, PermissionMode, RimaiaHandle, RunIntent,
    SessionIntent,
};

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// A provider's stable identity.
///
/// Reaches doctor labels, a fixtures directory, and — when a second real
/// provider lands — a `runs` column. **Not a display name**: that is task 032's,
/// and it is a different string for the same reason `run_state` is an enum and
/// its badge is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    ClaudeCode,
    /// The deliberately fictional provider that proves the seam.
    ///
    /// Compiled only under the `testing` feature, so a release build cannot name
    /// it, let alone select it. It is evidence about Rimaia's seam and nothing
    /// else — see `crates/core/tests/fixtures/ledger/README.md`.
    #[cfg(feature = "testing")]
    Ledger,
}

impl ProviderId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            #[cfg(feature = "testing")]
            Self::Ledger => "ledger",
        }
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// One agent CLI, as a translation table.
///
/// Object-safe by construction — no generics, no `Self` by value, no `async fn`,
/// no associated types — because [`RunnerConfig`](crate::runner::RunnerConfig)
/// holds one as `Arc<dyn AgentProvider>` and is `Clone + Debug` (D27.2). Every
/// method is `&self` and pure: a provider is a value, not a service.
pub trait AgentProvider: std::fmt::Debug + Send + Sync + 'static {
    /// Stable identity. Not a display name.
    fn id(&self) -> ProviderId;

    /// What [`RunnerConfig::program`](crate::runner::RunnerConfig::program)
    /// defaults to, resolved through `PATH`.
    fn default_program(&self) -> &'static str;

    /// What this provider can and cannot express. Read once per run, **before
    /// anything is written**.
    fn capabilities(&self) -> &'static Capabilities;

    /// Turns what Rimaia wants into a child process.
    ///
    /// May write into `scratch`, a per-attempt directory Rimaia creates and
    /// deletes on every exit path. Only ever called on an intent [`negotiate`]
    /// has already accepted, so it never decides whether it *can* — only how.
    fn plan_spawn(&self, intent: &RunIntent<'_>, scratch: &Path) -> Result<SpawnPlan>;

    /// One line of stdout, in Rimaia's event vocabulary.
    ///
    /// Tolerant by rule (ADR-0004): the only `Err` is "not parseable at all". An
    /// event this provider does not model is
    /// [`RunEvent::Other`](crate::runner::events::RunEvent::Other), which is a
    /// value and not a failure.
    fn parse_line(&self, line: &str) -> std::result::Result<RunEvent, serde_json::Error>;
}

/// How to start one attempt.
///
/// A plan rather than an argument vector, and that is the difference ADR-0026
/// point 2 turns on: a flag on one provider is a config file plus an environment
/// variable on another, and a `Vec<String>` cannot express the second at all.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpawnPlan {
    pub args: Vec<String>,
    /// Set on the child, on top of the inherited environment.
    pub env_set: Vec<(String, String)>,
    /// Removed from the inherited environment, on top of the identity variables
    /// Rimaia strips unconditionally.
    pub env_remove: Vec<String>,
    /// Written to the child's stdin, which is then closed. **A prompt left
    /// unclosed hangs the run** (`spike/FINDINGS.md` §7).
    pub stdin: String,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// What one provider can express, on the axes where providers actually differ.
///
/// Every field is `const`-constructible so a provider can declare one as a
/// `&'static` — a capability read off a database or a probe would be a capability
/// that can change between the refusal and the spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Whose capabilities these are, so [`negotiate`] can tell an operator rule
    /// written for *this* provider from one written for another.
    pub id: ProviderId,
    pub session: SessionCapability,
    /// How orchestrator facts reach the agent (ADR-0012 point 4).
    pub orchestrator_channel: OrchestratorChannel,
    pub handle_injection: HandleInjection,
    pub isolation: IsolationSupport,
    pub turn_budget: TurnBudget,
    pub posture_echo: PostureEcho,
    /// Which [`ForbiddenOperation`]s this provider can actually enforce.
    pub enforceable: &'static [ForbiddenKind],
    /// The environment-variable prefixes that are this provider's own process
    /// identity. A slice, not a string: one provider exports two of them.
    pub identity_prefixes: &'static [&'static str],
}

/// Who mints the conversation id, and how an earlier one is continued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCapability {
    /// Takes an id up front, so a resume works even if the child dies before it
    /// announces itself (ADR-0004).
    PreMinted,
    /// Mints and announces its own. Rimaia's conversation id is still what
    /// `runs.session_id` carries (ADR-0026 point 5).
    SelfMinted { resume: ResumeStyle },
}

impl SessionCapability {
    /// Whether an earlier conversation can be continued at all.
    ///
    /// The one thing ADR-0026 point 6 turns on: a continuation prompt delivered
    /// into a *fresh* session produces an agent with no plan, no context and an
    /// empty diff — a seam bug that would read as a bad model.
    pub const fn can_continue(self) -> bool {
        match self {
            Self::PreMinted => true,
            Self::SelfMinted { resume } => !matches!(resume, ResumeStyle::Unsupported),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeStyle {
    /// By the id the previous attempt announced, recovered from that attempt's
    /// transcript.
    ById,
    /// "The last conversation in this home", which ADR-0005's worktree-per-task
    /// makes exact by construction.
    LastInHome,
    /// Cannot continue. Every attempt is a fresh conversation, so every attempt
    /// is sent the composed prompt.
    Unsupported,
}

/// Where the facts in [`RunIntent::system_append`] can be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchestratorChannel {
    /// A channel the agent reads but the task text does not occupy — Claude's
    /// `--append-system-prompt`.
    OutOfBand,
    /// A file the agent is pointed at. `inside_workspace` is the dangerous case:
    /// a run that can rewrite its own constraints has none.
    HandedFile { inside_workspace: bool },
    /// No channel at all. Folding the facts into the prompt is explicitly **not**
    /// the fallback (ADR-0012 point 4).
    None,
}

/// How a scoped [`RimaiaHandle`] reaches the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleInjection {
    Argument { flag: &'static str },
    ConfigHome { env: &'static str },
    None,
}

/// Which `run_environment` values this provider can honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationSupport {
    Both,
    /// Only this one, for the stated reason.
    Only(RunEnvironment, &'static str),
    /// Inherits the operator's configuration, except that injecting a handle
    /// replaces the configuration home and therefore isolates the run.
    InheritExceptWhenHandleInjected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnBudget {
    ProviderEnforced {
        flag: &'static str,
    },
    /// ADR-0011 asked for a bound, and an unbounded overnight run on someone's
    /// subscription is what it asked for a bound against. **Rimaia counting turns
    /// itself is deliberately not built**: it is new machinery with no current
    /// user, and it would reorder `classify`'s rules.
    Unavailable,
}

/// Whether the provider says which posture it applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostureEcho {
    Echoed,
    Silent,
}

// ---------------------------------------------------------------------------
// Negotiation
// ---------------------------------------------------------------------------

/// What Rimaia decided, having read what the provider can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    pub prompt_style: PromptStyle,
    /// Whether the `init` event's posture is worth comparing. `false` under
    /// [`PostureEcho::Silent`], where there is nothing to compare against — never
    /// a licence to ignore a mismatch that *was* reported.
    pub verify_posture: bool,
    /// Operations this provider could not enforce, on a run a human started.
    /// Recorded **on the run**, so "this attempt ran without ADR-0012's
    /// mitigations" is a fact a morning reviewer can read.
    pub unenforced: Vec<ForbiddenOperation>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStyle {
    /// The whole composed prompt (ADR-0009).
    Composed,
    /// ADR-0011's one-line continuation, which is only correct into a session
    /// that already carries the composed one.
    Continuation,
}

/// Which axis a provider fell short on. Carried so a caller can act on the shape
/// of the refusal rather than on its sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalAxis {
    Forbidden,
    OrchestratorChannel,
    HandleInjection,
    Isolation,
    TurnBudget,
}

/// A run this provider may not start, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub provider: ProviderId,
    pub axis: RefusalAxis,
    pub message: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<Refusal> for Error {
    /// `invalid` rather than `internal`: nothing malfunctioned. The operator
    /// asked for a combination this provider cannot express, and the sentence
    /// names the capability rather than their repository.
    fn from(refusal: Refusal) -> Self {
        Error::invalid(refusal.message)
    }
}

/// Decides whether this provider may run this intent, and how.
///
/// One function rather than a trait method, so the rules below are Rimaia's and
/// every provider is judged by the same ones. Called **before any state is
/// written** — before the prerequisite probe, before the worktree, before the
/// claim — so a refusal leaves nothing half-open.
///
/// The axes, and what a shortfall costs:
///
/// 1. **An operation the provider cannot enforce** refuses an unattended run and
///    is recorded on a manual one. ADR-0012 granted `bypassPermissions` *because*
///    its mitigations are part of the feature; degrading with a warning is the
///    one answer this axis does not get. A rule tagged for a *different*
///    provider is dropped with a warning instead — it was never a statement
///    about this one.
/// 2. **No orchestrator channel** refuses an unattended run. Folding the facts
///    into the prompt is not the fallback.
/// 3. **A handed file inside the workspace** refuses an unattended run: a run
///    that can rewrite its own constraints has none. Outside it is allowed with
///    one recorded warning.
/// 4. **No handle injection**, for a run that needs one, is a refusal the planner
///    turns into a fallback — the same route seam-contract D16.7 already takes
///    for a busy MCP port. An implementation run carries no handle and never
///    reaches this.
/// 5. **An isolation mismatch** is refused, naming the combination. Downgrading
///    `inherit` to `strict_local` changes both cost and capability, which is a
///    choice and not a detail.
/// 6. **No turn budget**, for a run that asked for one, refuses unattended.
/// 7. **A silent posture** is allowed, with one warning recorded on the run.
pub fn negotiate(
    caps: &Capabilities,
    intent: &RunIntent<'_>,
) -> std::result::Result<RunPlan, Refusal> {
    let autonomy = intent.permission_mode.autonomy();
    let mut plan = RunPlan {
        prompt_style: if intent.session.is_continuation() && caps.session.can_continue() {
            PromptStyle::Continuation
        } else {
            PromptStyle::Composed
        },
        verify_posture: caps.posture_echo == PostureEcho::Echoed,
        unenforced: Vec::new(),
        warnings: Vec::new(),
    };

    for operation in &intent.forbidden {
        if caps.enforceable.contains(&operation.kind()) {
            continue;
        }
        if let ForbiddenOperation::ProviderRule { provider, .. } = operation {
            if *provider != caps.id {
                plan.warnings.push(format!(
                    "{} was not applied: it is written in {}'s vocabulary and this run is on {}",
                    operation.describe(),
                    provider.as_str(),
                    caps.id.as_str(),
                ));
                continue;
            }
        }
        if autonomy == Autonomy::NoQuestions {
            return Err(Refusal {
                provider: caps.id,
                axis: RefusalAxis::Forbidden,
                message: format!(
                    "{} cannot prevent the agent from {}, and an unattended run is not started \
                     without ADR-0012's mitigations. Start this task by hand, with the app in \
                     front of you, or use a provider that can express a tool blocklist.",
                    caps.id.as_str(),
                    operation.describe(),
                ),
            });
        }
        plan.warnings.push(format!(
            "{} cannot prevent the agent from {}; this run proceeds without that mitigation \
             because a human started it",
            caps.id.as_str(),
            operation.describe(),
        ));
        plan.unenforced.push(operation.clone());
    }

    match caps.orchestrator_channel {
        OrchestratorChannel::OutOfBand => {}
        OrchestratorChannel::HandedFile {
            inside_workspace: false,
        } => plan.warnings.push(format!(
            "{} receives Rimaia's instructions as a file beside the run rather than out of band",
            caps.id.as_str(),
        )),
        OrchestratorChannel::HandedFile {
            inside_workspace: true,
        } if autonomy == Autonomy::NoQuestions => {
            return Err(Refusal {
                provider: caps.id,
                axis: RefusalAxis::OrchestratorChannel,
                message: format!(
                    "{} can only receive Rimaia's instructions as a file inside the run's own \
                     workspace, where the agent could rewrite them. A run that can rewrite its \
                     own constraints has none, so it is not started unattended.",
                    caps.id.as_str(),
                ),
            })
        }
        OrchestratorChannel::HandedFile { .. } => plan.warnings.push(format!(
            "{} receives Rimaia's instructions as a file inside the workspace, where the agent \
             can reach them",
            caps.id.as_str(),
        )),
        OrchestratorChannel::None if autonomy == Autonomy::NoQuestions => {
            return Err(Refusal {
                provider: caps.id,
                axis: RefusalAxis::OrchestratorChannel,
                message: format!(
                    "{} has no channel for the facts Rimaia must state separately from the task \
                     (ADR-0012 point 4), and folding them into the prompt is not the fallback.",
                    caps.id.as_str(),
                ),
            })
        }
        OrchestratorChannel::None => plan.warnings.push(format!(
            "{} has no channel for Rimaia's own instructions; this run was given none",
            caps.id.as_str(),
        )),
    }

    if intent.rimaia_handle.is_some() && caps.handle_injection == HandleInjection::None {
        return Err(Refusal {
            provider: caps.id,
            axis: RefusalAxis::HandleInjection,
            message: format!(
                "{} cannot be handed Rimaia's scoped MCP handle, which is the only way this run \
                 could answer.",
                caps.id.as_str(),
            ),
        });
    }

    if let Some(message) = isolation_mismatch(caps, intent) {
        return Err(Refusal {
            provider: caps.id,
            axis: RefusalAxis::Isolation,
            message,
        });
    }

    if intent.max_turns.is_some() && caps.turn_budget == TurnBudget::Unavailable {
        if autonomy == Autonomy::NoQuestions {
            return Err(Refusal {
                provider: caps.id,
                axis: RefusalAxis::TurnBudget,
                message: format!(
                    "{} cannot bound an attempt to a number of turns, and an unbounded overnight \
                     run on your own subscription is what ADR-0011 asked for a bound against.",
                    caps.id.as_str(),
                ),
            });
        }
        plan.warnings.push(format!(
            "{} cannot bound this attempt to {} turns; it will run until it stops",
            caps.id.as_str(),
            intent.max_turns.unwrap_or_default(),
        ));
    }

    if caps.posture_echo == PostureEcho::Silent {
        plan.warnings.push(format!(
            "{} does not report the permission posture it applied, so this attempt's posture was \
             never verified",
            caps.id.as_str(),
        ));
    }

    Ok(plan)
}

/// The isolation half of [`negotiate`], as a sentence or nothing.
fn isolation_mismatch(caps: &Capabilities, intent: &RunIntent<'_>) -> Option<String> {
    match caps.isolation {
        IsolationSupport::Both => None,
        IsolationSupport::Only(supported, reason) if supported != intent.run_environment => {
            Some(format!(
                "{} can only run with `{}` ({reason}), and this run asked for `{}`. Downgrading \
                 would change both what it costs and what it can reach, which is a choice rather \
                 than a detail.",
                caps.id.as_str(),
                supported.as_str(),
                intent.run_environment.as_str(),
            ))
        }
        IsolationSupport::Only(..) => None,
        IsolationSupport::InheritExceptWhenHandleInjected => {
            let isolated_by_the_handle = intent.rimaia_handle.is_some();
            let asked_to_isolate = intent.run_environment == RunEnvironment::StrictLocal;
            if isolated_by_the_handle == asked_to_isolate {
                return None;
            }
            Some(format!(
                "{} decides its isolation by where its configuration home points, so `{}` and a \
                 {} scoped handle are not a combination it can be asked for.",
                caps.id.as_str(),
                intent.run_environment.as_str(),
                if isolated_by_the_handle {
                    "present"
                } else {
                    "missing"
                },
            ))
        }
    }
}

/// Every prefix any registered provider calls its own process identity
/// (seam-contract D27.5).
///
/// **The union, not the active provider's.** Rimaia is developed and tested from
/// inside a Claude Code session *and* may be spawning something else; the
/// converse arrives the moment anyone drives Rimaia from another agent. A child
/// told it is a nested session of whatever started Rimaia writes the wrong
/// session id into a transcript, and nothing else goes wrong until somebody reads
/// it.
pub fn identity_prefixes() -> Vec<&'static str> {
    let mut prefixes: Vec<&'static str> = Vec::new();
    for caps in every_capability() {
        for prefix in caps.identity_prefixes {
            if !prefixes.contains(prefix) {
                prefixes.push(prefix);
            }
        }
    }
    prefixes
}

/// Every provider this build knows about, for the union above.
///
/// A function rather than a `const` slice because the test-only provider is
/// compiled conditionally, and a list that silently lost an entry in release
/// builds is exactly the shape of bug D27.5 exists to prevent.
fn every_capability() -> Vec<&'static Capabilities> {
    vec![claude::CAPABILITIES]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    const EVERY_OPERATION: &[ForbiddenKind] = &[
        ForbiddenKind::RemoteHistoryRewrite,
        ForbiddenKind::RemoteBranchDeletion,
        ForbiddenKind::HardResetToRemote,
        ForbiddenKind::AnyFileMutation,
        ForbiddenKind::AnyShellCommand,
        ForbiddenKind::RimaiaToolSurface,
        ForbiddenKind::ProviderRule,
    ];

    /// A provider that can express everything, so each test below takes exactly
    /// one capability away.
    fn capable() -> Capabilities {
        Capabilities {
            id: ProviderId::ClaudeCode,
            session: SessionCapability::PreMinted,
            orchestrator_channel: OrchestratorChannel::OutOfBand,
            handle_injection: HandleInjection::Argument { flag: "--handle" },
            isolation: IsolationSupport::Both,
            turn_budget: TurnBudget::ProviderEnforced { flag: "--turns" },
            posture_echo: PostureEcho::Echoed,
            enforceable: EVERY_OPERATION,
            identity_prefixes: &["CLAUDE"],
        }
    }

    fn intent<'a>(permission_mode: PermissionMode) -> RunIntent<'a> {
        RunIntent {
            session: SessionIntent::Open {
                conversation: "conversation-1",
                home: Path::new("/tmp/home"),
            },
            permission_mode,
            run_environment: RunEnvironment::Inherit,
            system_append: "You are running unattended, started by Rimaia.".to_string(),
            prompt: "do the thing",
            model: None,
            effort: None,
            max_turns: Some(300),
            workspace: Path::new("/tmp/worktree"),
            forbidden: Vec::new(),
            required_tools: Vec::new(),
            rimaia_handle: None,
        }
    }

    #[test]
    fn a_provider_that_can_express_everything_negotiates_clean() {
        let plan = negotiate(&capable(), &intent(PermissionMode::BypassPermissions))
            .expect("nothing was asked for that this provider cannot do");

        assert_eq!(plan.prompt_style, PromptStyle::Composed);
        assert!(plan.verify_posture);
        assert_eq!(plan.warnings, Vec::<String>::new());
        assert_eq!(plan.unenforced, Vec::<ForbiddenOperation>::new());
    }

    #[test]
    fn an_operation_a_provider_cannot_enforce_refuses_an_unattended_run() {
        // ADR-0012's mitigations are part of why `bypassPermissions` was granted.
        // Dropping one silently would make the per-repository opt-in a statement
        // about a posture that no longer exists.
        let caps = Capabilities {
            enforceable: &[],
            ..capable()
        };
        let intent = RunIntent {
            forbidden: vec![ForbiddenOperation::RemoteHistoryRewrite],
            ..intent(PermissionMode::BypassPermissions)
        };

        let refusal = negotiate(&caps, &intent).expect_err("an unattended run must be refused");

        assert_eq!(refusal.axis, RefusalAxis::Forbidden);
        assert!(
            refusal.message.contains("rewriting history on a remote"),
            "the refusal must name the operation: {}",
            refusal.message
        );
    }

    #[test]
    fn the_same_shortfall_is_recorded_rather_than_refused_when_a_human_started_the_run() {
        let caps = Capabilities {
            enforceable: &[],
            ..capable()
        };
        let intent = RunIntent {
            forbidden: vec![ForbiddenOperation::RemoteHistoryRewrite],
            ..intent(PermissionMode::AcceptEdits)
        };

        let plan = negotiate(&caps, &intent).expect("a manual run proceeds");

        assert_eq!(
            plan.unenforced,
            vec![ForbiddenOperation::RemoteHistoryRewrite]
        );
        assert_eq!(plan.warnings.len(), 1);
    }

    #[test]
    fn a_rule_written_for_another_provider_is_dropped_with_a_warning_and_never_refused() {
        // It was never a statement about this provider. Letting one operator's
        // rule strings brick every run of another would be a bug wearing a
        // safety hat.
        let caps = Capabilities {
            id: ProviderId::ClaudeCode,
            enforceable: &[],
            ..capable()
        };
        let foreign = ForbiddenOperation::ProviderRule {
            #[cfg(feature = "testing")]
            provider: ProviderId::Ledger,
            #[cfg(not(feature = "testing"))]
            provider: ProviderId::ClaudeCode,
            rule: "Bash(git push --force:*)".to_string(),
        };
        let intent = RunIntent {
            forbidden: vec![foreign],
            ..intent(PermissionMode::BypassPermissions)
        };

        let outcome = negotiate(&caps, &intent);

        #[cfg(feature = "testing")]
        {
            let plan = outcome.expect("a foreign rule is not this provider's failure");
            assert_eq!(plan.warnings.len(), 1);
            assert_eq!(plan.unenforced, Vec::<ForbiddenOperation>::new());
        }
        // Without the testing feature there is only one provider, so the rule is
        // this provider's own and the ordinary refusal applies.
        #[cfg(not(feature = "testing"))]
        assert_eq!(
            outcome.expect_err("its own rule it cannot enforce").axis,
            RefusalAxis::Forbidden
        );
    }

    #[test]
    fn a_provider_with_no_orchestrator_channel_refuses_an_unattended_run() {
        let caps = Capabilities {
            orchestrator_channel: OrchestratorChannel::None,
            ..capable()
        };

        let refusal = negotiate(&caps, &intent(PermissionMode::BypassPermissions))
            .expect_err("there is nowhere to put ADR-0012 point 4's facts");

        assert_eq!(refusal.axis, RefusalAxis::OrchestratorChannel);
    }

    #[test]
    fn a_file_the_run_could_rewrite_is_refused_and_one_beside_the_run_is_merely_noted() {
        let inside = Capabilities {
            orchestrator_channel: OrchestratorChannel::HandedFile {
                inside_workspace: true,
            },
            ..capable()
        };
        let outside = Capabilities {
            orchestrator_channel: OrchestratorChannel::HandedFile {
                inside_workspace: false,
            },
            ..capable()
        };

        assert_eq!(
            negotiate(&inside, &intent(PermissionMode::BypassPermissions))
                .expect_err("a run that can rewrite its own constraints has none")
                .axis,
            RefusalAxis::OrchestratorChannel
        );
        assert_eq!(
            negotiate(&outside, &intent(PermissionMode::BypassPermissions))
                .expect("a file beside the run is allowed")
                .warnings
                .len(),
            1
        );
    }

    #[test]
    fn a_provider_that_cannot_be_handed_the_scoped_handle_refuses_only_a_run_that_needs_one() {
        let caps = Capabilities {
            handle_injection: HandleInjection::None,
            isolation: IsolationSupport::Both,
            ..capable()
        };
        let planner = RunIntent {
            rimaia_handle: Some(RimaiaHandle {
                url: "http://127.0.0.1:4517/mcp/run/token".to_string(),
                server: "rimaia",
            }),
            ..intent(PermissionMode::AcceptEdits)
        };

        assert_eq!(
            negotiate(&caps, &planner)
                .expect_err("a planner with no way to answer")
                .axis,
            RefusalAxis::HandleInjection
        );
        // An implementation run carries no handle and never notices.
        assert!(negotiate(&caps, &intent(PermissionMode::BypassPermissions)).is_ok());
    }

    #[test]
    fn an_isolation_a_provider_cannot_offer_is_refused_rather_than_quietly_downgraded() {
        let caps = Capabilities {
            isolation: IsolationSupport::Only(
                RunEnvironment::StrictLocal,
                "it has no notion of an operator's own configuration",
            ),
            ..capable()
        };

        let refusal = negotiate(&caps, &intent(PermissionMode::BypassPermissions))
            .expect_err("inherit was asked for and cannot be given");

        assert_eq!(refusal.axis, RefusalAxis::Isolation);
        assert!(refusal.message.contains("inherit"), "{}", refusal.message);
    }

    #[test]
    fn a_provider_whose_isolation_follows_its_config_home_refuses_the_inconsistent_combination() {
        let caps = Capabilities {
            isolation: IsolationSupport::InheritExceptWhenHandleInjected,
            ..capable()
        };
        let isolated_without_a_handle = RunIntent {
            run_environment: RunEnvironment::StrictLocal,
            ..intent(PermissionMode::AcceptEdits)
        };

        assert!(
            negotiate(&caps, &intent(PermissionMode::BypassPermissions)).is_ok(),
            "inherit with no handle is the consistent pair"
        );
        assert_eq!(
            negotiate(&caps, &isolated_without_a_handle)
                .expect_err("nothing would isolate this run")
                .axis,
            RefusalAxis::Isolation
        );
    }

    #[test]
    fn a_provider_that_cannot_bound_an_attempt_refuses_an_unattended_one() {
        let caps = Capabilities {
            turn_budget: TurnBudget::Unavailable,
            ..capable()
        };

        assert_eq!(
            negotiate(&caps, &intent(PermissionMode::BypassPermissions))
                .expect_err("ADR-0011 asked for a bound")
                .axis,
            RefusalAxis::TurnBudget
        );
        assert_eq!(
            negotiate(&caps, &intent(PermissionMode::AcceptEdits))
                .expect("a human is watching this one")
                .warnings
                .len(),
            1
        );
    }

    #[test]
    fn a_silent_posture_is_allowed_and_recorded_rather_than_trusted() {
        let caps = Capabilities {
            posture_echo: PostureEcho::Silent,
            ..capable()
        };

        let plan = negotiate(&caps, &intent(PermissionMode::BypassPermissions))
            .expect("nothing to verify is not a failure");

        assert!(!plan.verify_posture);
        assert_eq!(plan.warnings.len(), 1, "the fact goes on the run");
    }

    #[test]
    fn a_provider_that_cannot_continue_is_sent_the_composed_prompt() {
        // ADR-0026 point 6: a continuation prompt delivered into a fresh session
        // produces an agent with no plan, no context and an empty diff.
        let continuing = RunIntent {
            session: SessionIntent::Continue {
                conversation: "conversation-1",
                home: Path::new("/tmp/home"),
                last_announced: None,
            },
            ..intent(PermissionMode::BypassPermissions)
        };
        let cannot = Capabilities {
            session: SessionCapability::SelfMinted {
                resume: ResumeStyle::Unsupported,
            },
            ..capable()
        };

        assert_eq!(
            negotiate(&capable(), &continuing)
                .expect("a provider that can continue")
                .prompt_style,
            PromptStyle::Continuation
        );
        assert_eq!(
            negotiate(&cannot, &continuing)
                .expect("a provider that cannot continue still runs")
                .prompt_style,
            PromptStyle::Composed
        );
    }

    #[test]
    fn the_identity_prefixes_are_the_union_over_every_provider_this_build_knows() {
        let prefixes = identity_prefixes();

        assert!(prefixes.contains(&"CLAUDE"));
    }
}
