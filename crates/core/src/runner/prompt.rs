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
//! # A third pair, for the run that decides how the work is done
//!
//! [`compose_strategy_prompt`] and [`compose_strategy_system_append`] are task
//! 020's planner (ADR-0016, seam-contract D17.9). The planner reads the same
//! plan an implementation run would and answers with a single MCP call; it
//! writes nothing. Its prompt **takes no base instructions at all** — see that
//! function's own note, and ADR-0009's 2026-08-28 amendment, for why that is a
//! missing parameter rather than an empty argument.
//!
//! Everything here is pure. Nothing reads the database: task 008 loads the base
//! instructions through [`crate::db::settings`] and passes them in, which is
//! what keeps composition unit-testable without a pool or a process.

use serde::Deserialize;

use crate::db::{Repository, TaskLink};
use crate::strategy::{Catalogue, CatalogueEntry};
use crate::tasks::TaskDetail;

/// Section headings, in ADR-0009's fixed order.
///
/// Level one rather than two: a plan routinely carries its own `##` structure,
/// and a section boundary has to sit above whatever the plan brought with it.
const BASE_INSTRUCTIONS_HEADING: &str = "# Base instructions";
const TASK_CONTEXT_HEADING: &str = "# Task context";
const PLAN_HEADING: &str = "# Plan";
/// The fifth section ADR-0009's 2026-08-28 amendment adds, fourth in order.
const EXECUTION_STRATEGY_HEADING: &str = "# Execution strategy";
const EXTRA_INSTRUCTIONS_HEADING: &str = "# Extra instructions";

/// The strategy prompt's own headings, in seam-contract D17.9's order. `#
/// Task context`, `# Plan` and `# Extra instructions` are shared with the
/// implementation prompt above, which is the point of reusing them: the planner
/// judges exactly the text the run it is planning would be given.
const YOUR_JOB_HEADING: &str = "# Your job";
const AVAILABLE_MODELS_HEADING: &str = "# Available models";
const AVAILABLE_EFFORTS_HEADING: &str = "# Available effort levels";
const HOW_TO_ANSWER_HEADING: &str = "# How to answer";

/// The planner's one way to answer, as Claude Code spells an MCP tool:
/// `mcp__<server>__<tool>`, and the server is `rimaia` because that is the key
/// the runner writes into `--mcp-config` (seam-contract D17.4).
///
/// Exported so that the caller of [`compose_strategy_system_append`] passes this
/// same name rather than retyping it — the two channels naming different tools
/// would be a defect no compiler catches.
pub const SET_TASK_STRATEGY_TOOL: &str = "mcp__rimaia__set_task_strategy";

/// A blank line between a heading and its body, and between sections.
const SECTION_SEPARATOR: &str = "\n\n";

/// The whole prompt for one run, in ADR-0009's order: base instructions, task
/// context, plan, execution strategy, extra instructions.
///
/// `base` is the raw `settings.base_instructions` text, template variables
/// unexpanded; this expands them. Empty sections are omitted entirely, heading
/// and all — a section whose body is only whitespace counts as empty, because a
/// plan of three newlines and no plan at all are the same thing to the agent.
///
/// `guidance` is what a planner proposed for this task, and it is a parameter
/// rather than something read off `task` because the caller is the one that
/// knows whether the proposal is the strategy this run is actually executing:
/// the strategy run hands over the guidance it has just recorded, and a run that
/// did not plan passes `None`. A proposal with nothing to say — single-agent,
/// no phases — composes exactly what `None` composes, which is what keeps task
/// 006's byte-for-byte preview criterion true for every task that has never been
/// planned.
///
/// There is no trailing newline. The string is stored verbatim in `runs.prompt`
/// and compared byte for byte against the Settings preview, so it ends exactly
/// where its last section does.
pub fn compose_prompt(
    base: &str,
    task: &TaskDetail,
    repo: &Repository,
    guidance: Option<&StrategyGuidance>,
) -> String {
    let variables = Variables::of(task, repo);

    let mut sections = Vec::with_capacity(5);
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
        EXECUTION_STRATEGY_HEADING,
        &guidance.map(execution_strategy).unwrap_or_default(),
    );
    push_section(
        &mut sections,
        EXTRA_INSTRUCTIONS_HEADING,
        task.task.extra_instructions.as_deref().unwrap_or_default(),
    );

    sections.join(SECTION_SEPARATOR)
}

