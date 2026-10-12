//! What bounds a run: the stricter of the team's limits and this runner's
//! (ADR-0028 point 2, task 042).
//!
//! # Two halves, one rule
//!
//! `max_turns` and `disallowed_tools` each have a team value and a runner
//! value. The team's is read board-side, once, where `board::service` builds
//! [`TeamLimits`], and reaches a run only as `RunContext::limits`. The
//! runner's is an override this machine keeps for itself in `runner_settings`,
//! read through [`runner_limits`] when each run starts and never cached across
//! runs. [`effective`] and [`planner_max_turns`] combine them, and nothing
//! else does: a run is never built from either half alone.
//!
//! **The runner can only add.** It may lower the turn budget and add rules to
//! the blocklist. Nothing in [`RunnerLimits`] can remove a team rule or raise a
//! budget, and the type makes no room for it: there is no "allow" list and no
//! way to say "more turns".
//!
//! # The runner half is never adopted
//!
//! The board's `settings` table holds the team's values under the same two
//! names, and task 040's adoption copies only runner-placed keys. These two
//! stay out of `db::settings::RUNNER_KEYS`, so adoption never copies the
//! team's value into the runner's override, against seam-contract D28 part
//! 4's "the runner's stricter override starts out absent". There is no
//! command, no MCP tool and no UI for them yet: they are set in the `sqlite3`
//! CLI (ADR-0003), and a control is task 061's.

use crate::board::TeamLimits;
use crate::error::Result;
use crate::machine::MachineContext;
use crate::runner::provider::{claude, ForbiddenOperation, ProviderId};

/// The settings key holding the tool blocklist (ADR-0012 point 3: "the list is
/// a setting so it can grow with experience"). The team's value in
/// `team_settings`, and the runner's additions in `runner_settings`.
///
/// One pattern per line rather than comma-separated: a pattern contains
/// spaces and parentheses already, and a line is the one separator that
/// cannot appear inside one. Blank lines are ignored, so a stored value stays
/// readable in the `sqlite3` CLI. The key's vocabulary is the active
/// provider's own rule language, which is why each rule is tagged with the
/// provider when it becomes a [`ForbiddenOperation`].
pub const DISALLOWED_TOOLS: &str = "disallowed_tools";

/// The settings key holding the per-attempt turn budget (ADR-0011:
/// "`--max-turns` per attempt bounds runaway loops"). The team's budget in
/// `team_settings`, and the runner's lower one in `runner_settings`.
pub const MAX_TURNS: &str = "max_turns";

/// How many turns one attempt may take when the team has not set a budget.
///
/// Chosen from two constraints pulling in opposite directions. A turn limit is
/// `ExitClass::Fatal` (`runner::outcome`'s rule 4, and ADR-0011's fatal row
/// names it): a budget set too low does not cost a retry, it **abandons the
/// task**, half-done, with a card that says "failed" for a reason the operator
/// did not choose. And a budget set too high does not bound the runaway
/// ADR-0011 wants bounded. The spike's recorded runs took four to forty turns
/// for one-file work, so a substantial overnight plan plausibly wants a few
/// hundred; three hundred is comfortably above honest work and far below a loop
/// that has stopped making progress.
///
/// **This changes every implementation run's argv**, which is why
/// `tests/runner_process.rs` asserts the vector with `--max-turns 300` in it
/// rather than without: before task 014 the flag was never passed at all, and
/// the CLI's own default applied.
pub const DEFAULT_MAX_TURNS: u32 = 300;

