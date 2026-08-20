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
//! `crates/core/src/runner/prompt.rs`. What is here is composition: which
//! sections appear, in what order, and what they look like.

use chrono::{DateTime, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, Repository, RunState, StrategyMode, Task, TaskLink};
use rimaia_core::runner::prompt::{compose_prompt, compose_resume_prompt, compose_system_append};
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
        compose_prompt("Commit as you work.", &task, &repository()),
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
        compose_prompt("Commit as you work.", &blank, &repository()),
        compose_prompt("Commit as you work.", &absent, &repository())
    );
}

#[test]
fn a_task_with_no_extra_instructions_omits_that_section_and_its_heading() {
    let mut task = task();
    task.task.extra_instructions = None;

    assert_eq!(
        compose_prompt("Commit as you work.", &task, &repository()),
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
        compose_prompt("Commit as you work.", &task, &repository()),
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
        compose_prompt("", &task, &repository()),
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
        compose_prompt("Commit as you work.", &task, &repository()),
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
            &repository()
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
            &repository()
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

    let stored = compose_prompt("Open a draft PR.", &task, &repository);
    let recomposed = compose_prompt("Never open a PR.", &task, &repository);

    assert!(stored.contains("Open a draft PR."));
    assert!(!stored.contains("Never open a PR."));
    assert_ne!(stored, recomposed);
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
        },
        links: vec![
            link("Asana", "https://app.asana.com/0/1/2", 0.0),
            link("Design doc", "https://docs.example.com/board", 1.0),
        ],
        depends_on: vec![],
        last_run: None,
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
