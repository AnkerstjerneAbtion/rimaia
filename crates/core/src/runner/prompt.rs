//! Composing what a run is actually asked to do (ADR-0009, ADR-0012).
//!
//! **This module's output is the contract with the agent.** ADR-0009 persists
//! the exact composed string on the `runs` row because when a run goes wrong the
//! first question is always "what did it actually get", and reconstructing that
//! from three tables and a settings row that has since changed is not an answer.
//! So the tests here assert whole strings rather than substrings: a stray
//! heading change should fail a test, not surprise someone at 2am.
//!
//! # Two channels, and they are not interchangeable
//!
//! [`compose_prompt`] builds the **prompt**, delivered on stdin. It carries
//! workflow requirements the agent should treat as part of the job and may
//! reason about, and it is the thing a human reads in the transcript when asking
//! "why didn't it open a PR".
//!
//! [`compose_system_append`] builds `--append-system-prompt`, which ADR-0012
//! reserves for orchestrator facts the agent must *not* treat as negotiable: it
//! is unattended, nobody can answer a question, it must not push to the default
//! branch, and it must stop and report rather than guess. Both strings reach the
//! model; putting one channel's content in the other is a defect even so.
//!
//! [`compose_resume_prompt`] is neither. A retry is `--resume <session-id>`, and
//! the original prompt is already in the session — the spike confirmed a
//! one-line continuation picks up mid-plan rather than restarting, so re-sending
//! the composed prompt would only re-spend the tokens.
//!
//! Everything here is pure. Nothing reads the database: task 008 loads the base
//! instructions through [`crate::db::settings`] and passes them in, which is
//! what keeps composition unit-testable without a pool or a process.

use crate::db::{Repository, TaskLink};
use crate::tasks::TaskDetail;

/// Section headings, in ADR-0009's fixed order.
///
/// Level one rather than two: a plan routinely carries its own `##` structure,
/// and a section boundary has to sit above whatever the plan brought with it.
const BASE_INSTRUCTIONS_HEADING: &str = "# Base instructions";
const TASK_CONTEXT_HEADING: &str = "# Task context";
const PLAN_HEADING: &str = "# Plan";
const EXTRA_INSTRUCTIONS_HEADING: &str = "# Extra instructions";

/// A blank line between a heading and its body, and between sections.
const SECTION_SEPARATOR: &str = "\n\n";

/// The whole prompt for one run, in ADR-0009's order: base instructions, task
/// context, plan, extra instructions.
///
/// `base` is the raw `settings.base_instructions` text, template variables
/// unexpanded; this expands them. Empty sections are omitted entirely, heading
/// and all — a section whose body is only whitespace counts as empty, because a
/// plan of three newlines and no plan at all are the same thing to the agent.
///
/// There is no trailing newline. The string is stored verbatim in `runs.prompt`
/// and compared byte for byte against the Settings preview, so it ends exactly
/// where its last section does.
pub fn compose_prompt(base: &str, task: &TaskDetail, repo: &Repository) -> String {
    let variables = Variables::of(task, repo);

    let mut sections = Vec::with_capacity(4);
    push_section(
        &mut sections,
        BASE_INSTRUCTIONS_HEADING,
        &expand(base, &variables),
    );
    push_section(
        &mut sections,
        TASK_CONTEXT_HEADING,
        &task_context(task, repo),
    );
    push_section(
        &mut sections,
        PLAN_HEADING,
        task.task.plan.as_deref().unwrap_or_default(),
    );
    push_section(
        &mut sections,
        EXTRA_INSTRUCTIONS_HEADING,
        task.task.extra_instructions.as_deref().unwrap_or_default(),
    );

    sections.join(SECTION_SEPARATOR)
}