/// Rimaia's operator tool surface, denied to every run the runner spawns
/// whatever the operator's configuration says.
///
/// # Why this is not part of the blocklist setting
///
/// That list is configuration, and an explicitly empty setting means an empty
/// list — the operator is allowed to turn it off. This is not configuration. It
/// closes a hole that would otherwise make [`RunScope`](crate::mcp::RunScope)
/// decorative.
///
/// # The hole
///
/// `run_environment` defaults to `inherit` (ADR-0004's amendment), and ADR-0006
/// tells the operator to register Rimaia with `claude mcp add`. So an
/// implementation run's session loads the **operator's unscoped `/mcp`**, and
/// ADR-0012 gives that run `bypassPermissions`, which — unlike `acceptEdits` —
/// auto-approves MCP calls. The run would hold `move_task`, `create_task`,
/// `set_task_dependencies`, and every ADR-0021 configuration tool: exactly the
/// rows `Tool::run_access` marks `Refused`. A prompt-injected run could mark its
/// own card `done`, or change the model every future run uses, with no bash
/// involved at all.
///
/// # Named as an intent, spelled by the provider
///
/// Which tool names that is, and whether a whole server can be denied at once,
/// is one provider's business (ADR-0026 point 4). What Rimaia states is the
/// operation.
///
/// # Every run, and never a run's own handle
///
/// The denial works by tool name, so a run's own scoped handle is served under
/// a different server name, `rimaia-run`, and this is spelled at the operator's
/// `rimaia` only. That is what lets it be unconditional: [`effective`] appends
/// it for every intent, implementation, planner, review or fix, and no caller
/// can drop it. Seam-contract D30 points 1 and 2 decide it, and
/// `run-scoped-server-name.jsonl` records the CLI matching the server segment
/// exactly rather than as a prefix.
const RIMAIA_TOOL_SURFACE: ForbiddenOperation = ForbiddenOperation::RimaiaToolSurface;

/// This runner's half: an override that can only make a run stricter.
///
/// Both fields are empty by default, and empty means no override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunnerLimits {
    /// A lower turn budget than the team's, or `None` to take the team's.
    pub max_turns: Option<u32>,
    /// Rules this runner forbids on top of the team's, in the active
    /// provider's vocabulary.
    pub disallowed_tools: Vec<String>,
}

/// What a process is spawned with: the stricter of the two halves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveLimits {
    pub max_turns: u32,
    /// In a fixed order, team then runner then the caller's own then the
    /// operator surface, so the argv is deterministic.
    pub forbidden: Vec<ForbiddenOperation>,
}

/// The stricter of `team` and `runner`, as one process's limits.
///
/// - **`max_turns`** is the lower of the two when the runner set one, and the
///   team's otherwise.
/// - **The forbidden operations** are the team's first: ADR-0012 point 3's
///   three defaults when the team's blocklist is `None`, and one
///   `ProviderRule` per stored rule otherwise, an explicitly empty list
///   included (D27). Then the runner's rules that are not already present,
///   tagged with `provider`. Then `extra`, the caller's own (the review's
///   `AnyFileMutation`, the planner's denials). Then the operator tool
///   surface, which every intent carries and no caller can drop.
pub fn effective(
    team: &TeamLimits,
    runner: &RunnerLimits,
    provider: ProviderId,
    extra: impl IntoIterator<Item = ForbiddenOperation>,
) -> EffectiveLimits {
    let rule = |rule: &String| ForbiddenOperation::ProviderRule {
        provider,
        rule: rule.clone(),
    };

    let mut forbidden: Vec<ForbiddenOperation> = match &team.disallowed_tools {
        None => claude::DEFAULT_FORBIDDEN.to_vec(),
        Some(rules) => rules.iter().map(rule).collect(),
    };
    for addition in runner.disallowed_tools.iter().map(rule) {
        if !forbidden.contains(&addition) {
            forbidden.push(addition);
        }
    }
    forbidden.extend(extra);
    forbidden.push(RIMAIA_TOOL_SURFACE);

    EffectiveLimits {
        max_turns: runner
            .max_turns
            .map_or(team.max_turns, |runner| runner.min(team.max_turns)),
        forbidden,
    }
}

/// A planner's turn budget: the catalogue's, or the runner's when that is
/// lower.
///
/// The team's `max_turns` caps only the runs it capped before task 042, so a
/// solo planner is unchanged. Its forbidden operations still come from
/// [`effective`].
pub fn planner_max_turns(catalogue: u32, runner: &RunnerLimits) -> u32 {
    runner
        .max_turns
        .map_or(catalogue, |runner| runner.min(catalogue))
}

