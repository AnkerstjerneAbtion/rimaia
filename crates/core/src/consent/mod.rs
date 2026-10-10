//! Assignment, eligibility and consent to run someone's content on your
//! machine (ADR-0032, task 045).
//!
//! Three pure files decide: [`pieces`] lists what a run would execute and
//! whether an owner consents to each piece, [`eligibility`] whether a runner
//! may take a task at all, and [`ceiling`] whether its strategy is within
//! what the runner allows. This file reads what they decide over.
//!
//! # Two reads, on either side of the transaction
//!
//! The claim decides inside one `BEGIN IMMEDIATE` transaction
//! (`board::lease::claim`), and the test pool has one connection, so what the
//! transaction reads goes over the connection it holds. Every *revision*
//! (the plan, the instructions), every author, every acceptance and every
//! trust row is read there, by [`inputs`] and [`missing`]: an edit that lands
//! between a read and the claim is either seen or loses the write lock. A
//! context read before the transaction is judged only once
//! [`context_is_current`] says it holds the text whose revisions were read.
//!
//! What a composer includes besides those, which findings and which base
//! commit, is a [`Composition`]: read with the claim's context before the
//! transaction opens, as everything else a claim returns is. Its revisions
//! are run ids and commits, which never change, so the only thing a stale
//! composition can do is name the pieces of the context the claim returns.
//!
//! # Sentences
//!
//! Every refusal a person reads is written here, once: [`Ineligible`]'s for
//! the claim, and the services' below. Tasks 057 and 061 quote them.

pub mod ceiling;
pub mod eligibility;
pub mod pieces;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use crate::board::{BaseDependency, LeasePurpose, RunContext};
use crate::context::ServiceContext;
use crate::db::new_id;
use crate::db::settings::BASE_INSTRUCTIONS;
use crate::error::{Error, Result};
use crate::events::{ChangeEvent, RunId, RunnerId, TaskId, TeamId, UserId};
use crate::review::FindingStatus;
use crate::review_loop::config::REVIEW_INSTRUCTIONS;
use crate::runner::provider::ProviderId;
use crate::scheduler::SkipReason;

use ceiling::CeilingExceeded;
use eligibility::{Eligibility, Reason, RunnerEligibility, RunnerFacts, TaskFacts};
use pieces::{
    consents, pieces_for, Accepted, BaseCommitInput, Consent, ContentKind, MissingReason, Piece,
    PieceInputs, Revision, RunOnRunner,
};

// ---------------------------------------------------------------------------
// The composition, read before a transaction
// ---------------------------------------------------------------------------

/// Which recorded runs and which base commit a composer includes for a task
/// (see the module doc for why this half is read over the pool).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composition {
    /// The review runs whose open blocking findings a fix acts on.
    pub findings_to_fix: Vec<RunId>,
    /// The fix runs whose rejection reasons a review is told.
    pub rejections: Vec<RunId>,
    /// 044's dependency base, when the base is a dependency's commit.
    pub base: Option<BaseDependency>,
}

impl Composition {
    /// What `context`'s composers include: the runs behind its findings, and
    /// its base.
    pub fn of(context: &RunContext) -> Self {
        let (findings_to_fix, rejections) = match &context.review {
            Some(review) => (
                distinct(review.open_blocking.iter().map(|f| Some(&f.review_run_id))),
                distinct(
                    review
                        .rejected
                        .iter()
                        .map(|f| f.resolved_by_run_id.as_ref()),
                ),
            ),
            None => (Vec::new(), Vec::new()),
        };
        Self {
            findings_to_fix,
            rejections,
            base: context.base.dependency.clone(),
        }
    }
}

fn distinct<'a>(ids: impl Iterator<Item = Option<&'a String>>) -> Vec<RunId> {
    let mut seen = Vec::new();
    for id in ids.flatten() {
        if !seen.contains(id) {
            seen.push(id.clone());
        }
    }
    seen
}

/// A task's [`Composition`], read over the pool under `ctx`'s scope: the
/// base `read_context` would resolve, and, only when `findings` is set, the
/// runs behind the findings a review or fix would be told.
pub async fn composition(
    ctx: &ServiceContext,
    task_id: &str,
    findings: bool,
) -> Result<Composition> {
    let task = crate::tasks::service::task_row(ctx, task_id).await?;
    let repository = crate::repo::get(ctx, &task.repository_id).await?;
    let base = crate::worktree::base_ref::resolve(ctx, &task, &repository)
        .await?
        .dependency;
    if !findings {
        return Ok(Composition {
            base,
            ..Composition::default()
        });
    }

    let resolved = crate::review_loop::config::resolve(ctx, task_id, &repository.id).await?;
    let review = crate::review_loop::context(ctx, task_id, resolved).await?;
    let open = review
        .open_blocking
        .iter()
        .filter(|finding| finding.status == FindingStatus::Open)
        .map(|finding| Some(&finding.review_run_id));
    Ok(Composition {
        findings_to_fix: distinct(open),
        rejections: distinct(
            review
                .rejected
                .iter()
                .map(|finding| finding.resolved_by_run_id.as_ref()),
        ),
        base,
    })
}

// ---------------------------------------------------------------------------
// What the transaction reads
// ---------------------------------------------------------------------------

