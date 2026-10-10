//! Every prompt input has a class (ADR-0032's amendment of 2026-10-10, task
//! 073, seam-contract D37).
//!
//! The canary test: every team-writable text field D37's inventory lists is
//! written, through the services, as `CANARY-<field>`. Then every prompt a run
//! of the task can be sent is composed from the context the claiming runner
//! reads. In a shared team a class 1 canary (consent-gated) and a class 3 one
//! (constrained on write, so it appears as its validated value) must each
//! appear, and a class 4 one must not. Any other canary in a prompt fails the
//! test too: a field added later is decided in D37 before it is listed here.
//!
//! A runner is a writer too: it is one member's machine, and what it reports
//! is rendered on another's. The second case writes the runner's reports as
//! canaries through the board port, from one runner, and composes from the
//! other's context.
//!
//! The solo case writes the same canaries in a personal team, where every
//! author is the runner's owner, and asserts the prompts are what they were
//! before task 073.

use pretty_assertions::assert_eq;
use rimaia_core::board::{ClaimTarget, FinishRun, PreviewOf, RunContext, StartRun, TranscriptEnd};
use rimaia_core::db::{
    new_id, settings, BoardColumn, ExitClass, RunKind, RunStatus, StrategySource,
};
use rimaia_core::repo::{self, NewRepository, RepositoryPatch};
use rimaia_core::review_loop::config as review_config;
use rimaia_core::runner::events::TokenUsage;
use rimaia_core::runner::outcome::{RunOutcome, SpawnedAs};
use rimaia_core::runner::prompt::{
    compose_fix_continuation, compose_fix_prompt, compose_fix_resume, compose_prompt,
    compose_resume_prompt, compose_review_prompt, compose_review_resume,
    compose_review_system_append, compose_strategy_prompt, compose_strategy_system_append,
    compose_system_append, PromptTeam, StrategyGuidance,
};
use rimaia_core::runner::provider::ClaudeProvider;
use rimaia_core::runner::{RunTrigger, RunnerConfig};
use rimaia_core::runs::bundle::{BundleFile, PatchInclusion, ReviewBundle};
use rimaia_core::strategy::catalogue;
use rimaia_core::tasks::{
    self, NewTask, NewTaskLink, StrategyPhase, StrategyPlan, StrategyWorkflow,
};
use rimaia_core::testing::shared::SharedTeam;
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::worktree::DiffStat;
use rimaia_core::{consent, AppPaths, ErrorCode, ServiceContext};

/// Class 1: consent-gated content. Each must reach a prompt.
const CONSENTED: [&str; 10] = [
    "title",
    "plan",
    "extra_instructions",
    "link_label",
    "link_url",
    "phase_name",
    "phase_summary",
    "base_instructions",
    "review_instructions",
    "review_override",
];

/// Class 3: constrained on write. Each reaches a prompt as its validated
/// value, which for a canary is the canary itself.
const CONSTRAINED: [&str; 3] = ["model_id", "effort_id", "default_branch"];

/// Class 4: none of these. Never in a shared team's prompt.
const UNRENDERED: [&str; 3] = ["model_label", "effort_label", "repository_name"];

const SET_STRATEGY_TOOL: &str = "mcp__rimaia-run__set_task_strategy";
const RECORD_TOOL: &str = "mcp__rimaia-run__record_review_findings";
const RESOLVE_TOOL: &str = "mcp__rimaia-run__resolve_review_finding";
const FANOUT_NOUN: &str = "subagents";

fn canary(field: &str) -> String {
    format!("CANARY-{field}")
}