/// The whole prompt for a strategy run (ADR-0016, seam-contract D17.9): what the
/// job is, the same task context and plan an implementation run would get, the
/// catalogue to choose from, and the one way to answer.
///
/// **There is no `base` parameter, and that is the design.** Base instructions
/// are implementation workflow — commit as you work, run the suite, open a pull
/// request — and a planner that opens a pull request is a defect. Passing an
/// empty string would leave the wiring one plausible edit away; having nowhere
/// to pass them makes it impossible, which is the cheapest place to enforce a
/// rule that would otherwise be a comment (ADR-0009's 2026-08-28 amendment).
///
/// Composed by exactly the rules [`compose_prompt`] follows: level-1 headings,
/// the same separator, empty sections omitted with their heading, no trailing
/// newline. An empty catalogue list is therefore an omitted section rather than
/// an empty one — an operator who turned a dropdown off has said something, and
/// what `# How to answer` then asks for is an id the planner has to name on its
/// own.
pub fn compose_strategy_prompt(
    task: &TaskDetail,
    repo: &Repository,
    catalogue: &Catalogue,
) -> String {
    let mut sections = Vec::with_capacity(7);
    push_section(&mut sections, YOUR_JOB_HEADING, YOUR_JOB);
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
    push_section(
        &mut sections,
        AVAILABLE_MODELS_HEADING,
        &choices(&catalogue.models),
    );
    push_section(
        &mut sections,
        AVAILABLE_EFFORTS_HEADING,
        &choices(&catalogue.efforts),
    );
    push_section(
        &mut sections,
        HOW_TO_ANSWER_HEADING,
        &how_to_answer(&task.task.id),
    );

    sections.join(SECTION_SEPARATOR)
}