/// The pieces' inputs for `task_id` as `claiming_runner` would run them, on
/// the caller's connection, or `None` for a task that does not exist.
///
/// **Unscoped**: the caller has already answered whether its context reaches
/// the task, and the mark reads across every team on purpose (see
/// [`written_during_run`]).
pub(crate) async fn inputs(
    conn: &mut SqliteConnection,
    task_id: &str,
    claiming_runner: &str,
    composition: &Composition,
) -> Result<Option<PieceInputs>> {
    let Some(task) = sqlx::query!(
        r#"SELECT team_id, plan_revision, plan_updated_by,
                  plan_written_during_run AS "plan_written_during_run: bool",
                  review_instructions, review_instructions_revision,
                  review_instructions_updated_by,
                  review_instructions_written_during_run
                      AS "review_instructions_written_during_run: bool"
             FROM tasks WHERE id = ?1"#,
        task_id,
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };

    let says_something =
        |text: &Option<String>| text.as_deref().is_some_and(|t| !t.trim().is_empty());
    // Listed even when the plan and the extra instructions are blank: the
    // title and the links are plan content too (ADR-0032's 2026-10-10
    // amendment), and a title is never empty, so a blank-plan task still
    // composes text its revision's author wrote.
    let plan = Some(Revision {
        revision: task.plan_revision,
        author: task.plan_updated_by.clone(),
        written_during_run: task.plan_written_during_run,
    });
    let task_review_instructions = says_something(&task.review_instructions).then(|| Revision {
        revision: task.review_instructions_revision,
        author: task.review_instructions_updated_by.clone(),
        written_during_run: task.review_instructions_written_during_run,
    });

    let base_instructions = team_text(conn, &task.team_id, BASE_INSTRUCTIONS).await?;
    let team_review_instructions = team_text(conn, &task.team_id, REVIEW_INSTRUCTIONS).await?;

    let mut findings_to_fix = Vec::new();
    for run_id in &composition.findings_to_fix {
        findings_to_fix.push(run_on_runner(conn, run_id).await?);
    }
    let mut rejections = Vec::new();
    for run_id in &composition.rejections {
        rejections.push(run_on_runner(conn, run_id).await?);
    }

    let base_commit = match &composition.base {
        Some(base) => {
            let attempt =
                sqlx::query_scalar!("SELECT attempt FROM runs WHERE id = ?1", base.run_id)
                    .fetch_optional(&mut *conn)
                    .await?;
            let owners = match attempt {
                Some(attempt) => crate::runs::commit_authors(conn, &base.task_id, attempt).await?,
                // The run the base names is gone: nobody can be credited
                // with the commit, which reads as a former member's.
                None => vec![None],
            };
            Some(BaseCommitInput {
                task_id: base.task_id.clone(),
                commit: base.commit.clone(),
                owners,
            })
        }
        None => None,
    };

    Ok(Some(PieceInputs {
        task_id: task_id.to_string(),
        claiming_runner: claiming_runner.to_string(),
        plan,
        base_instructions,
        task_review_instructions,
        team_review_instructions,
        findings_to_fix,
        rejections,
        base_commit,
    }))
}

/// Whether `context`, read over the pool, still holds the consent-gated text
/// the database holds now, read on the caller's connection.
///
/// [`inputs`] reads revisions inside the transaction, and `run_context`
/// returns a context read before it opened, so consent judges a revision
/// only when this says the two agree: the plan by its revision, read in the
/// same row as its text, and the team's instructions and a task's override
/// by their text, which is what the composers read. A task that is gone is
/// current: whether it exists is the caller's question.
pub(crate) async fn context_is_current(
    conn: &mut SqliteConnection,
    task_id: &str,
    context: &RunContext,
) -> Result<bool> {
    let Some(task) = sqlx::query!(
        "SELECT team_id, plan_revision, review_instructions FROM tasks WHERE id = ?1",
        task_id,
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(true);
    };
    if task.plan_revision != context.task.task.plan_revision {
        return Ok(false);
    }
    let base = team_value(conn, &task.team_id, BASE_INSTRUCTIONS).await?;
    if base != context.base_instructions {
        return Ok(false);
    }
    if let Some(review) = &context.review {
        let team = team_value(conn, &task.team_id, REVIEW_INSTRUCTIONS).await?;
        let effective = crate::review_loop::config::effective_instructions(
            task.review_instructions.as_deref(),
            &team,
        );
        if effective != review.instructions {
            return Ok(false);
        }
    }
    Ok(true)
}

/// One team setting's text as a composer reads it: empty when absent.
async fn team_value(conn: &mut SqliteConnection, team_id: &str, key: &str) -> Result<String> {
    Ok(sqlx::query_scalar!(
        "SELECT value FROM team_settings WHERE team_id = ?1 AND key = ?2",
        team_id,
        key,
    )
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or_default())
}

/// One team setting as a revisioned text, `None` when it is absent or blank.
async fn team_text(
    conn: &mut SqliteConnection,
    team_id: &str,
    key: &str,
) -> Result<Option<Revision>> {
    let row = sqlx::query!(
        r#"SELECT value, revision, updated_by,
                  written_during_run AS "written_during_run: bool"
             FROM team_settings WHERE team_id = ?1 AND key = ?2"#,
        team_id,
        key,
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row
        .filter(|row| !row.value.trim().is_empty())
        .map(|row| Revision {
            revision: row.revision,
            author: row.updated_by,
            written_during_run: row.written_during_run,
        }))
}

async fn run_on_runner(conn: &mut SqliteConnection, run_id: &str) -> Result<RunOnRunner> {
    let row = sqlx::query!(
        r#"SELECT r.runner_id, ru.user_id AS "owner?"
             FROM runs r LEFT JOIN runners ru ON ru.id = r.runner_id
            WHERE r.id = ?1"#,
        run_id,
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match row {
        Some(row) => RunOnRunner {
            run_id: run_id.to_string(),
            runner_id: row.runner_id,
            owner: row.owner,
        },
        None => RunOnRunner {
            run_id: run_id.to_string(),
            runner_id: None,
            owner: None,
        },
    })
}

