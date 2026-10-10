# 32. Assignment, and consent to run someone's plan on your machine

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

ADR-0012 runs unattended work with `--permission-mode bypassPermissions` and argues it is
acceptable because of a set of mitigations. The first and most important one is consent:

> Registering a repository does not make it runnable. The user must tick "allow unattended
> agent runs" for that repository, behind a dialog that states in plain language what it
> permits.

That argument assumed the person who ticks the box, the person who writes the plans and the
person whose machine runs them are the same person. On a shared board (ADR-0027) they are
not.

**A plan is code by proxy.** Anyone who can put text in front of a runner's agent can make
that runner's machine do anything the agent can:

- read the environment and the owner's files;
- install packages;
- reach the network;
- push with the owner's forge credentials (ADR-0020).

That text is not only the plan. It is everything the prompt is composed from (ADR-0009):

- the plan and extra instructions;
- the team's base instructions;
- the code the worktree starts from. A dependent task branches from its dependency's commit
  (ADR-0033), and the agent runs its tests.

The per-repository opt-in never had to ask *whose* text it covered, because it was all the
owner's. Now it has to.

The product also needs work to flow without ceremony. A team that plans during the day and
wants its runners busy at night cannot require each member to approve each card by hand.

## Decision

### 1. A task can be assigned to one person

`tasks.assignee_id` is nullable and references a team member. Assignment is a board action
available to every member, recorded with `assigned_by` (ADR-0030). An unassigned task belongs
to the team's pool.

A task is assigned to a **person**, not a runner. A person's runners are interchangeable from
the board's point of view, except where ADR-0031's pinning makes one of them specific.

### 2. By default a runner runs only its owner's assigned tasks

Each runner has an eligibility policy, stored on the server so the claim can apply it:

| Policy | Runs |
| --- | --- |
| **Assigned to me** (default) | Tasks assigned to the runner's owner |
| **Assigned to me, then the pool** | The above first, then unassigned tasks in teams the owner picks |

A runner **never** runs a task assigned to someone else. That is not a policy option. Moving
work to another person means reassigning the card, which is visible to everyone.

In a personal team every task is the owner's own, so the default covers everything with no
configuration. That is the solo experience.

### 3. Consent is given to content revisions, not to tasks

A task is **runnable on a runner** only when the runner's owner has consented to every piece
of content the run would execute. Each piece has an author and a revision:

| Content | Revision | Author recorded as |
| --- | --- | --- |
| The task's `plan` and `extra_instructions` | `tasks.plan_revision`, incremented by every edit to either | `plan_updated_by` |
| The team's base instructions | a revision per team, incremented by every edit | the owner who edited it |
| The team's review instructions, and a task's override of them (ADR-0017) | a revision per team and per task, incremented by every edit | the member who edited it |
| Review findings a fix phase will act on, when another runner wrote them | the review run that produced them | the owner of the runner that ran the review |
| The starting commit, when it is a dependency's | the dependency's `head_sha` (ADR-0033) | the owner of the runner that produced it |

The owner consents to a piece of content in one of three ways:

- **by being its author;**
- **by accepting that revision.** This records `(user, content, revision)`. An edit by someone
  else makes the content not runnable for that user again until they accept the new revision.
  The card says why and names who changed it.
- **by trusting its author.** A personal, per-team list of teammates whose revisions count as
  accepted automatically. It is off by default and revocable, and the UI lists it next to the
  runner policy.

The trust list is what keeps the flow the team asked for (plan in the day, run at night) to
one decision per teammate rather than one per card. Revisions are what stop a plan, the base
instructions or a dependency's code from being quietly changed between the evening review and
2am.

Tasks created over MCP count as written by the token's user (ADR-0030). A plan a person's own
Claude Code session wrote is that person's plan, subject to point 6.

Execution strategy (model, effort, workflow; ADR-0016) is not consent-gated. It changes what a
run costs the owner's subscription, not what code it runs. A runner may set a ceiling on
model and effort as a runner setting. That is a cost control, not consent.

### 4. Unattended consent is per runner and per repository, under a team ceiling

ADR-0012's opt-in splits into two levels, and a run needs both:

- **Team ceiling (owners).** "Unattended runs are allowed in this repository." It is a
  statement about the repository: an owner can forbid it for a repository that must never be
  touched unattended. It cannot make anyone's machine run anything.