/// The orchestrator constraints for a strategy run's `--append-system-prompt`
/// (ADR-0012's 2026-08-28 amendment).
///
/// The same channel and the same rules as [`compose_system_append`], for a run
/// whose facts are different ones: it is unattended and nobody can answer it, it
/// writes nothing at all, and its whole output is one call to `tool` for
/// `task_id`. The task id is here as well as in the prompt because it is the one
/// fact the planner must not reason its way out of — a proposal addressed to
/// someone else's card is refused by the scoped handle (seam-contract D17.4),
/// and a run that has been told which task it is cannot spend turns discovering
/// that.
///
/// `tool` is a parameter rather than [`SET_TASK_STRATEGY_TOOL`] read directly,
/// so the runner passes the name it actually wired; the constant is what it
/// passes.
pub fn compose_strategy_system_append(task_id: &str, tool: &str) -> String {
    format!(
        "You are running unattended, started by Rimaia to decide how one task should be executed. What follows is how this session works, not a preference you may weigh against the job.\n\
         \n\
         - Nobody is watching and nobody can answer a question. There is no interactive terminal, and anything you ask will go unread until a human reviews this transcript, which may be many hours from now.\n\
         - You are not implementing anything. Write no files, run no commands, commit nothing, and open no pull request. The tools that would let you are denied for this session.\n\
         - Your whole answer is one call to `{tool}`, made exactly once, for the task `{task_id}` and no other. Anything you print instead of that call is read by nobody."
    )
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

// ---------------------------------------------------------------------------
// The strategy prompt's own sections
// ---------------------------------------------------------------------------

/// `# Your job`, and the whole of it — a constant rather than a `format!`
/// because nothing about the job varies by task. What varies is the plan it is
/// judging, and that is the section below it.
///
/// It says "not implementing" three ways on purpose. The planner is a Claude
/// Code session sitting in a prepared worktree with a plan in front of it, which
/// is exactly the situation it has been trained to start working in; the denied
/// tools stop it, but a run that spends six turns trying to edit files and then
/// stops has still answered nothing.
const YOUR_JOB: &str = "You are choosing how another agent should execute the task below. You are not implementing it, and nothing you decide here is code.

Read the plan and answer three questions:

- **Which model should run it.** Pick from the models listed below.
- **How much reasoning effort it needs.** Pick from the effort levels listed below. Reach for the expensive end only where the plan genuinely earns it: every task in the queue is paid for out of one subscription, and effort spent on a mechanical task is effort a hard one later tonight will not have.
- **Whether the work fans out.** Most tasks do not. Propose a multi-agent workflow only when the plan holds parts that can genuinely be worked in parallel, and name those phases; the agent that implements this task will run them itself, with its own subagents.

The plan and any extra instructions below are what you are judging, not what you are carrying out.";

/// One catalogue list, as the planner has to read it: the id it must copy
/// verbatim, then the label a human would recognise.
///
/// The id is in backticks and first because it is the part that reaches
/// `--model` or `--effort`; the label is there so a planner choosing between
/// `xhigh` and `max` has the words the operator chose for them.
fn choices(entries: &[CatalogueEntry]) -> String {
    entries
        .iter()
        .map(|entry| format!("- `{}` — {}", entry.id, entry.label))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `# How to answer`: the tool, its arguments, and the instruction to print
/// nothing else.
///
/// **There is no printed-JSON fallback, and this section is where that shows.**
/// Seam-contract D17.9 gives the argument in full: a second way in would be a
/// second writer with its own parser duplicating every invariant
/// `set_task_strategy` enforces, extracting it would mean a heuristic over
/// free-form prose, and the scope check lives on the MCP path. A planner that
/// reasons well and then forgets the call is detected by the runner and recorded
/// as a failure — not rescued by parsing its prose.
fn how_to_answer(task_id: &str) -> String {
    format!(
        "Answer with one tool call and nothing else.\n\
         \n\
         Call `{SET_TASK_STRATEGY_TOOL}` exactly once, with:\n\
         \n\
         - `task_id`: `{task_id}` — this task, and no other. A call naming a different task is refused.\n\
         - `model`: one id from **Available models** above, copied verbatim.\n\
         - `effort`: one id from **Available effort levels** above, copied verbatim.\n\
         - `rationale`: one to three sentences on why that pairing suits this plan. A human reads it on the card.\n\
         - `workflow`: `multi_agent`, with a `phases` list, only if the work genuinely fans out into parts that can be worked in parallel. Otherwise `single_agent`, and no phases.\n\
         \n\
         Then stop. Print nothing else: the tool call is the only answer that reaches Rimaia, and prose beside it is read by nobody. Do not edit files, run commands, or open a pull request."
    )
}

// ---------------------------------------------------------------------------
// Execution strategy
// ---------------------------------------------------------------------------

/// What a recorded proposal contributes to the prompt of the run that executes
/// it (ADR-0009's 2026-08-28 amendment).
///
/// A **projection** of the `strategy_plan` envelope (seam-contract D17.3), not a
/// second model of it. Two fields, because two are all the section renders, and
/// keeping composition independent of the envelope's full shape is what lets
/// task 021 extend that document without silently changing what a run is asked
/// to do.
///
/// `multi_agent` is a `bool` rather than a copy of the envelope's `workflow`
/// enum for the same reason: the only thing this section renders differently is
/// whether the work fans out, and a second enum spelling the same two variants
/// would be a thing to keep in step for no gain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StrategyGuidance {
    pub multi_agent: bool,
    pub phases: Vec<GuidancePhase>,
}

/// One phase of a multi-agent proposal, as the prompt renders it.
///
/// **No model and no effort**, though the envelope carries them per phase.
/// ADR-0009's amendment is explicit that this section never restates either:
/// they are `--model` and `--effort`, already applied to this process by the
/// time it reads this, and Rimaia cannot apply a different pair to a subagent it
/// does not run. Putting them in prose would invite the run to argue with a
/// decision it cannot change. The panel renders them; the prompt does not.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GuidancePhase {
    pub name: String,
    #[serde(default)]
    pub agents: Option<u32>,
    #[serde(default)]
    pub summary: Option<String>,
}

impl StrategyGuidance {
    /// The guidance a task's recorded proposal carries, or `None` when it has
    /// none to give.
    ///
    /// One derivation, reached by both callers — the strategy run and the
    /// Settings preview — so that what the preview shows and what a run receives
    /// cannot differ (task 006's criterion, ADR-0006's rule about a rule
    /// enforced in one adapter).
    pub fn for_task(task: &TaskDetail) -> Option<Self> {
        Self::from_plan(task.task.strategy_plan.as_deref()?)
    }