/// Every piece in `pieces` that `owner` does not consent to, in order, read
/// against the owner's acceptances and trust list for `team_id`.
pub(crate) async fn missing(
    conn: &mut SqliteConnection,
    owner: &str,
    team_id: &str,
    pieces: &[Piece],
) -> Result<Vec<(Piece, MissingReason)>> {
    if pieces.is_empty() {
        return Ok(Vec::new());
    }
    let accepted: Vec<Accepted> = sqlx::query!(
        r#"SELECT content AS "content: ContentKind", task_id, revision
             FROM acceptances WHERE user_id = ?1 AND team_id = ?2"#,
        owner,
        team_id,
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|row| Accepted {
        kind: row.content,
        task_id: row.task_id,
        revision: row.revision,
    })
    .collect();
    let trusted = sqlx::query_scalar!(
        "SELECT trusted_user_id FROM trusted_authors WHERE user_id = ?1 AND team_id = ?2",
        owner,
        team_id,
    )
    .fetch_all(&mut *conn)
    .await?;

    Ok(pieces
        .iter()
        .filter_map(|piece| match consents(owner, piece, &accepted, &trusted) {
            Consent::Consents => None,
            Consent::Missing { piece, reason } => Some((piece, reason)),
        })
        .collect())
}

/// What eligibility and the team ceiling read about a task.
#[derive(Debug, Clone)]
pub(crate) struct TaskRow {
    pub team_id: TeamId,
    pub personal_owner: Option<UserId>,
    pub assignee_id: Option<UserId>,
    pub assignee_login: Option<String>,
    pub pinned_runner_id: Option<RunnerId>,
    pub pinned_label: Option<String>,
    pub repository_name: String,
    pub allow_unattended_runs: bool,
}

impl TaskRow {
    pub fn facts(&self) -> TaskFacts<'_> {
        TaskFacts {
            assignee_id: self.assignee_id.as_deref(),
            team_id: &self.team_id,
            personal_owner: self.personal_owner.as_deref(),
        }
    }

    /// ADR-0032 point 4's team ceiling, which a personal team does not
    /// consult: its owner and the machine's owner are one person, so the
    /// runner's consent is the whole decision.
    pub fn team_ceiling(&self) -> TeamCeiling {
        match (self.personal_owner.is_some(), self.allow_unattended_runs) {
            (true, _) => TeamCeiling::NotConsulted,
            (false, true) => TeamCeiling::Allowed,
            (false, false) => TeamCeiling::Forbidden,
        }
    }
}

pub(crate) async fn task_row(
    conn: &mut SqliteConnection,
    task_id: &str,
) -> Result<Option<TaskRow>> {
    let row = sqlx::query!(
        r#"SELECT t.team_id, tm.personal_user_id, t.assignee_id, a.login AS "assignee_login?",
                  t.pinned_runner_id, p.label AS "pinned_label?", r.name AS repository_name,
                  r.allow_unattended_runs AS "allow_unattended_runs: bool"
             FROM tasks t
             JOIN teams tm ON tm.id = t.team_id
             JOIN repositories r ON r.id = t.repository_id
             LEFT JOIN users a ON a.id = t.assignee_id
             LEFT JOIN runners p ON p.id = t.pinned_runner_id
            WHERE t.id = ?1"#,
        task_id,
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| TaskRow {
        team_id: row.team_id,
        personal_owner: row.personal_user_id,
        assignee_id: row.assignee_id,
        assignee_login: row.assignee_login,
        pinned_runner_id: row.pinned_runner_id,
        pinned_label: row.pinned_label,
        repository_name: row.repository_name,
        allow_unattended_runs: row.allow_unattended_runs,
    }))
}

/// What eligibility reads about a runner.
#[derive(Debug, Clone)]
pub(crate) struct RunnerRow {
    pub owner: UserId,
    pub label: String,
    pub policy: RunnerEligibility,
    pub pool_teams: Vec<TeamId>,
}

impl RunnerRow {
    pub fn facts(&self) -> RunnerFacts<'_> {
        RunnerFacts {
            owner: &self.owner,
            policy: self.policy,
        }
    }
}

