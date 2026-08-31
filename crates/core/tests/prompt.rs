//! What a run is actually asked to do (ADR-0009, ADR-0012, task 006).
//!
//! Every assertion here is a whole composed string, never a substring. That is
//! task 006's own instruction and it is not pedantry: ADR-0009 stores this exact
//! text on the `runs` row so a morning review can answer "what did it actually
//! get", and the Settings preview has to match it byte for byte. A heading
//! reworded by accident should redden CI rather than turn up in a transcript at
//! 2am.
//!
//! The scanner's own edge cases — a variable used twice, a value that looks like
//! a variable, an unclosed `{{` — are pure and live colocated in
//! `crates/core/src/runner/prompt.rs`, and so are the strategy envelope's. What
//! is here is composition: which sections appear, in what order, and what they
//! look like.
//!
//! Task 020 adds the strategy prompt (seam-contract D17.9) and a fifth section
//! to the implementation one (ADR-0009's 2026-08-28 amendment), and they are
//! held to the same standard for a sharper reason than the rest: the planner's
//! only way to answer is a tool call it is told about here, and a task nobody
//! planned must still compose the bytes task 006 fixed.

use chrono::{DateTime, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::{
    BoardColumn, MutationSource, Repository, RunState, StrategyMode, Task, TaskLink,
};
use rimaia_core::runner::prompt::{
    compose_prompt, compose_resume_prompt, compose_strategy_prompt, compose_strategy_system_append,
    compose_system_append, StrategyGuidance, SET_TASK_STRATEGY_TOOL,
};
use rimaia_core::strategy::{Catalogue, CatalogueEntry, StrategyOrigin};
use rimaia_core::tasks::TaskDetail;

// ---------------------------------------------------------------------------
// compose_prompt
// ---------------------------------------------------------------------------

#[test]
fn a_prompt_with_every_section_reads_in_the_order_adr_0009_fixes() {
    let composed = compose_prompt(
        "Commit as you work, with focused commits and clear messages.\n\
         Run the project's tests and linters before you finish.",
        &task(),
        &repository(),
        None,
    );

    assert_eq!(
        composed,
        r#"# Base instructions

Commit as you work, with focused commits and clear messages.
Run the project's tests and linters before you finish.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main
- Links:
  - [Asana](https://app.asana.com/0/1/2)
  - [Design doc](https://docs.example.com/board)

# Plan

## Steps

1. Read the store.
2. Wire the board to it.

# Extra instructions

Skip the migration, it already landed."#
    );
}

#[test]
fn a_task_with_no_plan_omits_the_plan_section_and_its_heading() {
    let mut task = task();
    task.task.plan = None;

    assert_eq!(
        compose_prompt("Commit as you work.", &task, &repository(), None),
        r#"# Base instructions

Commit as you work.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main
- Links:
  - [Asana](https://app.asana.com/0/1/2)
  - [Design doc](https://docs.example.com/board)

# Extra instructions

Skip the migration, it already landed."#
    );
}

#[test]
fn a_plan_of_nothing_but_whitespace_is_the_same_as_no_plan() {
    // `plan` is nullable because `not_ready` means "captured, plan missing or
    // incomplete", but a user who selected the box and hit delete leaves an
    // empty string. Both are the same absence to the agent.
    let mut blank = task();
    blank.task.plan = Some("   \n\n\t\n".to_string());
    let mut absent = task();
    absent.task.plan = None;

    assert_eq!(
        compose_prompt("Commit as you work.", &blank, &repository(), None),
        compose_prompt("Commit as you work.", &absent, &repository(), None)
    );
}

#[test]
fn a_task_with_no_extra_instructions_omits_that_section_and_its_heading() {
    let mut task = task();
    task.task.extra_instructions = None;

    assert_eq!(
        compose_prompt("Commit as you work.", &task, &repository(), None),
        r#"# Base instructions

Commit as you work.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main
- Links:
  - [Asana](https://app.asana.com/0/1/2)
  - [Design doc](https://docs.example.com/board)

# Plan

## Steps

1. Read the store.
2. Wire the board to it."#
    );
}

#[test]
fn a_task_with_no_links_omits_the_links_line_rather_than_rendering_an_empty_one() {
    let mut task = task();
    task.links = vec![];

    assert_eq!(
        compose_prompt("Commit as you work.", &task, &repository(), None),
        r#"# Base instructions

Commit as you work.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main

# Plan

## Steps

1. Read the store.
2. Wire the board to it.

# Extra instructions

Skip the migration, it already landed."#
    );
}

#[test]
fn blank_base_instructions_omit_the_base_instructions_section() {
    // The prompt then opens on the task context. Nothing else shifts.
    let mut task = task();
    task.links = vec![];
    task.task.extra_instructions = None;

    assert_eq!(
        compose_prompt("", &task, &repository(), None),
        r#"# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main

# Plan

## Steps

1. Read the store.
2. Wire the board to it."#
    );
}

#[test]
fn a_task_with_no_worktree_yet_omits_the_branch_line() {
    // What the Settings preview composes against a task task 007 has not
    // prepared. A `- Branch:` with nothing after it teaches the agent less than
    // saying nothing does.
    let mut task = task();
    task.task.branch = None;
    task.links = vec![];
    task.task.extra_instructions = None;

    assert_eq!(
        compose_prompt("Commit as you work.", &task, &repository(), None),
        r#"# Base instructions

Commit as you work.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Base ref: main

# Plan

## Steps

1. Read the store.
2. Wire the board to it."#
    );
}

#[test]
fn an_unknown_template_variable_survives_into_the_composed_prompt_verbatim() {
    // ADR-0009's reason, in one test: a typo in a settings field must cost a
    // strange sentence, not an overnight queue.
    let mut task = task();
    task.links = vec![];
    task.task.plan = None;
    task.task.extra_instructions = None;

    assert_eq!(
        compose_prompt(
            "Ask {{task.owner}} before you touch {{repo.name}}.",
            &task,
            &repository(),
            None,
        ),
        r#"# Base instructions

Ask {{task.owner}} before you touch rimaia.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main"#
    );
}

#[test]
fn every_known_template_variable_expands_in_the_base_instructions() {
    let mut task = task();
    task.task.plan = None;
    task.task.extra_instructions = None;

    assert_eq!(
        compose_prompt(
            "Task {{task.title}} lands on {{task.branch}} in {{repo.name}}, cut from {{repo.default_branch}}.\n\
             \n\
             Read these first:\n\
             \n\
             {{task.links}}",
            &task,
            &repository(),
            None,
        ),
        r#"# Base instructions

Task Wire the board to the store lands on rimaia/wire-the-board in rimaia, cut from main.

Read these first:

- [Asana](https://app.asana.com/0/1/2)
- [Design doc](https://docs.example.com/board)

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main
- Links:
  - [Asana](https://app.asana.com/0/1/2)
  - [Design doc](https://docs.example.com/board)"#
    );
}

#[test]
fn editing_base_instructions_cannot_reach_a_prompt_already_composed() {
    // The mechanism behind ADR-0009's "changing base instructions does not
    // retroactively alter past runs": composition hands back an owned `String`,
    // and task 008 stores that copy on the `runs` row. Nothing here shares
    // storage with the settings table, so there is no path from an edit to an
    // already-composed prompt.
    let task = task();
    let repository = repository();

    let stored = compose_prompt("Open a draft PR.", &task, &repository, None);
    let recomposed = compose_prompt("Never open a PR.", &task, &repository, None);

    assert!(stored.contains("Open a draft PR."));
    assert!(!stored.contains("Never open a PR."));
    assert_ne!(stored, recomposed);
}

// ---------------------------------------------------------------------------
// compose_prompt — the fifth section (ADR-0009's 2026-08-28 amendment)
// ---------------------------------------------------------------------------

#[test]
fn an_implementation_prompt_with_no_guidance_is_byte_for_byte_what_task_006_composed() {
    // The regression guard for the whole of task 020's prompt work. Three
    // spellings of "this task has no execution strategy to inject" — never
    // planned, planned and failed, planned and single-agent — and all three must
    // compose the exact bytes that were composed before the fifth section
    // existed, or task 006's preview criterion silently stops holding for every
    // task on the board.
    let expected = r#"# Base instructions

Commit as you work.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main
- Links:
  - [Asana](https://app.asana.com/0/1/2)
  - [Design doc](https://docs.example.com/board)

# Plan

## Steps

1. Read the store.
2. Wire the board to it.

# Extra instructions

Skip the migration, it already landed."#;

    assert_eq!(
        compose_prompt("Commit as you work.", &task(), &repository(), None),
        expected
    );

    let mut failed = task();
    failed.task.strategy_plan = Some(failed_proposal());
    assert_eq!(
        StrategyGuidance::for_task(&failed),
        None,
        "a planner that failed proposed nothing to inject"
    );
    assert_eq!(
        compose_prompt(
            "Commit as you work.",
            &failed,
            &repository(),
            StrategyGuidance::for_task(&failed).as_ref()
        ),
        expected
    );

    let mut single_agent = task();
    single_agent.task.strategy_plan = Some(single_agent_proposal());
    assert_eq!(
        StrategyGuidance::for_task(&single_agent),
        None,
        "a single-agent proposal with no phases has nothing a prompt could say"
    );
    assert_eq!(
        compose_prompt(
            "Commit as you work.",
            &single_agent,
            &repository(),
            StrategyGuidance::for_task(&single_agent).as_ref()
        ),
        expected
    );
}

#[test]
fn a_multi_agent_proposal_lands_between_the_plan_and_the_extra_instructions() {
    // Where it goes is the decision ADR-0009's amendment argues, so the whole
    // string is the assertion: guidance after the plan it is about, and before
    // the sentence the user typed for this one task.
    //
    // What is *not* in it matters as much. The proposal below names a model and
    // an effort, at the top level and again on every phase; none of the five
    // appears here. They are `--model` and `--effort`, applied to this process
    // before it read a word, and prose restating them would invite an argument
    // with a decision the session cannot change.
    let mut task = task();
    task.task.strategy_plan = Some(multi_agent_proposal());
    let guidance = StrategyGuidance::for_task(&task).expect("a multi-agent proposal is guidance");

    assert_eq!(
        compose_prompt("Commit as you work.", &task, &repository(), Some(&guidance)),
        r#"# Base instructions

Commit as you work.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main
- Links:
  - [Asana](https://app.asana.com/0/1/2)
  - [Design doc](https://docs.example.com/board)

# Plan

## Steps

1. Read the store.
2. Wire the board to it.

# Execution strategy

A planning run read the plan above and proposed how to execute it. Treat it as guidance from an agent that saw the whole task at once: follow it unless the code tells you otherwise once you are in it, and say so in your final message if it does.

This work fans out. Run it with subagents rather than as one linear pass, giving each only the part of the plan it needs.

Phases, in order:

1. **Schema** (1 agent) — Add the columns and the migration.
2. **Wiring** (3 agents) — Thread the new field through every caller.

Rimaia does not run these phases; you do, in this session, with your own subagents.

# Extra instructions

Skip the migration, it already landed."#
    );
}

// ---------------------------------------------------------------------------
// compose_strategy_prompt (task 020, seam-contract D17.9)
// ---------------------------------------------------------------------------

#[test]
fn the_strategy_prompt_has_exactly_the_sections_task_020_specifies() {
    assert_eq!(
        compose_strategy_prompt(&task(), &repository(), &Catalogue::default()),
        r#"# Your job

You are choosing how another agent should execute the task below. You are not implementing it, and nothing you decide here is code.

Read the plan and answer three questions:

- **Which model should run it.** Pick from the models listed below.
- **How much reasoning effort it needs.** Pick from the effort levels listed below. Reach for the expensive end only where the plan genuinely earns it: every task in the queue is paid for out of one subscription, and effort spent on a mechanical task is effort a hard one later tonight will not have.
- **Whether the work fans out.** Most tasks do not. Propose a multi-agent workflow only when the plan holds parts that can genuinely be worked in parallel, and name those phases; the agent that implements this task will run them itself, with its own subagents.

The plan and any extra instructions below are what you are judging, not what you are carrying out.

# Task context

- Title: Wire the board to the store
- Repository: rimaia
- Branch: rimaia/wire-the-board
- Base ref: main
- Links:
  - [Asana](https://app.asana.com/0/1/2)
  - [Design doc](https://docs.example.com/board)

# Plan

## Steps

1. Read the store.
2. Wire the board to it.

# Extra instructions

Skip the migration, it already landed.

# Available models

- `opus` — Opus
- `sonnet` — Sonnet
- `haiku` — Haiku

# Available effort levels

- `low` — Low
- `medium` — Medium
- `high` — High
- `xhigh` — Extra high
- `max` — Max

# How to answer

Answer with one tool call and nothing else.

Call `mcp__rimaia__set_task_strategy` exactly once, with:

- `task_id`: `3f2b1c00-0000-4000-8000-000000000001` — this task, and no other. A call naming a different task is refused.
- `model`: one id from **Available models** above, copied verbatim.
- `effort`: one id from **Available effort levels** above, copied verbatim.
- `rationale`: one to three sentences on why that pairing suits this plan. A human reads it on the card.
- `workflow`: `multi_agent`, with a `phases` list, only if the work genuinely fans out into parts that can be worked in parallel. Otherwise `single_agent`, and no phases.

Then stop. Print nothing else: the tool call is the only answer that reaches Rimaia, and prose beside it is read by nobody. Do not edit files, run commands, or open a pull request."#
    );
}

#[test]
fn the_strategy_prompt_never_carries_the_base_instructions() {
    // There is no parameter to pass them through, so what is left to assert is
    // that no section quietly reintroduces implementation workflow. Base
    // instructions are where a run is told to commit, push and open a pull
    // request, and a planner that does any of those is a defect — the only
    // mention of one here is the prohibition in `# How to answer`.
    let composed = compose_strategy_prompt(&task(), &repository(), &Catalogue::default());

    assert_eq!(
        headings(&composed),
        [
            "# Your job",
            "# Task context",
            "# Plan",
            "# Extra instructions",
            "# Available models",
            "# Available effort levels",
            "# How to answer",
        ],
        "seam-contract D17.9's list, and `# Base instructions` is not on it"
    );
    // Nothing in this fixture's own plan or extra instructions says "commit"
    // either, so the word appearing at all would mean composition put it there.
    assert!(
        !composed.to_lowercase().contains("commit"),
        "the planner is never told to commit: {composed}"
    );
    assert!(
        composed.ends_with("Do not edit files, run commands, or open a pull request."),
        "the last thing the planner reads is what it must not do: {composed}"
    );
}

#[test]
fn the_strategy_prompt_names_the_task_id_and_the_write_back_tool() {
    // The planner's whole output is one call, and both halves of addressing it
    // are composed here: the wired tool name, and the id of the task it may
    // write to. The scoped handle refuses any other id (seam-contract D17.4),
    // so a prompt that names the wrong one buys a refusal instead of a run.
    let mut task = task();
    task.task.id = "3f2b1c00-0000-4000-8000-0000000000ff".to_string();

    let composed = compose_strategy_prompt(&task, &repository(), &Catalogue::default());

    assert_eq!(
        section(&composed, "# How to answer"),
        r#"Answer with one tool call and nothing else.

Call `mcp__rimaia__set_task_strategy` exactly once, with:

- `task_id`: `3f2b1c00-0000-4000-8000-0000000000ff` — this task, and no other. A call naming a different task is refused.
- `model`: one id from **Available models** above, copied verbatim.
- `effort`: one id from **Available effort levels** above, copied verbatim.
- `rationale`: one to three sentences on why that pairing suits this plan. A human reads it on the card.
- `workflow`: `multi_agent`, with a `phases` list, only if the work genuinely fans out into parts that can be worked in parallel. Otherwise `single_agent`, and no phases.

Then stop. Print nothing else: the tool call is the only answer that reaches Rimaia, and prose beside it is read by nobody. Do not edit files, run commands, or open a pull request."#
    );
    assert_eq!(
        SET_TASK_STRATEGY_TOOL, "mcp__rimaia__set_task_strategy",
        "the constant the runner hands to the system append is the name the prompt spells"
    );
}

#[test]
fn the_strategy_prompt_lists_every_model_and_effort_in_the_catalogue() {
    // ADR-0016's "a new model must not require a release", from the prompt's
    // side: ids nobody compiled reach the planner because the catalogue is
    // configuration, and the label goes with them so a planner choosing between
    // two efforts has the words the operator chose for them.
    let catalogue = Catalogue {
        models: vec![entry("opus-5", "Opus 5"), entry("sonnet-5", "Sonnet 5")],
        efforts: vec![entry("ludicrous", "Ludicrous")],
        ..Catalogue::default()
    };

    let composed = compose_strategy_prompt(&task(), &repository(), &catalogue);

    assert_eq!(
        section(&composed, "# Available models"),
        "- `opus-5` — Opus 5\n- `sonnet-5` — Sonnet 5"
    );
    assert_eq!(
        section(&composed, "# Available effort levels"),
        "- `ludicrous` — Ludicrous"
    );

    // An operator who emptied a list has said "offer nothing", and an empty
    // section is omitted with its heading like every other one — the same rule
    // the catalogue's own reader states about `"models": []`.
    let emptied = Catalogue {
        models: vec![],
        ..Catalogue::default()
    };

    assert!(
        !compose_strategy_prompt(&task(), &repository(), &emptied).contains("# Available models"),
        "an empty list is an omitted section, not an empty one"
    );
}

// ---------------------------------------------------------------------------
// compose_strategy_system_append
// ---------------------------------------------------------------------------

#[test]
fn the_strategy_system_append_states_the_facts_adr_0012_reserves_it_for() {
    // The planner's own three: unattended and unanswerable, writes nothing, and
    // answers with one call for one task. The task id is here as well as in the
    // prompt because ADR-0012's amendment puts it on the channel the run may not
    // treat as negotiable.
    assert_eq!(
        compose_strategy_system_append(
            "3f2b1c00-0000-4000-8000-000000000001",
            SET_TASK_STRATEGY_TOOL
        ),
        "You are running unattended, started by Rimaia to decide how one task should be executed. What follows is how this session works, not a preference you may weigh against the job.\n\
         \n\
         - Nobody is watching and nobody can answer a question. There is no interactive terminal, and anything you ask will go unread until a human reviews this transcript, which may be many hours from now.\n\
         - You are not implementing anything. Write no files, run no commands, commit nothing, and open no pull request. The tools that would let you are denied for this session.\n\
         - Your whole answer is one call to `mcp__rimaia__set_task_strategy`, made exactly once, for the task `3f2b1c00-0000-4000-8000-000000000001` and no other. Anything you print instead of that call is read by nobody."
    );
}

#[test]
fn the_strategy_system_append_carries_no_plan_and_no_catalogue() {
    // ADR-0012 splits the channels for the strategy run too: what the task is,
    // and what may be chosen for it, belong in the prompt where the planner may
    // reason about them.
    let composed = compose_strategy_system_append(
        "3f2b1c00-0000-4000-8000-000000000001",
        SET_TASK_STRATEGY_TOOL,
    );

    assert!(!composed.contains("Read the store"), "the plan leaked");
    assert!(!composed.contains("# "), "a prompt section heading leaked");
    assert!(!composed.contains("Sonnet"), "the catalogue leaked");
}

// ---------------------------------------------------------------------------
// compose_system_append
// ---------------------------------------------------------------------------

#[test]
fn the_system_append_states_the_four_orchestrator_facts_adr_0012_reserves_it_for() {
    assert_eq!(
        compose_system_append(&task(), &repository()),
        "You are running unattended, started by Rimaia. What follows is how this session works, not a preference you may weigh against the task.\n\
         \n\
         - Nobody is watching and nobody can answer a question. There is no interactive terminal, and anything you ask will go unread until a human reviews this transcript, which may be many hours from now.\n\
         - You are working in a git worktree of your own, on the branch `rimaia/wire-the-board`. Commit and push there. Never push to `main`, this repository's default branch, and never rewrite history that is already on the remote.\n\
         - If you cannot proceed, stop and report rather than guess. Commit what you have and state plainly what blocked you. A clear blocker is a useful outcome; a guess dressed as a decision is not."
    );
}

#[test]
fn the_system_append_names_the_repositorys_own_default_branch_not_a_guess_at_main() {
    let repository = Repository {
        default_branch: "trunk".to_string(),
        ..repository()
    };

    assert!(compose_system_append(&task(), &repository).contains(
        "Never push to `trunk`, this repository's default branch, and never rewrite history"
    ));
}

#[test]
fn the_system_append_says_where_to_work_even_before_a_branch_exists() {
    let mut task = task();
    task.task.branch = None;

    assert_eq!(
        compose_system_append(&task, &repository()),
        "You are running unattended, started by Rimaia. What follows is how this session works, not a preference you may weigh against the task.\n\
         \n\
         - Nobody is watching and nobody can answer a question. There is no interactive terminal, and anything you ask will go unread until a human reviews this transcript, which may be many hours from now.\n\
         - You are working in a git worktree of your own, on the branch Rimaia created for this task. Commit and push there. Never push to `main`, this repository's default branch, and never rewrite history that is already on the remote.\n\
         - If you cannot proceed, stop and report rather than guess. Commit what you have and state plainly what blocked you. A clear blocker is a useful outcome; a guess dressed as a decision is not."
    );
}

#[test]
fn the_system_append_carries_no_plan_and_no_base_instructions() {
    // ADR-0009 splits the channels on purpose, and both strings reach the model,
    // so the only thing that keeps them apart is a test. Base instructions and
    // the plan belong in the prompt, where a human reading the transcript can
    // see what the agent was asked and reason about why it did not do it.
    let composed = compose_system_append(&task(), &repository());

    assert!(!composed.contains("Read the store"), "the plan leaked");
    assert!(!composed.contains("# "), "a prompt section heading leaked");
    assert!(
        !composed.contains("Commit as you work"),
        "base instructions leaked"
    );
}

// ---------------------------------------------------------------------------
// compose_resume_prompt
// ---------------------------------------------------------------------------

#[test]
fn a_resume_prompt_points_at_the_session_instead_of_repeating_the_plan() {
    assert_eq!(
        compose_resume_prompt(&task()),
        "Continue the task \"Wire the board to the store\" from where you stopped. \
         The instructions and the plan are earlier in this session — do not start over \
         and do not ask for them again. If you cannot proceed, stop and report what blocked you."
    );
}

#[test]
fn a_resume_prompt_repeats_neither_the_plan_nor_the_base_instructions() {
    // ADR-0011's "retries resume, they do not restart", asserted as the property
    // the spike measured: a one-line continuation, not the composed prompt.
    let resumed = compose_resume_prompt(&task());

    assert!(!resumed.contains("Read the store"), "the plan leaked");
    assert!(
        !resumed.contains("Skip the migration"),
        "extra instructions leaked"
    );
    assert!(
        !resumed.contains('\n'),
        "a continuation prompt is one line: {resumed}"
    );
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A task with every section populated, so each test above can remove exactly
/// the one thing it is about.
fn task() -> TaskDetail {
    TaskDetail {
        task: Task {
            id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            repository_id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
            title: "Wire the board to the store".to_string(),
            plan: Some("## Steps\n\n1. Read the store.\n2. Wire the board to it.".to_string()),
            extra_instructions: Some("Skip the migration, it already landed.".to_string()),
            column: BoardColumn::Ready,
            position: 0.0,
            run_state: RunState::Queued,
            branch: Some("rimaia/wire-the-board".to_string()),
            worktree_path: Some(
                "/Users/someone/Library/Application Support/com.rimaia.app/worktrees/3f2b1c00"
                    .to_string(),
            ),
            strategy_mode: StrategyMode::Default,
            model: None,
            effort: None,
            strategy_plan: None,
            strategy_source: None,
            strategy_updated_at: None,
            created_at: timestamp(),
            updated_at: timestamp(),
            source: MutationSource::Ui,
        },
        links: vec![
            link("Asana", "https://app.asana.com/0/1/2", 0.0),
            link("Design doc", "https://docs.example.com/board", 1.0),
        ],
        depends_on: vec![],
        last_run: None,
        // The prompt never reads these — what a run is spawned *with* is argv,
        // not prose, and ADR-0009's amendment is explicit that the strategy
        // section never restates the model or effort. Present because the
        // struct requires them, and `None` so a test that started asserting on
        // them would have to say so.
        effective_model: None,
        effective_effort: None,
        effective_origin: StrategyOrigin::ClaudeCode,
    }
}

fn repository() -> Repository {
    Repository {
        id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
        // A path with a space, like every other fixture in this crate: nothing
        // here shells out, but the composed prompt is read by something that
        // will.
        path: "/Users/someone/Code/My Projects/rimaia".to_string(),
        name: "rimaia".to_string(),
        default_branch: "main".to_string(),
        worktree_root: "/Users/someone/Library/Application Support/com.rimaia.app/worktrees"
            .to_string(),
        allow_unattended_runs: true,
        created_at: timestamp(),
    }
}

fn entry(id: &str, label: &str) -> CatalogueEntry {
    CatalogueEntry {
        id: id.to_string(),
        label: label.to_string(),
    }
}

/// A proposal that fans out, as seam-contract D17.3 spells the envelope —
/// including the four things the prompt must **not** render: the top-level model
/// and effort, the per-phase ones, the rationale the panel shows, and the
/// planner's own accounting.
fn multi_agent_proposal() -> String {
    r#"{
  "version": 1,
  "status": "proposed",
  "model": "sonnet",
  "effort": "high",
  "workflow": "multi_agent",
  "phases": [
    { "name": "Schema", "model": "sonnet", "effort": "medium", "agents": 1,
      "summary": "Add the columns and the migration." },
    { "name": "Wiring", "model": "haiku", "effort": "low", "agents": 3,
      "summary": "Thread the new field through every caller." }
  ],
  "rationale": "Two halves that do not touch, and the schema half is mechanical.",
  "run": { "session_id": "0f0f0f0f", "num_turns": 4, "cost_usd": 0.031, "error": null }
}"#
    .to_string()
}

/// The envelope a failed planner leaves behind. It is what suppresses a re-plan
/// (D17.8), so it is on the card — and it must still compose no section.
fn failed_proposal() -> String {
    r#"{ "version": 1, "status": "failed",
         "run": { "session_id": "0f0f0f0f", "num_turns": 6, "cost_usd": 0.004,
                  "error": "the planner stopped without calling the tool" } }"#
        .to_string()
}

fn single_agent_proposal() -> String {
    r#"{ "version": 1, "status": "proposed", "model": "haiku", "effort": "low",
         "workflow": "single_agent", "phases": [],
         "rationale": "One function and its test." }"#
        .to_string()
}

/// Every level-1 heading of a composed prompt, in order.
///
/// Level one is the section boundary and a plan brings its own `##` structure,
/// which is exactly why ADR-0009 chose it — so this is the section list and not
/// a guess at one. No fixture here has a plan with an `# ` heading of its own;
/// one that did would need a different assertion.
fn headings(composed: &str) -> Vec<&str> {
    composed
        .lines()
        .filter(|line| line.starts_with("# "))
        .collect()
}

/// The body of one section, for the tests that pin a section rather than a
/// whole prompt.
///
/// Whole strings are still the rule; this is the whole of a *section*, used
/// where the alternative is repeating five unrelated sections to assert one.
/// Same caveat as [`headings`] about a plan that carries its own `# ` heading.
fn section<'a>(composed: &'a str, heading: &str) -> &'a str {
    let body = composed
        .split_once(&format!("{heading}\n\n"))
        .unwrap_or_else(|| panic!("{heading} is not in the composed prompt:\n{composed}"))
        .1;

    match body.find("\n\n# ") {
        Some(end) => &body[..end],
        None => body,
    }
}

fn link(label: &str, url: &str, position: f64) -> TaskLink {
    TaskLink {
        id: format!("3f2b1c00-0000-4000-8000-00000000001{position}"),
        task_id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
        label: label.to_string(),
        url: url.to_string(),
        position,
    }
}

/// Numeric `+00:00`, never `Z` — the one spelling this codebase writes, so a
/// fixture copied out of here into SQL cannot invert a lexicographic sort.
fn timestamp() -> DateTime<Utc> {
    "2026-08-20T12:00:00+00:00"
        .parse()
        .expect("a literal timestamp must parse")
}