#[tokio::test]
async fn a_shared_team_prompt_carries_no_ungated_team_text() {
    let team = SharedTeam::new().await;
    let task = write_canaries(&team.alice.ctx, &team.repository.id).await;
    tasks::assign_task(&team.alice.ctx, &task, Some(&team.bob.user_id))
        .await
        .expect("assign the task to Bob");
    // Every class 1 piece is Alice's, and Bob trusts her: the claim passes,
    // so what is composed is what Bob's machine would run.
    consent::set_trust(&team.bob.ctx, &team.team_id, &team.alice.user_id, true)
        .await
        .expect("Bob trusts Alice");
    let board = team.board(&team.bob);
    let claimed = board
        .claim(run_now(&task))
        .await
        .expect("Bob's runner may take it")
        .expect("nobody else holds it");

    let context = board
        .run_context(&claimed.lease)
        .await
        .expect("the context under the lease");
    let mut prompts = every_prompt(&context);
    // The task's override replaces the team's review instructions, so the
    // review is composed once with each.
    write_review_override(&team.alice.ctx, &task).await;
    let overridden = board
        .run_context(&claimed.lease)
        .await
        .expect("the context after the override");
    prompts.extend(every_prompt(&overridden));
    let all = prompts
        .iter()
        .map(|(_, prompt)| prompt.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    for field in CONSENTED.iter().chain(&CONSTRAINED) {
        assert!(all.contains(&canary(field)), "{field} reaches no prompt");
    }
    for (kind, prompt) in &prompts {
        for field in UNRENDERED {
            assert!(
                !prompt.contains(&canary(field)),
                "the {kind} prompt carries {field}:\n{prompt}"
            );
        }
        for field in canaries_in(prompt) {
            assert!(
                CONSENTED.contains(&field.as_str()) || CONSTRAINED.contains(&field.as_str()),
                "the {kind} prompt carries {field}, which has no class in D37:\n{prompt}"
            );
        }
    }
    // The stand-in for `{{repo.name}}` is the repository's id.
    assert!(
        prompts[0].1.contains(&format!(
            "CANARY-base_instructions: CANARY-title | {} | CANARY-default_branch",
            team.repository.id
        )),
        "{}",
        prompts[0].1
    );
}

/// What a runner reports and a later prompt renders: the branch, the base,
/// the commits and the bundle's paths. A runner is one member's machine, so
/// each is checked where it is written, and a bundle another runner recorded
/// is rendered as counts only (D37).
const REPORTED: [&str; 5] = ["branch", "base_ref", "base_sha", "head_sha", "bundle_path"];

/// A full sha, the only commit the board stores.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

#[tokio::test]
async fn a_runners_reports_carry_no_text_into_another_runners_prompt() {
    let team = SharedTeam::new().await;
    let task = write_canaries(&team.alice.ctx, &team.repository.id).await;
    tasks::assign_task(&team.alice.ctx, &task, Some(&team.alice.user_id))
        .await
        .expect("assign the task to Alice");
    let alice = team.board(&team.alice);
    let claimed = alice
        .claim(run_now(&task))
        .await
        .expect("Alice's runner may take it")
        .expect("nobody else holds it");
    let lease = &claimed.lease;

    // Each write that would carry text is refused, and nothing is stored.
    let refused = alice
        .record_branch(lease, &canary("branch"))
        .await
        .expect_err("a branch that is text");
    assert_eq!(refused.code(), ErrorCode::Invalid, "{refused}");
    for start in [
        starting(Some(canary("base_ref")), Some(COMMIT.to_string())),
        starting(Some(canary("default_branch")), Some(canary("base_sha"))),
    ] {
        let refused = alice
            .start_run(lease, start)
            .await
            .expect_err("a base that is text");
        assert_eq!(refused.code(), ErrorCode::Invalid, "{refused}");
    }

    // What `worktree::naming` builds, the default branch and full shas pass.
    let branch = format!("rimaia/{task}-canary-title");
    alice
        .record_branch(lease, &branch)
        .await
        .expect("the branch naming built");
    let start = starting(Some(canary("default_branch")), Some(COMMIT.to_string()));
    let run_id = start.run_id.clone();
    alice.start_run(lease, start).await.expect("open the run");
    let refused = alice
        .finish_run(lease, &run_id, finishing(&canary("head_sha")))
        .await
        .expect_err("a head that is text");
    assert_eq!(refused.code(), ErrorCode::Invalid, "{refused}");
    alice
        .finish_run(lease, &run_id, finishing(&"f".repeat(40)))
        .await
        .expect("finish the run");

    // Alice's own runner reads its own bundle's paths.
    let own = every_prompt(
        &alice
            .preview(&task, PreviewOf::Run)
            .await
            .expect("Alice's preview"),
    );
    assert!(
        own.iter()
            .any(|(_, prompt)| prompt.contains(&canary("bundle_path"))),
        "a runner's own bundle is rendered whole"
    );

    // Bob's runner, which Alice's runner's writes reached, renders none of it.
    tasks::assign_task(&team.alice.ctx, &task, Some(&team.bob.user_id))
        .await
        .expect("assign the task to Bob");
    consent::set_trust(&team.bob.ctx, &team.team_id, &team.alice.user_id, true)
        .await
        .expect("Bob trusts Alice");
    let context = team
        .board(&team.bob)
        .preview(&task, PreviewOf::Run)
        .await
        .expect("Bob's preview");
    let change = context
        .review
        .as_ref()
        .and_then(|review| review.change.as_ref())
        .expect("Alice's run recorded a bundle");
    assert_eq!(change.files, None, "another runner's paths are withheld");
    let prompts = every_prompt(&context);
    let review = &prompts
        .iter()
        .find(|(kind, _)| *kind == "review")
        .expect("a review prompt")
        .1;
    assert!(
        review.contains("1 file changed, 1 insertion(+), 0 deletions(-).\n"),
        "the counts are rendered:\n{review}"
    );
    // The branch is the task's id and a slug of the title Bob consented to.
    assert!(
        review.contains(&format!("- Branch: {branch}\n")),
        "the branch is rendered:\n{review}"
    );
    for (kind, prompt) in prompts {
        for field in REPORTED {
            assert!(
                !prompt.contains(&canary(field)),
                "the {kind} prompt carries {field}:\n{prompt}"
            );
        }
        for field in canaries_in(&prompt) {
            assert!(
                CONSENTED.contains(&field.as_str()) || CONSTRAINED.contains(&field.as_str()),
                "the {kind} prompt carries {field}, which has no class in D37:\n{prompt}"
            );
        }
    }
}

#[tokio::test]
async fn a_solo_prompt_carries_every_team_field_as_it_did_before() {
    let harness = TestContext::new().await;
    let data = tempfile::TempDir::new().expect("a data directory");
    let paths = AppPaths::new(data.path());
    let source = TempRepo::init();
    let repository = repo::register(
        &harness.context,
        harness.machine(),
        &paths.worktrees_dir(),
        NewRepository {
            path: source.path().to_string_lossy().into_owned(),
            name: None,
            worktree_root: None,
        },
    )
    .await
    .expect("register a repository");
    let task = write_canaries(&harness.context, &repository.id).await;
    let board = harness.board(&paths, &RunnerConfig::default());
    let claimed = board
        .claim(run_now(&task))
        .await
        .expect("claim")
        .expect("nobody else holds it");

    let context = board
        .run_context(&claimed.lease)
        .await
        .expect("the context under the lease");
    let mut prompts = every_prompt(&context);
    write_review_override(&harness.context, &task).await;
    let overridden = board
        .run_context(&claimed.lease)
        .await
        .expect("the context after the override");
    prompts.extend(every_prompt(&overridden));
    let all = prompts
        .iter()
        .map(|(_, prompt)| prompt.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    for field in CONSENTED.iter().chain(&CONSTRAINED).chain(&UNRENDERED) {
        assert!(all.contains(&canary(field)), "{field} reaches no prompt");
    }

    assert_eq!(
        prompts[0],
        (
            "implementation",
            r#"# Base instructions

CANARY-base_instructions: CANARY-title | CANARY-repository_name | CANARY-default_branch
- [CANARY-link_label](https://example.com/CANARY-link_url)

# Task context

- Title: CANARY-title
- Repository: CANARY-repository_name
- Base ref: CANARY-default_branch
- Links:
  - [CANARY-link_label](https://example.com/CANARY-link_url)

# Plan

CANARY-plan

# Execution strategy

A planning run read the plan above and proposed how to execute it. Treat it as guidance from an agent that saw the whole task at once: follow it unless the code tells you otherwise once you are in it, and say so in your final message if it does.

This work fans out. Run it with subagents rather than as one linear pass, giving each only the part of the plan it needs.

Phases, in order:

1. **CANARY-phase_name** (1 agent) — CANARY-phase_summary

Rimaia does not run these phases; you do, in this session, with your own subagents.

# Extra instructions

CANARY-extra_instructions"#
                .to_string()
        )
    );
    assert_eq!(
        prompts[1],
        (
            "strategy",
            format!(
                r#"# Your job

You are choosing how another agent should execute the task below. You are not implementing it, and nothing you decide here is code.

Read the plan and answer three questions:

- **Which model should run it.** Pick from the models listed below.
- **How much reasoning effort it needs.** Pick from the effort levels listed below. Reach for the expensive end only where the plan genuinely earns it: every task in the queue is paid for out of one subscription, and effort spent on a mechanical task is effort a hard one later tonight will not have.
- **Whether the work fans out.** Most tasks do not. Propose a multi-agent workflow only when the plan holds parts that can genuinely be worked in parallel, and name those phases; the agent that implements this task will run them itself, with its own subagents.

The plan and any extra instructions below are what you are judging, not what you are carrying out.

# Task context

- Title: CANARY-title
- Repository: CANARY-repository_name
- Base ref: CANARY-default_branch
- Links:
  - [CANARY-link_label](https://example.com/CANARY-link_url)

# Plan

CANARY-plan

# Extra instructions

CANARY-extra_instructions

# Available models

- `CANARY-model_id` — CANARY-model_label

# Available effort levels

- `CANARY-effort_id` — CANARY-effort_label

# How to answer

Answer with one tool call and nothing else.

Call `{SET_STRATEGY_TOOL}` exactly once, with:

- `task_id`: `{task}` — this task, and no other. A call naming a different task is refused.
- `model`: one id from **Available models** above, copied verbatim.
- `effort`: one id from **Available effort levels** above, copied verbatim.
- `rationale`: one to three sentences on why that pairing suits this plan. A human reads it on the card.
- `workflow`: `multi_agent`, with a `phases` list, only if the work genuinely fans out into parts that can be worked in parallel. Otherwise `single_agent`, and no phases.

Then stop. Print nothing else: the tool call is the only answer that reaches Rimaia, and prose beside it is read by nobody. Do not edit files, run commands, or open a pull request."#
            )
        )
    );
    assert!(
        prompts.iter().any(|(kind, prompt)| *kind == "review"
            && prompt.contains(
                "# Review instructions\n\nCANARY-review_override in CANARY-repository_name"
            )),
        "the override expands `{{{{repo.name}}}}` to the name in solo"
    );
}

/// Writes every team-writable text field D37's inventory lists, each as its
/// canary, through the services a member's doors call. Returns the task.
async fn write_canaries(ctx: &ServiceContext, repository_id: &str) -> String {
    settings::set_base_instructions(
        ctx,
        "CANARY-base_instructions: {{task.title}} | {{repo.name}} | {{repo.default_branch}}\n\
         {{task.links}}",
    )
    .await
    .expect("write the base instructions");
    review_config::set_review_settings(
        ctx,
        &ClaudeProvider,
        "CANARY-review_instructions in {{repo.name}}",
        serde_json::Value::Null,
    )
    .await
    .expect("write the team's review instructions");
    catalogue::set_catalogue(
        ctx,
        r#"{
          "models": [{"id": "CANARY-model_id", "label": "CANARY-model_label"}],
          "efforts": [{"id": "CANARY-effort_id", "label": "CANARY-effort_label"}]
        }"#,
    )
    .await
    .expect("write the catalogue");
    repo::update(
        ctx,
        repository_id,
        RepositoryPatch {
            name: Some(canary("repository_name")),
            default_branch: Some(canary("default_branch")),
        },
    )
    .await
    .expect("rename the repository and its default branch");

    let task = tasks::create_task(
        ctx,
        NewTask {
            repository_id: repository_id.to_string(),
            title: canary("title"),
            plan: Some(canary("plan")),
            extra_instructions: Some(canary("extra_instructions")),
            column: Some(BoardColumn::Ready),
            links: vec![NewTaskLink {
                label: canary("link_label"),
                url: format!("https://example.com/{}", canary("link_url")),
            }],
        },
    )
    .await
    .expect("create the task");

    let mut plan = StrategyPlan::proposed(Some(canary("model_id")), Some(canary("effort_id")));
    plan.workflow = Some(StrategyWorkflow::MultiAgent);
    plan.phases = vec![StrategyPhase {
        name: canary("phase_name"),
        model: None,
        effort: None,
        agents: 1,
        summary: canary("phase_summary"),
    }];
    tasks::set_task_strategy(ctx, &task.id, plan, StrategySource::User)
        .await
        .expect("record the strategy");
    task.id
}

async fn write_review_override(ctx: &ServiceContext, task: &str) {
    review_config::set_task_review(
        ctx,
        &ClaudeProvider,
        task,
        Some("CANARY-review_override in {{repo.name}}".to_string()),
        serde_json::Value::Null,
    )
    .await
    .expect("write the task's review instructions");
}

/// Every prompt and system append a run of the context's task can be sent,
/// each composed as the runner composes it.
fn every_prompt(context: &RunContext) -> Vec<(&'static str, String)> {
    let task = &context.task;
    let repository = &context.repository;
    let team = PromptTeam::of(context);
    let review = context.review.as_ref().expect("the loop's context");
    let guidance = StrategyGuidance::for_task(task);
    vec![
        (
            "implementation",
            compose_prompt(
                &context.base_instructions,
                task,
                repository,
                team,
                guidance.as_ref(),
                FANOUT_NOUN,
            ),
        ),
        (
            "strategy",
            compose_strategy_prompt(
                task,
                repository,
                team,
                &context.catalogue,
                SET_STRATEGY_TOOL,
                FANOUT_NOUN,
            ),
        ),
        (
            "review",
            compose_review_prompt(task, repository, team, review, RECORD_TOOL),
        ),
        (
            "fix",
            compose_fix_prompt(
                &context.base_instructions,
                task,
                repository,
                team,
                review,
                RESOLVE_TOOL,
            ),
        ),
        (
            "fix continuation",
            compose_fix_continuation(task, review, RESOLVE_TOOL),
        ),
        ("resume", compose_resume_prompt(task)),
        (
            "review resume",
            compose_review_resume(task, RECORD_TOOL, false),
        ),
        (
            "review resume, recorded",
            compose_review_resume(task, RECORD_TOOL, true),
        ),
        ("fix resume", compose_fix_resume(task, RESOLVE_TOOL)),
        (
            "system append",
            compose_system_append(task, repository, team),
        ),
        (
            "strategy system append",
            compose_strategy_system_append(&task.task.id, SET_STRATEGY_TOOL),
        ),
        (
            "review system append",
            compose_review_system_append(RECORD_TOOL),
        ),
    ]
}

/// The field of every `CANARY-<field>` in `prompt`.
fn canaries_in(prompt: &str) -> Vec<String> {
    prompt
        .split("CANARY-")
        .skip(1)
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '_')
                .collect()
        })
        .collect()
}

fn starting(base_ref: Option<String>, base_sha: Option<String>) -> StartRun {
    StartRun {
        run_id: new_id(),
        kind: RunKind::Implementation,
        session_id: "session-1".to_string(),
        prompt: "do the work".to_string(),
        base_ref,
        base_sha,
    }
}

/// A successful finish at `head`, with a bundle of one file whose path is a
/// canary.
fn finishing(head: &str) -> FinishRun {
    FinishRun {
        outcome: RunOutcome {
            exit_class: ExitClass::Success,
            status: RunStatus::Succeeded,
            error_message: None,
            num_turns: Some(4),
            cost_usd: Some(0.25),
            duration_ms: Some(1_000),
            pr_url: None,
            usage_limit_resets_at: None,
            resume_after: None,
            spawned_as: SpawnedAs::default(),
            usage: TokenUsage::default(),
        },
        head_sha: Some(head.to_string()),
        bundle: Some(ReviewBundle {
            diff: DiffStat {
                files_changed: 1,
                insertions: 1,
                deletions: 0,
            },
            files: vec![BundleFile {
                path: canary("bundle_path"),
                insertions: Some(1),
                deletions: Some(0),
                patch: PatchInclusion::Included,
            }],
            commits: vec![],
            patch: String::new(),
            patch_bytes: 0,
            patch_truncated: false,
        }),
        window_closes_at: None,
        transcript: TranscriptEnd::Complete { length: 0 },
        ceiling: Default::default(),
    }
}

fn run_now(task: &str) -> ClaimTarget {
    ClaimTarget::Run {
        task_id: task.to_string(),
        trigger: RunTrigger::Queued,
        continue_session: false,
        ceiling: Default::default(),
    }
}