pub(crate) async fn runner_row(
    conn: &mut SqliteConnection,
    runner_id: &str,
) -> Result<Option<RunnerRow>> {
    let Some(row) = sqlx::query!(
        r#"SELECT user_id, label, eligibility AS "eligibility: RunnerEligibility"
             FROM runners WHERE id = ?1"#,
        runner_id,
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let pool_teams = sqlx::query_scalar!(
        "SELECT team_id FROM runner_pool_teams WHERE runner_id = ?1 ORDER BY team_id",
        runner_id,
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(Some(RunnerRow {
        owner: row.user_id,
        label: row.label,
        policy: row.eligibility,
        pool_teams,
    }))
}

async fn login_of(conn: &mut SqliteConnection, user_id: Option<&str>) -> Result<Option<String>> {
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    Ok(
        sqlx::query_scalar!("SELECT login FROM users WHERE id = ?1", user_id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// The refusal for a missing piece, with the names its sentence needs read on
/// `conn`.
pub(crate) async fn refusal_for(
    conn: &mut SqliteConnection,
    piece: &Piece,
    reason: MissingReason,
) -> Result<Ineligible> {
    let who = login_of(conn, piece.author.as_deref()).await?;
    let what = what(conn, piece).await?;
    Ok(Ineligible::ConsentMissing { what, who, reason })
}

/// `{what}` in Scope 11's sentences.
async fn what(conn: &mut SqliteConnection, piece: &Piece) -> Result<String> {
    Ok(match piece.kind {
        ContentKind::Plan => "the plan".to_string(),
        ContentKind::TaskReviewInstructions => "this task's review instructions".to_string(),
        ContentKind::BaseInstructions => "the team's base instructions".to_string(),
        ContentKind::ReviewInstructions => "the team's review instructions".to_string(),
        ContentKind::ReviewFindings => {
            format!("the findings recorded in run {}", piece.revision)
        }
        ContentKind::BaseCommit => {
            let title = match piece.task_id.as_deref() {
                Some(task_id) => {
                    sqlx::query_scalar!("SELECT title FROM tasks WHERE id = ?1", task_id)
                        .fetch_optional(&mut *conn)
                        .await?
                        .unwrap_or_default()
                }
                None => String::new(),
            };
            format!("commit {} from \"{title}\"", piece.revision)
        }
    })
}

// ---------------------------------------------------------------------------
// What a claim is refused for
// ---------------------------------------------------------------------------

/// The first reason `board::lease::eligible` refuses a runner a task: one
/// value, which every caller renders and none re-derives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ineligible {
    /// ADR-0031 point 4: another runner holds the task's next attempt.
    PinnedElsewhere {
        label: String,
    },
    /// Pinned to this runner, then assigned to someone else: nobody can take
    /// it until someone chooses to run it elsewhere (task 057).
    PinnedThenReassigned {
        label: String,
        login: String,
    },
    AssignedToSomeoneElse {
        login: String,
    },
    OutsideThePool {
        label: String,
    },
    /// ADR-0032 point 4: the team does not allow unattended runs here.
    ForbiddenByTeam {
        repository: String,
    },
    /// Task 067's model rule.
    ModelNotOffered {
        model: String,
        provider: ProviderId,
    },
    /// The runner's strategy ceiling, a cost control.
    CeilingExceeded {
        label: String,
        exceeded: CeilingExceeded,
    },
    ConsentMissing {
        what: String,
        /// The author's login, `None` for a former member.
        who: Option<String>,
        reason: MissingReason,
    },
}

impl Ineligible {
    /// The sentence a person reads (Scope 11's table, quoted by 057 and 061).
    pub fn refusal(&self) -> String {
        match self {
            Ineligible::PinnedElsewhere { label } => format!(
                "this task is pinned to {label}, which has its worktree and the agent's \
                 conversation. Only that runner can run it until someone chooses to run it \
                 elsewhere."
            ),
            Ineligible::PinnedThenReassigned { label, login } => format!(
                "this task is pinned to {label}, but it is now assigned to @{login}. No runner \
                 can take it until someone chooses to run it elsewhere."
            ),
            Ineligible::AssignedToSomeoneElse { login } => format!(
                "this task is assigned to @{login}. Only their runners run it: reassign it to \
                 run it here."
            ),
            Ineligible::OutsideThePool { label } => format!(
                "this task is unassigned, and {label} does not take pool work from this team. \
                 Assign it to yourself, or add the team to the runner's pool."
            ),
            Ineligible::ForbiddenByTeam { repository } => format!(
                "the team does not allow unattended runs in {repository}. A team owner can \
                 allow them."
            ),
            Ineligible::ModelNotOffered { model, provider } => format!(
                "this task asks for the model \"{model}\", which {} cannot run. Change the \
                 task's model, or run it on a runner whose provider offers it.",
                provider.display_name()
            ),
            Ineligible::CeilingExceeded {
                label,
                exceeded: CeilingExceeded::Model { model },
            } => format!(
                "this task asks for the model \"{model}\", which {label}'s strategy ceiling does \
                 not allow. Change the task's model, or run it on another runner."
            ),
            Ineligible::CeilingExceeded {
                label,
                exceeded: CeilingExceeded::Effort { effort, max_effort },
            } => format!(
                "this task asks for the effort \"{effort}\", above {label}'s ceiling of \
                 \"{max_effort}\". Nothing is lowered for it: change the task's effort, or raise \
                 the ceiling."
            ),
            Ineligible::ConsentMissing {
                what,
                who: Some(login),
                reason: MissingReason::WrittenDuringRun,
            } => format!(
                "{what} was written with @{login}'s credentials during a run on someone else's \
                 task. Only accepting that revision lets it run."
            ),
            Ineligible::ConsentMissing {
                what,
                who: Some(login),
                reason: MissingReason::NotAccepted,
            } => format!(
                "{what} was changed by @{login}, and you have not accepted that revision. Accept \
                 it, or trust @{login}'s changes."
            ),
            Ineligible::ConsentMissing { what, .. } => {
                format!("{what} was changed by a former member. Accept that revision to run it.")
            }
        }
    }

    /// The reason the queue shows for a task it passes over, or `None` for a
    /// refusal another runner resolves with nobody acting: a pin, the model
    /// rule and the strategy ceiling, which `Next` passes over silently (D21
    /// point 3, D23 point 4).
    pub fn skip_reason(&self) -> Option<SkipReason> {
        match self {
            Ineligible::PinnedThenReassigned { .. }
            | Ineligible::AssignedToSomeoneElse { .. }
            | Ineligible::OutsideThePool { .. } => Some(SkipReason::NotEligible),
            Ineligible::ConsentMissing { .. } => Some(SkipReason::ConsentMissing),
            Ineligible::ForbiddenByTeam { .. } => Some(SkipReason::ForbiddenByTeam),
            Ineligible::PinnedElsewhere { .. }
            | Ineligible::ModelNotOffered { .. }
            | Ineligible::CeilingExceeded { .. } => None,
        }
    }
}

/// The refusal for an eligibility answer that is not eligible, with the
/// names its sentence needs.
pub(crate) fn not_eligible(
    task: &TaskRow,
    runner: Option<&RunnerRow>,
    reason: Reason,
    runner_id: &str,
) -> Ineligible {
    let label = runner.map_or_else(|| runner_id.to_string(), |runner| runner.label.clone());
    let login = task.assignee_login.clone().unwrap_or_default();
    match reason {
        Reason::AssignedToSomeoneElse if task.pinned_runner_id.as_deref() == Some(runner_id) => {
            Ineligible::PinnedThenReassigned { label, login }
        }
        Reason::AssignedToSomeoneElse => Ineligible::AssignedToSomeoneElse { login },
        Reason::Unassigned => Ineligible::OutsideThePool { label },
    }
}

/// The footing an eligible runner takes a task on, which orders its queue:
/// the owner's own tasks first, then the pool's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Route {
    Assigned,
    Pool,
}

/// Eligibility for a task and a runner, or the refusal.
pub(crate) fn decide(
    task: &TaskRow,
    runner: Option<&RunnerRow>,
    runner_id: &str,
) -> std::result::Result<Route, Ineligible> {
    let unknown = RunnerRow {
        owner: String::new(),
        label: runner_id.to_string(),
        policy: RunnerEligibility::Assigned,
        pool_teams: Vec::new(),
    };
    let row = runner.unwrap_or(&unknown);
    match eligibility::decide(&task.facts(), &row.facts(), &row.pool_teams) {
        Eligibility::Assigned => Ok(Route::Assigned),
        Eligibility::Pool => Ok(Route::Pool),
        Eligibility::NotEligible(reason) => Err(not_eligible(task, runner, reason, runner_id)),
    }
}

// ---------------------------------------------------------------------------
// Point 6's mark
// ---------------------------------------------------------------------------

/// Whether a write by `actor` now is written during a run: a live lease is
/// held by a runner of `actor`'s, and the content that lease's run would
/// execute names an author other than `actor` (ADR-0032 point 6).
///
/// Reusing [`pieces_for`] makes "content authored by someone else" mean the
/// same thing here and in the check. **It reads across all of the actor's
/// runners and teams, ignoring `ctx.scope`**: Bob's runner can hold a lease in
/// a shared team while Bob writes into his personal team, and that is exactly
/// the path point 6 closes. It is safe because it returns one bool about the
/// actor's own write and nothing else crosses back. For a user who belongs
/// only to a personal team every lease's content is their own, so the mark is
/// never set; that is a consequence, not a team-kind rule.
///
/// Read over the pool, so each writer asks it before its own transaction
/// opens: the composition behind a lease is read over the pool too.
pub async fn written_during_run(ctx: &ServiceContext, actor: &str) -> Result<bool> {
    let now = ctx.clock.now();
    let leases = sqlx::query!(
        r#"SELECT l.task_id, l.purpose AS "purpose: LeasePurpose", l.runner_id, t.team_id
             FROM runner_leases l
             JOIN runners r ON r.id = l.runner_id
             JOIN tasks t ON t.id = l.task_id
            WHERE r.user_id = ?1
              AND (l.expires_at IS NULL OR l.expires_at > ?2)
            ORDER BY l.task_id"#,
        actor,
        now,
    )
    .fetch_all(&ctx.pool)
    .await?;

    for lease in leases {
        let scoped = ctx.with_scope(crate::context::TeamScope::one(lease.team_id.clone()));
        let findings = matches!(lease.purpose, LeasePurpose::Review | LeasePurpose::Fix);
        let composition = composition(&scoped, &lease.task_id, findings).await?;
        let mut conn = ctx.pool.acquire().await?;
        let Some(inputs) =
            inputs(&mut conn, &lease.task_id, &lease.runner_id, &composition).await?
        else {
            continue;
        };
        if pieces_for(lease.purpose, &inputs)
            .iter()
            .any(|piece| piece.author.as_deref() != Some(actor))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// Trust
// ---------------------------------------------------------------------------

/// Adds `user_id` to, or removes them from, `ctx.actor`'s trust list for
/// `team_id`. Trust is personal: nobody writes another person's list.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), team_id = %team_id))]
pub async fn set_trust(
    ctx: &ServiceContext,
    team_id: &str,
    user_id: &str,
    trusted: bool,
) -> Result<Vec<UserId>> {
    ensure_team_in_scope(ctx, team_id)?;
    if user_id == ctx.actor {
        return Err(Error::invalid(trusting_oneself()));
    }
    let mut tx = ctx.begin_immediate().await?;
    ensure_member(&mut tx, team_id, user_id).await?;
    let now = ctx.clock.now();
    if trusted {
        sqlx::query!(
            "INSERT OR IGNORE INTO trusted_authors (user_id, team_id, trusted_user_id, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            ctx.actor,
            team_id,
            user_id,
            now,
        )
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query!(
            "DELETE FROM trusted_authors
              WHERE user_id = ?1 AND team_id = ?2 AND trusted_user_id = ?3",
            ctx.actor,
            team_id,
            user_id,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    // Every card in the team may have changed runnability for this person.
    ctx.publish(ChangeEvent::settings(team_id.to_string()));
    list_trusted(ctx, team_id).await
}

/// `ctx.actor`'s own trust list for `team_id`, by user id, and nobody else's.
pub async fn list_trusted(ctx: &ServiceContext, team_id: &str) -> Result<Vec<UserId>> {
    ensure_team_in_scope(ctx, team_id)?;
    Ok(sqlx::query_scalar!(
        "SELECT trusted_user_id FROM trusted_authors
          WHERE user_id = ?1 AND team_id = ?2 ORDER BY trusted_user_id",
        ctx.actor,
        team_id,
    )
    .fetch_all(&ctx.pool)
    .await?)
}

// ---------------------------------------------------------------------------
// Acceptance
// ---------------------------------------------------------------------------

/// Records that `ctx.actor` accepts `kind` at `revision`, for `task_id` or,
/// for the two team-wide kinds, for `team_id`.
///
/// Only the **current** revision can be accepted, so nobody accepts text they
/// did not see; a stale one is `Invalid`, naming the current revision and who
/// wrote it (never `Conflict`, which means only "your lease is not the current
/// one"). Accepting twice is idempotent.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), team_id = %team_id))]
pub async fn accept(
    ctx: &ServiceContext,
    team_id: &str,
    task_id: Option<&str>,
    kind: ContentKind,
    revision: &str,
) -> Result<()> {
    ensure_team_in_scope(ctx, team_id)?;
    if kind.is_team_wide() != task_id.is_none() {
        return Err(Error::invalid(if kind.is_team_wide() {
            format!("{kind} belongs to the team, so accepting it names no task")
        } else {
            format!("accepting {kind} names the task it belongs to")
        }));
    }
    if let Some(task_id) = task_id {
        let team = crate::tasks::service::team_of(ctx, task_id).await?;
        if team != team_id {
            return Err(Error::not_found(format!("no task with id {task_id}")));
        }
    }

    // The pieces whose current revision is a run id or a commit are the ones
    // the task lists now, for any purpose, and they are read over the pool
    // before the write lock is taken.
    let listed = match (kind, task_id) {
        (ContentKind::ReviewFindings | ContentKind::BaseCommit, Some(task_id)) => {
            current_listed(ctx, task_id, kind).await?
        }
        _ => Vec::new(),
    };

    let mut tx = ctx.begin_immediate().await?;
    let current: Vec<Piece> = match (kind, task_id) {
        (ContentKind::Plan | ContentKind::TaskReviewInstructions, Some(task_id)) => {
            let row = sqlx::query!(
                r#"SELECT plan_revision, plan_updated_by, review_instructions_revision,
                          review_instructions_updated_by
                     FROM tasks WHERE id = ?1"#,
                task_id,
            )
            .fetch_one(&mut *tx)
            .await?;
            let (revision, author) = match kind {
                ContentKind::Plan => (row.plan_revision, row.plan_updated_by),
                _ => (
                    row.review_instructions_revision,
                    row.review_instructions_updated_by,
                ),
            };
            vec![current_piece(kind, Some(task_id), revision, author)]
        }
        (ContentKind::BaseInstructions | ContentKind::ReviewInstructions, None) => {
            let key = match kind {
                ContentKind::BaseInstructions => BASE_INSTRUCTIONS,
                _ => REVIEW_INSTRUCTIONS,
            };
            sqlx::query!(
                "SELECT revision, updated_by FROM team_settings WHERE team_id = ?1 AND key = ?2",
                team_id,
                key,
            )
            .fetch_optional(&mut *tx)
            .await?
            .map(|row| current_piece(kind, None, row.revision, row.updated_by))
            .into_iter()
            .collect()
        }
        _ => listed,
    };

    if !current.iter().any(|piece| piece.revision == revision) {
        let Some(newest) = current.first() else {
            return Err(Error::invalid(format!(
                "there is no {kind} to accept here: nothing a run would execute is at revision \
                 {revision}"
            )));
        };
        let who = login_of(&mut tx, newest.author.as_deref()).await?;
        let what = what(&mut tx, newest).await?;
        return Err(Error::invalid(stale_acceptance(
            revision,
            &what,
            &newest.revision,
            who.as_deref(),
        )));
    }

    let id = new_id();
    let now = ctx.clock.now();
    sqlx::query!(
        "INSERT OR IGNORE INTO acceptances
            (id, user_id, team_id, task_id, content, revision, accepted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        id,
        ctx.actor,
        team_id,
        task_id,
        kind,
        revision,
        now,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    ctx.publish(match task_id {
        Some(task_id) => ChangeEvent::tasks(team_id.to_string(), [task_id.to_string()]),
        None => ChangeEvent::settings(team_id.to_string()),
    });
    Ok(())
}

fn current_piece(
    kind: ContentKind,
    task_id: Option<&str>,
    revision: i64,
    author: Option<UserId>,
) -> Piece {
    Piece {
        kind,
        task_id: task_id.map(str::to_string),
        revision: revision.to_string(),
        author,
        written_during_run: false,
    }
}

/// Every piece of `kind` that `task_id` lists now, for any purpose and
/// whichever runner would run it.
///
/// A [`ContentKind::BaseCommit`] piece names the *dependency* as its task, so
/// its current revision is the commit that dependency offers its dependents
/// now: its latest successful head (D29 point 5), credited to every owner
/// whose runners' work that commit holds.
async fn current_listed(
    ctx: &ServiceContext,
    task_id: &str,
    kind: ContentKind,
) -> Result<Vec<Piece>> {
    if kind == ContentKind::BaseCommit {
        let Some(head) = crate::runs::latest_successful_head(ctx, task_id).await? else {
            return Ok(Vec::new());
        };
        let mut conn = ctx.pool.acquire().await?;
        let attempt = sqlx::query_scalar!("SELECT attempt FROM runs WHERE id = ?1", head.run_id)
            .fetch_one(&mut *conn)
            .await?;
        let owners = crate::runs::commit_authors(&mut conn, task_id, attempt).await?;
        return Ok(owners
            .into_iter()
            .map(|author| Piece {
                kind,
                task_id: Some(task_id.to_string()),
                revision: head.head_sha.clone(),
                author,
                written_during_run: false,
            })
            .collect());
    }

    let composition = composition(ctx, task_id, true).await?;
    let mut conn = ctx.pool.acquire().await?;
    // No runner's id: every recorded run is "another runner's" here.
    let Some(inputs) = inputs(&mut conn, task_id, "", &composition).await? else {
        return Ok(Vec::new());
    };
    let mut listed: Vec<Piece> = Vec::new();
    for purpose in [
        LeasePurpose::Fix,
        LeasePurpose::Review,
        LeasePurpose::Implementation,
    ] {
        for piece in pieces_for(purpose, &inputs) {
            if piece.kind == kind && !listed.contains(&piece) {
                listed.push(piece);
            }
        }
    }
    Ok(listed)
}

// ---------------------------------------------------------------------------
// What a card shows: the read 061 draws from
// ---------------------------------------------------------------------------

/// Whether a runner may take a task at all, as the card says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EligibilityStatus {
    Assigned,
    Pool,
    AssignedToSomeoneElse,
    Unassigned,
}

/// ADR-0032 point 4's team ceiling, as the card says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TeamCeiling {
    /// A personal team: the runner's own consent is the whole decision.
    NotConsulted,
    Allowed,
    Forbidden,
}

/// One piece the runner's owner has not consented to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MissingPiece {
    #[schemars(with = "String")]
    pub kind: ContentKind,
    pub task_id: Option<TaskId>,
    pub revision: String,
    /// `None` for a former member.
    pub author_login: Option<String>,
    #[schemars(with = "String")]
    pub reason: MissingReason,
}

/// Everything that decides whether a runner would take a task, for its owner
/// to read. Holds no path (ADR-0028 point 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskConsent {
    pub eligibility: EligibilityStatus,
    pub pinned_runner_id: Option<RunnerId>,
    pub team_ceiling: TeamCeiling,
    pub missing: Vec<MissingPiece>,
}

/// Why `runner_id` would or would not take `task_id`: eligibility, the pin,
/// the team ceiling, and every missing piece for the purpose a claim would
/// lease it as now. `runner_id` must be one of `ctx.actor`'s runners, because
/// what a person has accepted is theirs to read.
pub async fn status(ctx: &ServiceContext, task_id: &str, runner_id: &str) -> Result<TaskConsent> {
    let task = crate::tasks::get_task(ctx, task_id).await?;
    let purpose = purpose_now(ctx, &task.task).await?;
    let findings = matches!(purpose, LeasePurpose::Review | LeasePurpose::Fix);
    let composition = composition(ctx, task_id, findings).await?;

    let mut conn = ctx.pool.acquire().await?;
    let runner = runner_row(&mut conn, runner_id)
        .await?
        .filter(|runner| runner.owner == ctx.actor)
        .ok_or_else(|| Error::not_found(format!("no runner with id {runner_id}")))?;
    let row = task_row(&mut conn, task_id)
        .await?
        .ok_or_else(|| Error::not_found(format!("no task with id {task_id}")))?;

    let eligibility = match eligibility::decide(&row.facts(), &runner.facts(), &runner.pool_teams) {
        Eligibility::Assigned => EligibilityStatus::Assigned,
        Eligibility::Pool => EligibilityStatus::Pool,
        Eligibility::NotEligible(Reason::AssignedToSomeoneElse) => {
            EligibilityStatus::AssignedToSomeoneElse
        }
        Eligibility::NotEligible(Reason::Unassigned) => EligibilityStatus::Unassigned,
    };

    let mut pieces_missing = Vec::new();
    if let Some(inputs) = inputs(&mut conn, task_id, runner_id, &composition).await? {
        let pieces = pieces_for(purpose, &inputs);
        for (piece, reason) in missing(&mut conn, &runner.owner, &row.team_id, &pieces).await? {
            pieces_missing.push(MissingPiece {
                author_login: login_of(&mut conn, piece.author.as_deref()).await?,
                kind: piece.kind,
                task_id: piece.task_id,
                revision: piece.revision,
                reason,
            });
        }
    }

    Ok(TaskConsent {
        eligibility,
        pinned_runner_id: row.pinned_runner_id.clone(),
        team_ceiling: row.team_ceiling(),
        missing: pieces_missing,
    })
}

/// The purpose a claim of `task` would lease now: the kind a due retry
/// resumes as, the planner for a fresh start that needs one, the
/// implementation otherwise.
pub(crate) async fn purpose_now(
    ctx: &ServiceContext,
    task: &crate::db::Task,
) -> Result<LeasePurpose> {
    if task.run_state == crate::db::RunState::WaitingRetry {
        if let Some(point) = crate::scheduler::attempts::resume_point(ctx, &task.id).await? {
            return Ok(point.kind.into());
        }
    }
    let team_id = crate::tasks::service::team_of(ctx, &task.id).await?;
    let global = crate::strategy::settings::global_default_for(ctx, &team_id).await?;
    let repository =
        crate::strategy::settings::repository_default(ctx, &task.repository_id).await?;
    let mode = crate::strategy::effective_strategy(task, &repository, &global).mode;
    Ok(if crate::tasks::strategy::needs_planning(task, mode) {
        LeasePurpose::Strategy
    } else {
        LeasePurpose::Implementation
    })
}

// ---------------------------------------------------------------------------
// The runner's eligibility policy
// ---------------------------------------------------------------------------

/// Sets a runner's eligibility policy and replaces its pool list whole, in
/// one transaction (ADR-0032 point 2). Only the runner's owner may, and every
/// pool team must be one the owner is a member of.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), runner_id = %runner_id))]
pub async fn set_runner_eligibility(
    ctx: &ServiceContext,
    runner_id: &str,
    policy: RunnerEligibility,
    pool_team_ids: &[TeamId],
) -> Result<()> {
    let mut tx = ctx.begin_immediate().await?;
    let owner = sqlx::query_scalar!("SELECT user_id FROM runners WHERE id = ?1", runner_id)
        .fetch_optional(&mut *tx)
        .await?
        .filter(|owner| *owner == ctx.actor)
        .ok_or_else(|| Error::not_found(format!("no runner with id {runner_id}")))?;

    let teams: BTreeSet<&TeamId> = pool_team_ids.iter().collect();
    for team_id in &teams {
        let member = sqlx::query_scalar!(
            "SELECT 1 AS \"member!: i64\" FROM team_memberships WHERE team_id = ?1 AND user_id = ?2",
            team_id,
            owner,
        )
        .fetch_optional(&mut *tx)
        .await?;
        if member.is_none() {
            return Err(Error::not_found(format!("no team with id {team_id}")));
        }
    }

    sqlx::query!(
        "UPDATE runners SET eligibility = ?1 WHERE id = ?2",
        policy,
        runner_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM runner_pool_teams WHERE runner_id = ?1",
        runner_id
    )
    .execute(&mut *tx)
    .await?;
    for team_id in &teams {
        sqlx::query!(
            "INSERT INTO runner_pool_teams (runner_id, team_id) VALUES (?1, ?2)",
            runner_id,
            team_id,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    for team in ctx.scope.teams() {
        ctx.publish(ChangeEvent::settings(team.clone()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared checks and sentences
// ---------------------------------------------------------------------------

fn ensure_team_in_scope(ctx: &ServiceContext, team_id: &str) -> Result<()> {
    if !ctx.scope.contains(team_id) {
        return Err(Error::not_found(format!("no team with id {team_id}")));
    }
    Ok(())
}

/// `NotFound` for a user who is not a member of `team_id`, worded as for a
/// user who does not exist (039's rule).
pub(crate) async fn ensure_member(
    conn: &mut SqliteConnection,
    team_id: &str,
    user_id: &str,
) -> Result<()> {
    let member = sqlx::query_scalar!(
        "SELECT 1 AS \"member!: i64\" FROM team_memberships WHERE team_id = ?1 AND user_id = ?2",
        team_id,
        user_id,
    )
    .fetch_optional(&mut *conn)
    .await?;
    match member {
        Some(_) => Ok(()),
        None => Err(Error::not_found(format!("no user with id {user_id}"))),
    }
}

/// Scope 11's stale-acceptance sentence.
pub fn stale_acceptance(given: &str, what: &str, current: &str, who: Option<&str>) -> String {
    let who = who.map_or_else(
        || "a former member".to_string(),
        |login| format!("@{login}"),
    );
    format!(
        "revision {given} is not current: {what} is at revision {current}, changed by {who}. \
         Read it before accepting it."
    )
}

/// Scope 11's sentence for trusting oneself.
pub fn trusting_oneself() -> &'static str {
    "you cannot trust yourself: your own changes already count."
}

/// Scope 11's sentence for a member changing the team ceiling.
pub fn ceiling_needs_an_owner() -> &'static str {
    "only an owner of this team can change whether it allows unattended runs."
}

/// Scope 11's sentence for the ceiling command on a personal team.
pub fn personal_team_has_no_ceiling() -> &'static str {
    "a personal team has no ceiling: this machine's own consent decides."
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn consent_missing(who: Option<&str>, reason: MissingReason) -> Ineligible {
        Ineligible::ConsentMissing {
            what: "the plan".to_string(),
            who: who.map(str::to_string),
            reason,
        }
    }

    #[test]
    fn every_refusal_reads_exactly() {
        let rows: Vec<(String, &str)> = vec![
            (
                Ineligible::AssignedToSomeoneElse { login: "alice".into() }.refusal(),
                "this task is assigned to @alice. Only their runners run it: reassign it to run it here.",
            ),
            (
                Ineligible::OutsideThePool { label: "Mac mini".into() }.refusal(),
                "this task is unassigned, and Mac mini does not take pool work from this team. Assign it to yourself, or add the team to the runner's pool.",
            ),
            (
                Ineligible::PinnedThenReassigned {
                    label: "Mac mini".into(),
                    login: "alice".into(),
                }
                .refusal(),
                "this task is pinned to Mac mini, but it is now assigned to @alice. No runner can take it until someone chooses to run it elsewhere.",
            ),
            (
                consent_missing(Some("alice"), MissingReason::NotAccepted).refusal(),
                "the plan was changed by @alice, and you have not accepted that revision. Accept it, or trust @alice's changes.",
            ),
            (
                consent_missing(None, MissingReason::FormerMember).refusal(),
                "the plan was changed by a former member. Accept that revision to run it.",
            ),
            (
                consent_missing(Some("bob"), MissingReason::WrittenDuringRun).refusal(),
                "the plan was written with @bob's credentials during a run on someone else's task. Only accepting that revision lets it run.",
            ),
            (
                Ineligible::ForbiddenByTeam { repository: "widgets".into() }.refusal(),
                "the team does not allow unattended runs in widgets. A team owner can allow them.",
            ),
            (
                Ineligible::CeilingExceeded {
                    label: "Mac mini".into(),
                    exceeded: CeilingExceeded::Model { model: "opus".into() },
                }
                .refusal(),
                "this task asks for the model \"opus\", which Mac mini's strategy ceiling does not allow. Change the task's model, or run it on another runner.",
            ),
            (
                Ineligible::CeilingExceeded {
                    label: "Mac mini".into(),
                    exceeded: CeilingExceeded::Effort {
                        effort: "high".into(),
                        max_effort: "medium".into(),
                    },
                }
                .refusal(),
                "this task asks for the effort \"high\", above Mac mini's ceiling of \"medium\". Nothing is lowered for it: change the task's effort, or raise the ceiling.",
            ),
            (
                stale_acceptance("3", "the plan", "4", Some("alice")),
                "revision 3 is not current: the plan is at revision 4, changed by @alice. Read it before accepting it.",
            ),
            (
                stale_acceptance("3", "the plan", "4", None),
                "revision 3 is not current: the plan is at revision 4, changed by a former member. Read it before accepting it.",
            ),
            (
                trusting_oneself().to_string(),
                "you cannot trust yourself: your own changes already count.",
            ),
            (
                ceiling_needs_an_owner().to_string(),
                "only an owner of this team can change whether it allows unattended runs.",
            ),
            (
                personal_team_has_no_ceiling().to_string(),
                "a personal team has no ceiling: this machine's own consent decides.",
            ),
        ];
        for (actual, expected) in rows {
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn a_composition_names_each_run_behind_its_findings_once() {
        assert_eq!(
            distinct(
                [
                    Some(&"a".to_string()),
                    None,
                    Some(&"b".to_string()),
                    Some(&"a".to_string())
                ]
                .into_iter()
            ),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