/// The orchestrator constraints for `--append-system-prompt` (ADR-0012).
///
/// Exactly the four facts that ADR names, phrased as statements about the
/// session rather than as requests: unattended, unanswerable, not the default
/// branch, stop rather than guess. Nothing task-specific beyond the branch —
/// what the task *is* belongs in the prompt, where the agent may reason about
/// it.
pub fn compose_system_append(task: &TaskDetail, repo: &Repository) -> String {
    // A run always has a branch by the time task 008 spawns it, because task
    // 007 prepared the worktree first. The other spelling exists for the
    // Settings preview, which composes against whatever task the user picked.
    let where_to_work = match task.task.branch.as_deref() {
        Some(branch) => format!("on the branch `{branch}`"),
        None => "on the branch Rimaia created for this task".to_string(),
    };
    let default_branch = &repo.default_branch;

    format!(
        "You are running unattended, started by Rimaia. What follows is how this session works, not a preference you may weigh against the task.\n\
         \n\
         - Nobody is watching and nobody can answer a question. There is no interactive terminal, and anything you ask will go unread until a human reviews this transcript, which may be many hours from now.\n\
         - You are working in a git worktree of your own, {where_to_work}. Commit and push there. Never push to `{default_branch}`, this repository's default branch, and never rewrite history that is already on the remote.\n\
         - If you cannot proceed, stop and report rather than guess. Commit what you have and state plainly what blocked you. A clear blocker is a useful outcome; a guess dressed as a decision is not."
    )
}