- **Runner consent (the runner's owner, on that runner).** "My machine may run this
  repository unattended." Given locally, behind ADR-0012's dialog, whose wording does not
  soften. It is stored in the runner's store (ADR-0028), not on the server, so no remote change
  can grant it.

The claim (ADR-0031) checks the team ceiling and point 3's consent. Before spawning, the runner
checks its own repository consent again, and refuses a task that fails it.

**What that protects against, stated exactly.** A compromised or faulty server cannot make a
runner execute in a repository its owner never opted into on that machine. It *can* serve
plan text the owner never accepted, into a repository they did opt into, because only the
server records acceptances. Point 3 is enforced by the server. The repository opt-in is
enforced by both. Making acceptance verifiable on the runner (for example with signatures by
the owner's desktop) is possible later, and not decided here.

### 5. The same rules cover every process a runner spawns

The strategy planner (ADR-0016) and the review-and-fix loop (ADR-0017, task 021) run under the
task's eligibility and consent, and are claimed like any run (ADR-0031 point 1). The
on-archive cleanup script (ADR-0025) is runner-local configuration (ADR-0033), so a team member
cannot author a command for someone else's machine at all.

### 6. A run cannot launder consent

A run executes with its owner's local authority (ADR-0012), and on a connected machine that
includes the owner's Rimaia credentials. A run executing Alice's trusted plan on Bob's machine
could then write a new plan *as Bob*. That plan would pass Bob's own runners, and the runners
of everyone who trusts Bob. Three rules close the path as far as a local process can be
constrained, and state plainly what remains:

- **Rimaia's own surfaces are removed from the run.** The run's environment is stripped of
  Rimaia token variables. Every MCP server whose URL points at the Rimaia server or the
  runner's loopback operator port is denied by name, extending
  `runner::process::rimaia_tools_denied_to_a_run` beyond the literal name `rimaia`. The
  loopback operator endpoint requires a token once connected (ADR-0030 point 6).
- **Trust does not pass through a run.** A revision written with a user's credentials, while
  one of that user's runners holds a lease on content authored by *someone else*, is marked
  `written during a run`. It never counts as trusted for anyone. Only explicit acceptance
  lets it run. Ordinary planning is unaffected, including while the user's runner works on
  their own tasks.
- **What remains is ADR-0012's residual, and is stated.** A `bypassPermissions` run can read
  any file its owner can, including a token stored in a Claude Code configuration file, and
  can reach the network. The rules above make laundered content visible and stop it spreading
  through trust. They do not make the run unable to act. `run_environment = strict_local`
  removes the inherited MCP registrations entirely, and pairing a runner recommends it for team
  use.

### 7. What the agent is told

ADR-0009's composition gains two facts, stated plainly: who wrote the plan revision being
executed, and whose machine and credentials it is running on. They are not safety mechanisms,
since the agent cannot verify them. They make the transcript honest about the situation, and
they give a prompt-injected plan one fewer ambiguity to exploit.

## Consequences

- **Nobody's machine runs content they have not written, accepted or chosen to trust.** This
  is the property that makes a shared board with `bypassPermissions` runners defensible, and
  the reason it is recorded as a decision, not left to UI copy.
- **Assignment is the routing mechanism**, and the pool is opt-in. A team that wants
  first-come-first-served turns on "then the pool" on every runner. A team that wants explicit
  ownership leaves the default.
- **Edits have a visible cost.** Editing a teammate's plan, or the team's base instructions,
  makes affected tasks not runnable for members who have not accepted it or who do not trust
  the editor. That is the consent model working. The card, and a banner for base
  instructions, must make the reason obvious instead of leaving tasks silently skipped.
- **Trust is personal, not a team setting.** A team cannot declare that its members trust each
  other. Each member decides for their own machine, and owners cannot override it.
- **The team ceiling and the runner consent can disagree, and the stricter wins.** A
  repository may be allowed by the team and not consented to on a runner, which is common, or
  consented to on a runner and later forbidden by the team, which blocks it immediately.
- **Chains across people ask for consent to each other's code.** Bob's task built on Alice's
  commit needs Bob to trust Alice or accept that commit. In a team where members trust each
  other this is invisible. Where they do not, the card explains exactly which commit is
  waiting for acceptance.

## Alternatives considered

- **Team membership implies trust.** Least friction, and plausible for a small company team.
  Rejected as a default because it makes every member's account a way to run code on every
  other member's machine. One phished GitHub account becomes a team-wide incident. Available as
  an explicit choice through point 3's trust list.
- **Approve every task by hand.** Maximum control. Rejected as the only mechanism because it
  defeats "plan in the day, run at night" for any task someone else touched. Kept as the
  fallback when a revision is not trusted.
- **Consent per task instead of per revision.** Simpler to store. It lets the content change
  after consent, which is the gap the whole record exists to close.
- **Consent to the plan only.** Simpler, and leaves base instructions and dependency code as
  ways to put unaccepted content in front of an agent. Base instructions reach every task in
  the team at once, which makes them the more valuable target, not the less.
- **Assign to runners instead of people.** Precise, and it leaks machine topology into
  planning: the person planning should not need to know that Bob has a laptop and a Mac mini.
  ADR-0031's pinning already handles the one case where the machine matters.
- **Unattended consent held on the server.** Easier to show on the board. Rejected because it
  lets a remote write grant execution on a local machine. The board shows whether each runner
  has consented. The runner decides.

## Amendment, 2026-10-10 — a personal team does not consult the team ceiling (task 045)

Point 4's team ceiling is not consulted for a task in a personal team. There, the team's owner
and the machine's owner are one person, so the runner's consent is the whole decision. Asking
the same person a second time, in a second place, would only make solo's single toggle into
two, and every repository registered after task 066 starts with the column at `0`, so solo
would stop running anything until a setting nobody knows about was found. A repository never
leaves its team (task 039 refuses the move, and task 051 copies tasks, not repositories), so a
personal team's column never becomes a shared team's ceiling. The board command that sets the
ceiling refuses on a personal team.

In a shared team point 4 reads as written: a run needs both, and the Consequences' "the
stricter wins" holds. Seam-contract D36 point 1 records this beside the other personal-team
rules, which follow from who the people are rather than from a team-kind switch.

## Amendment, 2026-10-10 — the title, links and strategy prose are plan content (task 045)

Task 045's security review found three channels into a run's prompt that point 3's table did
not name, each written by any member and none consent-gated: the task's **title**, its
**links** (label and URL), and the strategy plan's **phase names and summaries**. All three
reach the composed prompt (`# Task context`, the `{{task.title}}` and `{{task.links}}`
template variables, and the strategy guidance section). So a teammate could put instructions
into another member's unattended run without an acceptance or a trust, which is exactly what
the Consequences rule out ("nobody's machine runs content they have not written, accepted or
chosen to trust").

**They join the plan revision.** Point 3's first row reads as covering the task's `title`,
`plan`, `extra_instructions` and links, and the free-text `name` and `summary` of each phase
of its strategy plan. Every edit that changes any of them increments `tasks.plan_revision`
and records `plan_updated_by` and the written-during-run mark, exactly as a plan edit does:
retitling, adding, editing, removing or reordering a link, and a strategy write (by
`set_task_strategy` or by the planner) whose phase prose differs from what was stored. One
revision rather than a piece per field, because each of these describes what the task is,
and a separate acceptance per field would multiply the decisions the trust list exists to
keep to one per teammate.

**Execution strategy's structured fields stay exempt**, as point 3 says: model, effort,
workflow and agent count change what a run costs, not what it runs, and the runner's strategy
ceiling governs them. A strategy write that changes only those fields bumps nothing. A planner
run on someone else's runner that writes phase prose sets the written-during-run mark, so
point 6 applies to it unchanged.

Rejected: a separate piece per field (more acceptances for the same decision, and more UI in
task 061), and recording the channels as an accepted residual (it would void the property
this ADR exists for).

## Amendment, 2026-10-10 — every prompt input has a class (tasks 045 and 073)

Task 045's second security review found two more channels of the same kind as the title: the
strategy catalogue's labels, which the planner prompt renders, and the repository's name and
default branch, which every prompt renders. Closing channels as reviews find them does not
converge, so the promise in the Consequences is restated as a rule over every input.

In a shared team, every value a composer renders into a prompt, including through a template
variable, is one of:

1. **consent-gated content**, a piece point 3 lists, at a revision the runner's owner wrote,
   accepted or trusts;
2. **generated by Rimaia**: ids, commit shas, counts, timestamps and logins;
3. **constrained on write** to a charset and length that cannot carry a sentence;
4. **none of these**, and then it is **not rendered** in a shared team. A line or variable that
   needs a value gets a class 2 stand-in.

A personal team's every author is its owner, so the rule changes nothing there, and solo
prompts stay as they are. The catalogue's model and effort ids and the default branch become
class 3. The catalogue's labels and the repository's name are class 4. Task 073 applies the
rule, records the full inventory in the seam contract, and adds a canary test that fails on
any team-writable text reaching a shared team's prompt without a class.

Until 073 lands these two channels are open. That is acceptable only because no shared team
can have a second member before task 051, and 073 is ordered before 051. Rejected: making
each channel a new consent piece (a new content kind and migration for text nobody needs to
send to an agent), and leaving them as a residual (it would void the property this ADR exists
for).