/// This runner's override, read from its own store (D3's typed accessor).
///
/// Tolerant, like every other stored key, for ADR-0003's reason: a
/// `max_turns` that is unparseable or `0` warns and reads as absent, never as
/// `0` (which would abandon every task at its first turn) and never as the
/// team's value. The blocklist is one pattern per line, blank lines ignored.
///
/// Read straight off the store rather than through `db::settings::get_runner`,
/// whose placement check is keyed by name: the board places these two names
/// with the team, and that is the point, since the runner's keys are
/// overrides of the team's and are never adopted from them.
pub async fn runner_limits(machine: &MachineContext) -> Result<RunnerLimits> {
    let max_turns = match machine.store.get_setting(MAX_TURNS).await? {
        None => None,
        Some(stored) => match stored.trim().parse::<u32>() {
            Ok(0) | Err(_) => {
                tracing::warn!(
                    value = stored,
                    "unusable runner max_turns override; the team's budget applies"
                );
                None
            }
            Ok(value) => Some(value),
        },
    };
    let disallowed_tools = machine
        .store
        .get_setting(DISALLOWED_TOOLS)
        .await?
        .map(|stored| rules(&stored))
        .unwrap_or_default();

    Ok(RunnerLimits {
        max_turns,
        disallowed_tools,
    })
}