    /// Reads the guidance fields out of a stored `strategy_plan` document.
    ///
    /// `None` for anything that is not a proposal with something to say: a
    /// `failed` envelope, a status this build does not recognise, a
    /// single-agent proposal with no phases, or JSON that will not parse at all.
    /// Tolerant rather than fallible, the rule
    /// [`crate::strategy::catalogue::catalogue`] and
    /// [`RunEnvironment::from_stored`](crate::db::RunEnvironment) already
    /// state — a mangled row costs a log line and a prompt without this
    /// section, never a queue.
    pub fn from_plan(plan: &str) -> Option<Self> {
        let proposal = match serde_json::from_str::<Proposal>(plan) {
            Ok(proposal) => proposal,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "unreadable strategy_plan; composing the prompt without an execution strategy"
                );
                return None;
            }
        };

        if proposal.status != ProposalStatus::Proposed {
            return None;
        }

        let guidance = Self {
            multi_agent: proposal.workflow == Workflow::MultiAgent,
            phases: proposal.phases,
        };

        // A proposal that neither fans out nor names a phase has nothing the
        // prompt could say, and saying it anyway would move the bytes of every
        // single-agent task's prompt.
        (guidance.multi_agent || !guidance.phases.is_empty()).then_some(guidance)
    }
}

/// The subset of seam-contract D17.3's envelope this module reads. Unknown
/// fields are ignored rather than denied: task 021 extends this document, and a
/// field it adds must not cost the run its phases.
#[derive(Debug, Deserialize)]
struct Proposal {
    #[serde(default)]
    status: ProposalStatus,
    #[serde(default)]
    workflow: Workflow,
    #[serde(default)]
    phases: Vec<GuidancePhase>,
}

/// `#[serde(other)]` on both of these is the tolerance rule as a type: a status
/// or workflow written by a newer Rimaia parses into a variant this build knows
/// it does not understand, and the guidance is dropped, rather than the whole
/// document failing and taking the rest of the prompt's fidelity with it.
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProposalStatus {
    Proposed,
    Failed,
    #[serde(other)]
    #[default]
    Unrecognised,
}

#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Workflow {
    #[default]
    SingleAgent,
    MultiAgent,
    #[serde(other)]
    Unrecognised,
}

/// The body of `# Execution strategy`, or an empty string when the guidance has
/// nothing to say — which [`push_section`] then omits, heading and all.
fn execution_strategy(guidance: &StrategyGuidance) -> String {
    if !guidance.multi_agent && guidance.phases.is_empty() {
        return String::new();
    }

    let mut parts = vec![
        "A planning run read the plan above and proposed how to execute it. Treat it as guidance from an agent that saw the whole task at once: follow it unless the code tells you otherwise once you are in it, and say so in your final message if it does.".to_string(),
    ];

    if guidance.multi_agent {
        parts.push(
            "This work fans out. Run it with subagents rather than as one linear pass, giving each only the part of the plan it needs."
                .to_string(),
        );
    }

    if !guidance.phases.is_empty() {
        parts.push("Phases, in order:".to_string());
        parts.push(
            guidance
                .phases
                .iter()
                .enumerate()
                .map(|(index, phase)| phase.render(index + 1))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    // ADR-0016's boundary, in the prompt rather than only in an ADR: Rimaia
    // injects a strategy, it never orchestrates one.
    parts.push(
        "Rimaia does not run these phases; you do, in this session, with your own subagents."
            .to_string(),
    );

    parts.join(SECTION_SEPARATOR)
}

impl GuidancePhase {
    /// `1. **Schema** (2 agents) — Add the columns.`
    ///
    /// The agent count and the summary are each omitted when the planner gave
    /// none, for [`task_context`]'s reason: a phase rendered `(0 agents)` or
    /// trailing an empty dash tells the run less than one that says nothing.
    fn render(&self, number: usize) -> String {
        let name = self.name.trim();
        let agents = match self.agents {
            Some(1) => " (1 agent)".to_string(),
            Some(agents) => format!(" ({agents} agents)"),
            None => String::new(),
        };
        let summary = match self.summary.as_deref().map(str::trim) {
            Some(summary) if !summary.is_empty() => format!(" — {summary}"),
            _ => String::new(),
        };

        format!("{number}. **{name}**{agents}{summary}")
    }
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