/// The continuation instruction a retry sends with `--resume` (ADR-0011).
///
/// Short on purpose, and it names the task only so a human reading the
/// transcript can tell which one resumed — the instructions and the plan are
/// already in the session, and re-sending them is what "resume rather than
/// restart" exists to avoid.
pub fn compose_resume_prompt(task: &TaskDetail) -> String {
    format!(
        "Continue the task \"{}\" from where you stopped. The instructions and the plan are earlier in this session — do not start over and do not ask for them again. If you cannot proceed, stop and report what blocked you.",
        task.task.title
    )
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn push_section(sections: &mut Vec<String>, heading: &str, body: &str) {
    let body = body.trim();
    if body.is_empty() {
        return;
    }
    sections.push(format!("{heading}{SECTION_SEPARATOR}{body}"));
}

/// Title, repository, branch, base ref and links, as a Markdown list.
///
/// A line whose value does not exist is omitted rather than rendered empty: an
/// agent told `- Branch:` with nothing after it learns less than one told
/// nothing.
///
/// **Base ref is the repository's default branch, which is true for the MVP and
/// only for the MVP.** Task 007 resolves the real base ref — "default branch, or
/// a dependency's branch once 011 lands" — so when ADR-0008's branch chaining
/// arrives, the resolved ref has to reach this function instead of being
/// inferred from `repo`, or the prompt will name a base the worktree was not cut
/// from.
fn task_context(task: &TaskDetail, repo: &Repository) -> String {
    let mut lines = vec![
        format!("- Title: {}", task.task.title),
        format!("- Repository: {}", repo.name),
    ];
    if let Some(branch) = task.task.branch.as_deref() {
        lines.push(format!("- Branch: {branch}"));
    }
    lines.push(format!("- Base ref: {}", repo.default_branch));
    if !task.links.is_empty() {
        lines.push("- Links:".to_string());
        for link in &task.links {
            lines.push(format!("  - [{}]({})", link.label, link.url));
        }
    }
    lines.join("\n")
}

/// The links as top-level bullets, in the order [`crate::tasks::get_task`]
/// returns them, which is their board order.
///
/// Top-level because this is also what `{{task.links}}` expands to, and that
/// lands wherever the user put it in their own instructions; the nested copy in
/// [`task_context`] indents it there instead.
fn link_list(links: &[TaskLink]) -> String {
    links
        .iter()
        .map(|link| format!("- [{}]({})", link.label, link.url))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Template variables
// ---------------------------------------------------------------------------

const OPEN: &str = "{{";
const CLOSE: &str = "}}";

/// ADR-0009's five variable names, as a list something outside this module can
/// read.
///
/// Task 010's `get_base_instructions` returns the stored template unexpanded
/// and hands this alongside it, so a planning agent writing a plan can use a
/// placeholder that exists rather than invent one. The colocated test asserts
/// every name here resolves through [`Variables::lookup`] *and* that `lookup`
/// recognises nothing else — which is what stops this constant and the
/// expander drifting apart.
pub const TEMPLATE_VARIABLES: [&str; 5] = [
    "task.title",
    "task.branch",
    "task.links",
    "repo.name",
    "repo.default_branch",
];

/// The five variables ADR-0009 fixes, resolved once before any expansion.
///
/// Resolved up front rather than looked up lazily so that [`expand`] is a pure
/// string operation with an obvious test: its hard cases — the same variable
/// twice, a value that itself looks like a variable, an unclosed `{{` — are
/// about the scanner and have nothing to do with a task.
struct Variables {
    task_title: String,
    task_branch: String,
    task_links: String,
    repo_name: String,
    repo_default_branch: String,
}

impl Variables {
    fn of(task: &TaskDetail, repo: &Repository) -> Self {
        Self {
            task_title: task.task.title.clone(),
            // A known variable with no value expands to nothing rather than
            // staying verbatim. Leaving `{{task.branch}}` in the prompt would
            // show the agent Rimaia's own template syntax, which is worse than
            // a short sentence.
            task_branch: task.task.branch.clone().unwrap_or_default(),
            task_links: link_list(&task.links),
            repo_name: repo.name.clone(),
            repo_default_branch: repo.default_branch.clone(),
        }
    }

    /// The value for a variable name, or `None` when the name is not one of
    /// ADR-0009's five.
    fn lookup(&self, name: &str) -> Option<&str> {
        match name {
            "task.title" => Some(&self.task_title),
            "task.branch" => Some(&self.task_branch),
            "task.links" => Some(&self.task_links),
            "repo.name" => Some(&self.repo_name),
            "repo.default_branch" => Some(&self.repo_default_branch),
            _ => None,
        }
    }
}

/// Substitutes `{{...}}` in the user's base instructions.
///
/// One left-to-right pass, and **expanded values are never rescanned**: a task
/// titled `Support {{repo.name}} in the parser` is a title, not a template, and
/// a second pass would rewrite it.
///
/// Everything it does not recognise survives untouched, because ADR-0009 is
/// explicit that a typo in a settings field must not kill an overnight queue:
///
/// - an unknown name keeps its whole `{{...}}` construct, braces included;
/// - an unclosed `{{` and everything after it is text;
/// - `{{ task.title }}` expands — the padding is a person writing, not a
///   different variable. Only the padding is forgiven; the name itself is
///   matched exactly, so `{{Task.Title}}` is unknown.
fn expand(template: &str, variables: &Variables) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find(OPEN) {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + OPEN.len()..];

        let Some(close) = after_open.find(CLOSE) else {
            out.push_str(&rest[open..]);
            return out;
        };

        match variables.lookup(after_open[..close].trim()) {
            Some(value) => out.push_str(value),
            None => out.push_str(&rest[open..open + OPEN.len() + close + CLOSE.len()]),
        }
        rest = &after_open[close + CLOSE.len()..];
    }

    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Values chosen to be visibly distinct from one another, so a test that
    /// fails says which variable went where.
    fn variables() -> Variables {
        Variables {
            task_title: "Wire the board to the store".to_string(),
            task_branch: "rimaia/wire-the-board".to_string(),
            task_links: "- [Asana](https://app.asana.com/0/1/2)".to_string(),
            repo_name: "rimaia".to_string(),
            repo_default_branch: "main".to_string(),
        }
    }

    #[test]
    fn the_published_variable_list_is_exactly_what_lookup_recognises() {
        // Both directions, because either half alone lets the two drift: a
        // name published but unresolvable expands to nothing, and a name
        // resolvable but unpublished is one no agent will ever use.
        let variables = variables();

        for name in TEMPLATE_VARIABLES {
            assert!(
                variables.lookup(name).is_some(),
                "{name} is published but does not resolve"
            );
        }

        for unknown in [
            "task.plan",
            "task.id",
            "repo.path",
            "Task.Title",
            "task.title ",
        ] {
            assert!(
                variables.lookup(unknown).is_none(),
                "{unknown} resolves but is not published"
            );
        }
    }

    #[test]
    fn every_known_variable_resolves_to_its_own_value() {
        let variables = variables();

        assert_eq!(
            expand(
                "{{task.title}}|{{task.branch}}|{{repo.name}}|{{repo.default_branch}}|{{task.links}}",
                &variables
            ),
            "Wire the board to the store|rimaia/wire-the-board|rimaia|main|- [Asana](https://app.asana.com/0/1/2)"
        );
    }

    #[test]
    fn a_variable_used_twice_expands_both_times() {
        assert_eq!(
            expand("{{repo.name}} and again {{repo.name}}", &variables()),
            "rimaia and again rimaia"
        );
    }

    #[test]
    fn a_value_that_looks_like_a_variable_is_not_expanded_again() {
        // The reason expansion is one pass over the input and never over the
        // output: a title is data, and re-scanning it would let a task rename
        // its own repository in the prompt.
        let variables = Variables {
            task_title: "Support {{repo.name}} in the parser".to_string(),
            ..variables()
        };

        assert_eq!(
            expand("Task: {{task.title}}", &variables),
            "Task: Support {{repo.name}} in the parser"
        );
    }

    #[test]
    fn an_unknown_variable_keeps_its_braces() {
        assert_eq!(
            expand(
                "Owner is {{task.owner}}, repo is {{repo.name}}.",
                &variables()
            ),
            "Owner is {{task.owner}}, repo is rimaia."
        );
    }

    #[test]
    fn a_misspelled_variable_does_not_stop_the_ones_after_it() {
        // The whole point of passing unknowns through: one typo in Settings must
        // still leave a usable prompt for an overnight queue.
        assert_eq!(
            expand("{{repo.naem}} {{repo.name}} {{Repo.Name}}", &variables()),
            "{{repo.naem}} rimaia {{Repo.Name}}"
        );
    }

    #[test]
    fn an_unclosed_variable_is_left_verbatim() {
        assert_eq!(
            expand("Branch: {{task.branch and then nothing", &variables()),
            "Branch: {{task.branch and then nothing"
        );
    }

    #[test]
    fn a_variable_padded_with_spaces_still_expands() {
        assert_eq!(
            expand("{{ task.title }} on {{  repo.name  }}", &variables()),
            "Wire the board to the store on rimaia"
        );
    }

    #[test]
    fn empty_braces_are_text_like_any_other_unknown_name() {
        assert_eq!(expand("a {{}} b", &variables()), "a {{}} b");
    }

    #[test]
    fn text_with_no_variables_survives_unchanged() {
        assert_eq!(
            expand("Commit as you work.\n\nRun the tests.", &variables()),
            "Commit as you work.\n\nRun the tests."
        );
    }

    #[test]
    fn a_known_variable_with_no_value_expands_to_nothing() {
        let variables = Variables {
            task_branch: String::new(),
            task_links: String::new(),
            ..variables()
        };

        assert_eq!(
            expand("[{{task.branch}}][{{task.links}}]", &variables),
            "[][]"
        );
    }

    #[test]
    fn a_section_whose_body_is_only_whitespace_is_omitted_with_its_heading() {
        let mut sections = Vec::new();
        push_section(&mut sections, PLAN_HEADING, "  \n\n\t\n");

        assert_eq!(sections, Vec::<String>::new());
    }

    #[test]
    fn a_section_body_is_trimmed_but_its_internal_blank_lines_are_kept() {
        let mut sections = Vec::new();
        push_section(&mut sections, PLAN_HEADING, "\n\nfirst\n\nsecond\n\n");

        assert_eq!(sections, vec!["# Plan\n\nfirst\n\nsecond".to_string()]);
    }
}