/// A stored blocklist as rules: one per non-blank line, trimmed.
pub(crate) fn rules(stored: &str) -> Vec<String> {
    stored
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const CLAUDE: ProviderId = ProviderId::ClaudeCode;

    fn team(max_turns: u32, disallowed_tools: Option<&[&str]>) -> TeamLimits {
        TeamLimits {
            max_turns,
            disallowed_tools: disallowed_tools
                .map(|rules| rules.iter().map(|rule| (*rule).to_string()).collect()),
        }
    }

    fn runner(max_turns: Option<u32>, disallowed_tools: &[&str]) -> RunnerLimits {
        RunnerLimits {
            max_turns,
            disallowed_tools: disallowed_tools
                .iter()
                .map(|rule| (*rule).to_string())
                .collect(),
        }
    }

    fn rule(rule: &str) -> ForbiddenOperation {
        ForbiddenOperation::ProviderRule {
            provider: CLAUDE,
            rule: rule.to_string(),
        }
    }

    #[test]
    fn with_no_runner_override_the_team_values_apply_unchanged() {
        assert_eq!(
            effective(
                &team(300, Some(&["Bash(rm:*)"])),
                &RunnerLimits::default(),
                CLAUDE,
                [],
            ),
            EffectiveLimits {
                max_turns: 300,
                forbidden: vec![rule("Bash(rm:*)"), ForbiddenOperation::RimaiaToolSurface],
            }
        );
    }

    #[test]
    fn a_runner_may_lower_the_turn_budget() {
        assert_eq!(
            effective(&team(300, Some(&[])), &runner(Some(40), &[]), CLAUDE, []),
            EffectiveLimits {
                max_turns: 40,
                forbidden: vec![ForbiddenOperation::RimaiaToolSurface],
            }
        );
    }

    #[test]
    fn a_runner_cannot_raise_the_turn_budget_above_the_team_ceiling() {
        assert_eq!(
            effective(&team(30, Some(&[])), &runner(Some(500), &[]), CLAUDE, []),
            EffectiveLimits {
                max_turns: 30,
                forbidden: vec![ForbiddenOperation::RimaiaToolSurface],
            }
        );
    }

    #[test]
    fn the_default_blocklist_survives_a_runner_addition() {
        // An unset team blocklist means ADR-0012 point 3's three operations,
        // and a runner adding a rule must not turn that into "only the
        // runner's rule".
        assert_eq!(
            effective(
                &team(300, None),
                &runner(None, &["Bash(curl:*)"]),
                CLAUDE,
                []
            ),
            EffectiveLimits {
                max_turns: 300,
                forbidden: vec![
                    ForbiddenOperation::RemoteHistoryRewrite,
                    ForbiddenOperation::RemoteBranchDeletion,
                    ForbiddenOperation::HardResetToRemote,
                    rule("Bash(curl:*)"),
                    ForbiddenOperation::RimaiaToolSurface,
                ],
            }
        );
    }

    #[test]
    fn an_explicitly_empty_team_blocklist_forbids_only_what_the_runner_adds() {
        assert_eq!(
            effective(
                &team(300, Some(&[])),
                &runner(None, &["Bash(curl:*)"]),
                CLAUDE,
                [],
            ),
            EffectiveLimits {
                max_turns: 300,
                forbidden: vec![rule("Bash(curl:*)"), ForbiddenOperation::RimaiaToolSurface],
            }
        );
    }

    #[test]
    fn a_rule_on_both_lists_is_forbidden_once() {
        assert_eq!(
            effective(
                &team(300, Some(&["Bash(rm:*)"])),
                &runner(None, &["Bash(rm:*)", "Bash(curl:*)", "Bash(curl:*)"]),
                CLAUDE,
                [],
            ),
            EffectiveLimits {
                max_turns: 300,
                forbidden: vec![
                    rule("Bash(rm:*)"),
                    rule("Bash(curl:*)"),
                    ForbiddenOperation::RimaiaToolSurface,
                ],
            }
        );
    }

    #[test]
    fn the_operations_are_ordered_team_then_runner_then_the_callers_own() {
        assert_eq!(
            effective(
                &team(300, Some(&["Bash(rm:*)"])),
                &runner(None, &["Bash(curl:*)"]),
                CLAUDE,
                [
                    ForbiddenOperation::AnyFileMutation,
                    ForbiddenOperation::AnyShellCommand,
                ],
            )
            .forbidden,
            vec![
                rule("Bash(rm:*)"),
                rule("Bash(curl:*)"),
                ForbiddenOperation::AnyFileMutation,
                ForbiddenOperation::AnyShellCommand,
                ForbiddenOperation::RimaiaToolSurface,
            ]
        );
    }

    #[tokio::test]
    async fn an_unusable_runner_turn_budget_reads_as_no_override() {
        // Against `MemoryMachine`: never `0`, which would abandon every task at
        // its first turn, and never the team's value, which is not this
        // runner's to copy.
        let harness = crate::testing::TestContext::new().await;
        let machine = harness.machine();

        for stored in ["0", "-1", "lots"] {
            machine
                .store
                .set_setting(MAX_TURNS, stored)
                .await
                .expect("store a hand-edited value");
            assert_eq!(
                runner_limits(machine).await.expect("read the override"),
                RunnerLimits::default(),
                "{stored:?}",
            );
        }
    }

    #[tokio::test]
    async fn a_runner_blocklist_is_one_pattern_per_line_and_both_keys_start_absent() {
        let harness = crate::testing::TestContext::new().await;
        let machine = harness.machine();
        assert_eq!(
            runner_limits(machine).await.expect("read the override"),
            RunnerLimits::default(),
        );

        machine
            .store
            .set_setting(DISALLOWED_TOOLS, "Bash(curl:*)\n\n  Bash(wget:*)  \n")
            .await
            .expect("store a blocklist");
        machine
            .store
            .set_setting(MAX_TURNS, " 40 ")
            .await
            .expect("store a budget");

        assert_eq!(
            runner_limits(machine).await.expect("read the override"),
            runner(Some(40), &["Bash(curl:*)", "Bash(wget:*)"]),
        );
    }

    #[test]
    fn the_planner_budget_is_the_lower_of_the_catalogue_and_the_runner() {
        assert_eq!(planner_max_turns(6, &RunnerLimits::default()), 6);
        assert_eq!(planner_max_turns(6, &runner(Some(3), &[])), 3);
        assert_eq!(planner_max_turns(6, &runner(Some(50), &[])), 6);
    }
}
