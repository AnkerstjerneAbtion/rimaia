//! Registered local git repositories: registration, validation, and read-only
//! git inspection (default branch, remote, `gh` readiness) — task 003
//! (ADR-0002, ADR-0005, ADR-0012).
//!
//! Repository state on disk is authoritative — the database records paths
//! and branches, and startup reconciles rows against reality rather than
//! trusting them (ADR-0005). That is also why there is no stored remote URL:
//! [`remote_info`] answers it fresh every call, exactly as
//! [`Repository`]'s own doc comment says.
//!
//! Every function here takes [`&ServiceContext`](ServiceContext) and no
//! `AppHandle`, so the Tauri shell (task 010) and, eventually, the MCP server
//! are both thin adapters over this module rather than a second
//! implementation of its rules (ADR-0006).

mod git;
mod naming;

/// The two binaries this module shells out to, resolved through `PATH`.
/// Re-exported so task 018's doctor probes the same names, rather than
/// spelling them a second time somewhere they could drift apart.
pub use git::{GH_CLI, GIT_CLI};

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::context::{ScopedTx, ServiceContext};
use crate::db::{OnArchive, Repository};
use crate::error::{Error, Result};
use crate::events::{ChangeEvent, TeamId};
use crate::machine::{self, Checkout, CheckoutPatch, CheckoutView, MachineContext};
use crate::scheduler::CONCURRENCY_CEILING;
use crate::tasks::Patch;

/// What registering a new local repository needs.
///
/// `name` and `worktree_root` override what task 003 would otherwise derive
/// — the directory's basename, and `<worktrees_dir>/<slug>` — which is what
/// an "add repository" dialog shows before the user edits anything. Passing
/// `None` for either takes the derived default.
#[derive(Debug, Clone)]
pub struct NewRepository {
    pub path: String,
    pub name: Option<String>,
    pub worktree_root: Option<String>,
}

/// An edit to the board's half of an already-registered repository.
///
/// A patch, not a replacement: every field left `None` is left unchanged, the
/// same shape task 004's task edits use and for the same reason — an "edit
/// default branch" form has no business also overwriting the name.
///
/// Only what the board holds. The worktree root and the per-repository cap are
/// this machine's checkout since task 066, and have setters of their own:
/// [`set_worktree_root`] and [`set_max_concurrency`].
#[derive(Debug, Clone, Default)]
pub struct RepositoryPatch {
    pub name: Option<String>,
    pub default_branch: Option<String>,
}

/// What live inspection of a repository's remote found (task 003's "detect
/// the remote URL and whether `gh` is available and authenticated for it").
/// Computed fresh on every call, never cached — see the module doc for why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteInfo {
    pub remote_url: Option<String>,
    /// `None` when there is no remote to open a PR against, so the question
    /// does not apply. `Some(false)` is task 003's warning case — `gh` is
    /// missing, or not authenticated for the remote's host — which base
    /// instructions that ask for a PR (ADR-0009) read as a reason to skip
    /// that step rather than a reason to fail the run.
    pub gh_ready: Option<bool>,
}

/// Every repository the context's one team registered, alphabetically — the
/// order the Settings list shows them in.
///
/// Entity-less, so it acts on one team (ADR-0035 point 2): a context that
/// reaches several is refused rather than handed a mixed list.
pub async fn list(ctx: &ServiceContext) -> Result<Vec<Repository>> {
    let team_id = ctx.scope.sole()?;
    let repositories = sqlx::query_as!(
        Repository,
        r#"
        SELECT id, name, default_branch, allow_unattended_runs,
               created_at AS "created_at: chrono::DateTime<chrono::Utc>"
        FROM repositories
        WHERE team_id = ?1
        ORDER BY name ASC, created_at ASC
        "#,
        team_id,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(repositories)
}

/// This machine's checkouts of the repositories the context's one team holds,
/// ordered by repository id: what `list_checkouts` answers, through both
/// doors (task 066).
///
/// A checkout of a repository the caller cannot see is left out, so one
/// machine shared by two teams' clones tells neither about the other (ADR-0029
/// point 5); in solo that is every checkout. The board is read through
/// [`list`], the named read a local handler may make (D32's 2026-10-04
/// amendment).
pub async fn checkouts(
    ctx: &ServiceContext,
    machine: &MachineContext,
) -> Result<Vec<CheckoutView>> {
    let visible: std::collections::HashSet<String> = list(ctx)
        .await?
        .into_iter()
        .map(|repository| repository.id)
        .collect();
    Ok(machine
        .store
        .list_checkouts()
        .await?
        .into_iter()
        .filter(|checkout| visible.contains(&checkout.repository_id))
        .map(CheckoutView::from)
        .collect())
}

/// One repository by id. `Error::not_found` when there is none in the
/// context's scope — task 003's removal and edit paths both start here, so
/// both get that message for free rather than reimplementing "does this id
/// exist", and another team's repository gets exactly the same one.
pub async fn get(ctx: &ServiceContext, id: &str) -> Result<Repository> {
    fetch_repository_row(&ctx.pool, &ctx.scope.json(), id).await
}

/// The one place a repository row is read back — used both inside a
/// transaction (`&mut *tx`, before a write that depends on the current row,
/// the way [`update`] does) and against the bare pool (a plain [`get`]).
///
/// Scoped in the query, never fetched and compared after: `scope` is
/// [`TeamScope::json`](crate::TeamScope::json), and a row outside it is not
/// there.
async fn fetch_repository_row<'e, E>(executor: E, scope: &str, id: &str) -> Result<Repository>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query_as!(
        Repository,
        r#"
        SELECT id, name, default_branch, allow_unattended_runs,
               created_at AS "created_at: chrono::DateTime<chrono::Utc>"
        FROM repositories
        WHERE id = ?1 AND team_id IN (SELECT value FROM json_each(?2))
        "#,
        id,
        scope,
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| Error::not_found(format!("no repository with id {id}")))
}

/// The team repository `id` belongs to, inside the caller's transaction, in
/// the sentence [`fetch_repository_row`] uses for one the scope does not hold.
///
/// What a task created in it is owned by (ADR-0029 point 1), and what an
/// event about it names (ADR-0034 point 3).
pub(crate) async fn team_of_repository(tx: &mut ScopedTx, id: &str) -> Result<TeamId> {
    let scope = tx.scope().json();
    select_team(&mut **tx, &scope, id).await
}

/// [`team_of_repository`] over the pool: the team whose settings a
/// repository-level read or write goes to.
pub(crate) async fn team_of(ctx: &ServiceContext, id: &str) -> Result<TeamId> {
    select_team(&ctx.pool, &ctx.scope.json(), id).await
}

async fn select_team<'e, E>(executor: E, scope: &str, id: &str) -> Result<TeamId>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query_scalar!(
        "SELECT team_id FROM repositories
         WHERE id = ?1 AND team_id IN (SELECT value FROM json_each(?2))",
        id,
        scope,
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| Error::not_found(format!("no repository with id {id}")))
}

/// Validates and registers a local repository: the board's row, then this
/// machine's checkout of it.
///
/// `worktrees_dir` is the shell-resolved `<app-data>/worktrees`
/// ([`crate::paths::AppPaths::worktrees_dir`]) — core derives paths and never
/// discovers them, so the app-data root arrives as a parameter rather than
/// being looked up here.
///
/// Each of task 003's four checks produces its own message rather than a
/// generic "invalid repository", and runs in the order the task lists them:
/// the path must exist and be a directory, it must be a git repository and
/// not a worktree of another one, it must have at least one commit, and a
/// default branch must be determinable. The first failure wins — there is no
/// use validating a default branch in a directory that turned out not to be
/// a repository at all.
///
/// Two more checks: a clone this machine already has a checkout of is refused
/// rather than silently duplicated (checked first, right after
/// canonicalization — it needs no git introspection), and a caller-supplied
/// `name` or `worktree_root` is held to the same non-blank rule the setters
/// enforce — a blank value must not be storable through one door and refused
/// through the other (ADR-0006).
///
/// A local command in solo until task 054 splits it (D32's appendix). The
/// board's row names no retired column, so its `path` and `worktree_root` are
/// `NULL` and the rest take their defaults; everything about the clone is
/// this machine's [`Checkout`] (ADR-0033 point 2).
pub async fn register(
    ctx: &ServiceContext,
    machine: &MachineContext,
    worktrees_dir: &Path,
    new: NewRepository,
) -> Result<Repository> {
    // A repository is the root of what a team owns, so there is no parent row
    // to take its team from: the request has to name exactly one (ADR-0029
    // point 5). Read before anything else, so a scope of several teams is
    // refused rather than half-applied.
    let team_id = ctx.scope.sole()?.clone();

    let requested = Path::new(&new.path);
    let canonical = validate_directory(requested).await?;
    let path = path_to_string(&canonical)?;
    ensure_not_already_registered(machine, &path).await?;
    validate_is_a_registrable_git_repository(&canonical).await?;
    validate_has_at_least_one_commit(&canonical).await?;
    let default_branch = resolve_default_branch(&canonical).await?;

    let name = match new.name {
        Some(name) => require_non_empty(name, "name")?,
        None => naming::derive_name(&canonical),
    };
    let worktree_root = match new.worktree_root {
        Some(root) => require_non_empty(root, "worktree root")?,
        None => default_worktree_root(worktrees_dir, &name)?,
    };

    let id = crate::db::new_id();
    let created_at = ctx.clock.now();

    sqlx::query!(
        r#"
        INSERT INTO repositories (id, team_id, name, default_branch, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        id,
        team_id,
        name,
        default_branch,
        created_at,
    )
    .execute(&ctx.pool)
    .await?;

    // Publish before the checkout: the row is already committed (this insert
    // runs in autocommit), so a failure below must not cost the notification
    // for a mutation that already happened (ADR-0018).
    ctx.publish(ChangeEvent::repositories(team_id, [id.clone()]));

    let checkout = Checkout {
        repository_id: id.clone(),
        path,
        worktree_root,
        max_concurrency: MIN_REPOSITORY_CONCURRENCY,
        unattended_consent: false,
        on_archive: OnArchive::None,
        on_archive_script: None,
        credential_login: None,
        credential_label: None,
        credential_added_at: None,
        created_at,
    };
    machine::local::insert_checkout(machine, &checkout).await?;

    get(ctx, &id).await
}

/// Whether `path` may be registered twice. It may not — two checkouts naming
/// one directory would give the Settings list two identical entries, and let
/// task 007 create worktrees for "different" repositories against the one git
/// repository.
///
/// Checked against this machine's checkouts, which is where a clone lives
/// since task 066, rather than against the board. One clone mapped to two
/// teams' repositories is task 054's to allow by remote, not this check's.
async fn ensure_not_already_registered(machine: &MachineContext, path: &str) -> Result<()> {
    let taken = machine
        .store
        .list_checkouts()
        .await?
        .iter()
        .any(|checkout| checkout.path == path);

    if taken {
        return Err(Error::invalid(format!("{path} is already registered")));
    }
    Ok(())
}

/// Applies `patch` to an already-registered repository. Unlike [`register`],
/// this does not re-run git validation — a default branch typed by hand is
/// the user overriding what registration found, not a second discovery.
///
/// The read and the write run in one transaction, the same shape
/// `tasks::update_task` uses for its own read-modify-write: two autocommit
/// statements would let a concurrent patch's write land between this read and
/// this write and be silently reverted by it (ADR-0003 names the UI, the MCP
/// server and the scheduler as writers that can all touch a repository "at
/// the same moment").
pub async fn update(ctx: &ServiceContext, id: &str, patch: RepositoryPatch) -> Result<Repository> {
    let mut tx = ctx.begin().await?;
    let scope = tx.scope().json();
    let mut repository = fetch_repository_row(&mut *tx, &scope, id).await?;

    if let Some(name) = patch.name {
        repository.name = require_non_empty(name, "name")?;
    }
    if let Some(default_branch) = patch.default_branch {
        repository.default_branch = require_non_empty(default_branch, "default branch")?;
    }

    let team_id = sqlx::query_scalar!(
        r#"
        UPDATE repositories
        SET name = ?1, default_branch = ?2
        WHERE id = ?3
        RETURNING team_id
        "#,
        repository.name,
        repository.default_branch,
        id,
    )
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    ctx.publish(ChangeEvent::repositories(team_id, [id.to_string()]));
    Ok(repository)
}

/// Flips this runner's consent to unattended runs in one repository (ADR-0012,
/// ADR-0032 point 4).
///
/// The confirmation dialog that states plainly what enabling this permits —
/// "the agent can run any command in this repository's worktree, including
/// network access and package installation, without asking" (ADR-0012's own
/// wording) — is the caller's job. This function is the explicit act itself,
/// called only once the user has agreed to that, never the thing that
/// decides whether to ask.
///
/// Writes the checkout's `unattended_consent` and nothing else. The board's
/// `allow_unattended_runs` is the team ceiling, which task 045 gives its own
/// command; neither this nor any reader before 045 touches it.
pub async fn set_allow_unattended_runs(
    ctx: &ServiceContext,
    machine: &MachineContext,
    id: &str,
    allow: bool,
) -> Result<CheckoutView> {
    let repository = get(ctx, id).await?;
    let patch = CheckoutPatch {
        unattended_consent: Some(allow),
        ..CheckoutPatch::default()
    };
    Ok(machine::local::patch_checkout(machine, &repository, &patch)
        .await?
        .into())
}

/// Moves where this machine creates the repository's worktrees.
///
/// Its own command since task 066, when the root left `update_repository`'s
/// patch: it is this machine's setting, not the board's (D32's appendix).
/// Held to the non-blank rule [`register`] applies to a chosen root.
pub async fn set_worktree_root(
    ctx: &ServiceContext,
    machine: &MachineContext,
    id: &str,
    worktree_root: String,
) -> Result<CheckoutView> {
    let worktree_root = require_non_empty(worktree_root, "worktree root")?;
    let repository = get(ctx, id).await?;
    let patch = CheckoutPatch {
        worktree_root: Some(worktree_root),
        ..CheckoutPatch::default()
    };
    Ok(machine::local::patch_checkout(machine, &repository, &patch)
        .await?
        .into())
}

/// The smallest per-repository cap that means anything: a repository that will
/// hold no runs at all is spelled by taking ADR-0012's opt-in away, not by
/// setting this to zero — a second way to say "never run here" is a second
/// thing the Settings panel has to explain and a second thing selection has to
/// agree with.
pub const MIN_REPOSITORY_CONCURRENCY: i64 = 1;

/// Raises or lowers ADR-0010's per-repository cap.
///
/// The reason this is opt-out rather than a share of the global limit is worth
/// having at the call site, because the Settings control has to say it too:
/// "two agents in two worktrees of the same repo is safe for git, but they will
/// fight over ports, test databases, and lockfiles. Parallelism across
/// *repositories* is the safe default; within one repo it is opt-in."
///
/// Strict where the read is tolerant, which is this codebase's settled
/// asymmetry (`mcp::settings::set_configured_port` states it): a value out of
/// range from a form or a tool is refused with a sentence the panel renders,
/// while a value somebody typed into the `sqlite3` CLI is warned about and
/// clamped by [`scheduler::capacity`] rather than stopping a night's queue.
///
/// [`scheduler::capacity`]: crate::scheduler::capacity
///
/// A per-runner cap since task 066 (ADR-0031 point 6), so it is written to this
/// machine's checkout.
pub async fn set_max_concurrency(
    ctx: &ServiceContext,
    machine: &MachineContext,
    id: &str,
    max_concurrency: i64,
) -> Result<CheckoutView> {
    let max_concurrency = require_usable_concurrency(max_concurrency)?;
    let repository = get(ctx, id).await?;
    let patch = CheckoutPatch {
        max_concurrency: Some(max_concurrency),
        ..CheckoutPatch::default()
    };
    Ok(machine::local::patch_checkout(machine, &repository, &patch)
        .await?
        .into())
}

/// Holds a per-repository cap to the range that has a meaning, naming both
/// bounds and why each is there.
fn require_usable_concurrency(max_concurrency: i64) -> Result<i64> {
    let ceiling = CONCURRENCY_CEILING as i64;
    if !(MIN_REPOSITORY_CONCURRENCY..=ceiling).contains(&max_concurrency) {
        return Err(Error::invalid(format!(
            "a repository may hold between {MIN_REPOSITORY_CONCURRENCY} and {ceiling} runs at \
             once, not {max_concurrency}. To stop this repository running at all, turn off \
             unattended agent runs instead."
        )));
    }
    Ok(max_concurrency)
}

/// Whether this runner consented to unattended runs in `checkout`'s repository
/// (ADR-0012, ADR-0032 point 4). A plain read of the flag, named so a call
/// site reads as a question rather than a field access —
/// [`ensure_unattended_runs_allowed`] is the version that turns "no" into the
/// `Error` a caller can propagate directly.
pub fn allows_unattended_runs(checkout: &Checkout) -> bool {
    checkout.unattended_consent
}

/// Refuses to let a task start unless this runner consented to unattended runs
/// in its repository, and answers the checkout a run then works in. The
/// starter (task 008) and the runner both call this rather than re-deriving
/// the rule, so a repository is runnable — or not — the same way from
/// whichever path asks (ADR-0006), and the run button's explanation is this
/// message rather than a second one invented at the call site.
///
/// The runner's half only (D31's table). A repository with no checkout here
/// is refused as [`machine::not_set_up`]; the team ceiling's check on the claim
/// is task 045's.
pub async fn ensure_unattended_runs_allowed(
    machine: &MachineContext,
    repository: &Repository,
) -> Result<Checkout> {
    let checkout = machine::checkout_of(machine, repository).await?;
    if checkout.unattended_consent {
        Ok(checkout)
    } else {
        Err(Error::invalid(format!(
            "\"{}\" has not enabled unattended agent runs. Enable it in Settings → Repositories before starting tasks here.",
            repository.name
        )))
    }
}

/// Removes a repository. Refused, naming how many, when any task still
/// references it — the schema's `ON DELETE RESTRICT` is the backstop for a
/// writer that is not this function (the MCP server, or the user with the
/// `sqlite3` CLI); this is the message the user actually reads. The refusal
/// comes before any write in either store.
///
/// Given a machine, it then forgets every worktree record of the repository
/// and removes the checkout (task 066). Each such record belongs to a task the
/// board already deleted, because the refusal above passed; `delete_task`
/// removes no worktree, so they exist on real installs, and the checkout's
/// `RESTRICT` would otherwise refuse its removal and orphan it. The
/// directories themselves stay on disk, as a deleted task's always have. The
/// shell passes `Some`; a server, which has no machine, `None`.
///
/// It also deletes the repository's strategy default, which lives in
/// `team_settings` under a key rather than in a column (seam-contract D17.1).
/// A settings key is not a foreign key and nothing cascades, so this is the
/// only thing standing between removing a repository and leaving configuration
/// behind that no screen will ever show again. In the same transaction as the
/// repository's delete, so a refused removal has not thrown that configuration
/// away on its way to the refusal, and the removal announces only itself.
pub async fn remove(
    ctx: &ServiceContext,
    machine: Option<&MachineContext>,
    id: &str,
) -> Result<()> {
    let mut tx = ctx.begin().await?;

    // Looked up first, in the scope, so another team's repository is refused
    // as a missing one rather than with a count of tasks it has no business
    // hearing about.
    let team_id = team_of_repository(&mut tx, id).await?;

    let task_count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!: i64" FROM tasks WHERE repository_id = ?1"#,
        id,
    )
    .fetch_one(&mut *tx)
    .await?;

    if task_count > 0 {
        // Written as two whole clauses rather than one format string with a
        // pluralized noun, because English also inflects the verb: "1 task
        // still references it" against "2 tasks still reference it" is not a
        // suffix away from itself.
        let reason = if task_count == 1 {
            "1 task still references it".to_string()
        } else {
            format!("{task_count} tasks still reference it")
        };
        return Err(Error::invalid(format!(
            "cannot remove this repository: {reason}"
        )));
    }

    sqlx::query!("DELETE FROM repositories WHERE id = ?1", id)
        .execute(&mut *tx)
        .await?;

    // Through the one statement that writes `team_settings`, so the checks
    // tasks 045 and 051 hang there cover a removal too. The key is spelled by
    // the module that owns it, not with a `format!` here: two spellings of
    // `strategy_default.<id>` would leak a row per removed repository and
    // nothing would ever notice (seam-contract D3, D17.1).
    crate::db::settings::set_team_in(
        ctx,
        &mut tx,
        &team_id,
        &crate::strategy::settings::repository_default_key(id),
        None,
    )
    .await?;

    tx.commit().await?;
    ctx.publish(ChangeEvent::repositories(team_id, [id.to_string()]));

    if let Some(machine) = machine {
        for record in machine.store.list_worktrees().await? {
            if record.repository_id == id {
                machine::local::forget_worktree(machine, &record.task_id).await?;
            }
        }
        machine::local::remove_checkout(machine, id).await?;
    }
    Ok(())
}

/// Fresh inspection of a checkout's remote and PR readiness. Never fails on
/// a missing or unauthenticated `gh` — see [`RemoteInfo::gh_ready`] — so the
/// only propagated error is `git` itself being unrunnable.
pub async fn remote_info(checkout: &Checkout) -> Result<RemoteInfo> {
    let path = Path::new(&checkout.path);
    let remote_url = git::remote_url(path).await?;

    let gh_ready = match remote_url.as_deref().and_then(git::host_from_remote_url) {
        Some(host) => Some(git::gh_authenticated(&host).await),
        None => None,
    };

    Ok(RemoteInfo {
        remote_url,
        gh_ready,
    })
}

/// Whether a repository is ready to have a pull request opened against its
/// remote, with the two unready cases told apart.
///
/// [`RemoteInfo::gh_ready`] collapses them into `Some(false)`, which is all
/// task 003's row warning needs. Task 018's doctor needs them separate: the
/// remediation for one is "install the GitHub CLI" and for the other "run
/// `gh auth login`", and offering the wrong one is worse than offering none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GhStatus {
    /// No `origin`, or an `origin` with no host — a local path remote. There
    /// is no pull request to open, so `gh` is not a question here.
    NoRemote,
    NotInstalled,
    NotAuthenticated,
    Ready,
}

/// [`remote_info`]'s finer sibling, for the doctor.
///
/// Same subprocesses, same never-fails-on-`gh` contract; it only declines to
/// throw away which of the two failures happened. `program` is injected for
/// the reason [`git::gh_probe`] gives — a test cannot depend on whether the
/// machine running it happens to be logged in to GitHub.
pub async fn gh_status(checkout: &Checkout, program: &Path) -> Result<GhStatus> {
    let remote_url = git::remote_url(Path::new(&checkout.path)).await?;
    let Some(host) = remote_url.as_deref().and_then(git::host_from_remote_url) else {
        return Ok(GhStatus::NoRemote);
    };

    Ok(match git::gh_probe(program, &host).await {
        git::GhProbe::NotInstalled => GhStatus::NotInstalled,
        git::GhProbe::NotAuthenticated => GhStatus::NotAuthenticated,
        git::GhProbe::Ready => GhStatus::Ready,
    })
}

/// Why a checkout's stored path is no longer usable, or `None` when it still
/// is.
///
/// The doctor's version of the checks [`validate_directory`] and
/// [`register`] run at registration time, asked again later: a path that was
/// valid when it was registered is not a path that is valid tonight. Renaming
/// a project directory is an ordinary thing to do and nothing tells Rimaia.
pub async fn path_problem(checkout: &Checkout) -> Result<Option<String>> {
    let path = Path::new(&checkout.path);

    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return Ok(Some("the directory no longer exists".to_string()));
    };
    if !metadata.is_dir() {
        return Ok(Some("the path is no longer a directory".to_string()));
    }
    if git::git_dirs(path).await?.is_none() {
        return Ok(Some(
            "the directory is no longer a git repository".to_string(),
        ));
    }
    Ok(None)
}

/// Checks the first of task 003's four validations and resolves the path to
/// its canonical form, which is what every later check (and the stored row)
/// uses — the same resolution `rimaia_core::testing::TempRepo` performs on
/// its own root, so a test comparing paths never trips over a macOS
/// `/var` → `/private/var` symlink.
async fn validate_directory(path: &Path) -> Result<PathBuf> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| Error::invalid(format!("{} does not exist", path.display())))?;

    if !metadata.is_dir() {
        return Err(Error::invalid(format!(
            "{} is not a directory",
            path.display()
        )));
    }

    // Through `git_safe`, because this path is *stored* and every later use of
    // it is a `git` invocation in that directory — a Windows extended-length
    // path would be written into the row once and fail every clone and every
    // worktree from then on.
    tokio::fs::canonicalize(path)
        .await
        .map(crate::paths::git_safe)
        .map_err(|error| {
            Error::invalid(format!("{} could not be resolved: {error}", path.display()))
        })
}

/// The second of task 003's four validations: a git repository, and not a
/// linked worktree of one.
async fn validate_is_a_registrable_git_repository(path: &Path) -> Result<()> {
    match git::git_dirs(path).await? {
        None => Err(Error::invalid(format!(
            "{} is not a git repository",
            path.display()
        ))),
        Some((git_dir, common_dir)) if git_dir != common_dir => Err(Error::invalid(format!(
            "{} is a worktree of another repository; register that repository instead",
            path.display()
        ))),
        Some(_) => Ok(()),
    }
}

/// The third of task 003's four validations.
async fn validate_has_at_least_one_commit(path: &Path) -> Result<()> {
    if git::has_at_least_one_commit(path).await? {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{} has no commits yet",
            path.display()
        )))
    }
}

/// The fourth of task 003's four validations.
async fn resolve_default_branch(path: &Path) -> Result<String> {
    git::default_branch(path).await?.ok_or_else(|| {
        Error::invalid(format!(
            "{} has no origin/HEAD, main, or master branch, and HEAD is detached — \
             a default branch cannot be determined",
            path.display()
        ))
    })
}

/// Where a repository named `name` keeps its worktrees when nobody chose:
/// `<worktrees_dir>/<slug>` (ADR-0005).
///
/// [`register`]'s default, and the one task 041's adoption gives a checkout
/// whose board row has no root, so the two cannot derive different
/// directories for one repository.
pub fn default_worktree_root(worktrees_dir: &Path, name: &str) -> Result<String> {
    path_to_string(&worktrees_dir.join(naming::slugify(name)))
}

fn path_to_string(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::invalid(format!("{} is not valid UTF-8", path.display())))
}

fn require_non_empty(value: String, field: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(Error::invalid(format!("{field} must not be empty")))
    } else {
        Ok(trimmed.to_string())
    }
}

// ---------------------------------------------------------------------------
// Per-repository forge credentials (task 022, ADR-0020)
// ---------------------------------------------------------------------------

/// What a settings pane may know about a repository's credential.
///
/// **Never the secret.** The login, the label and the date are metadata; the
/// token is in the keychain and there is no read path from here to it. That is
/// also why this is the only shape the MCP surface gets: it is an operator-only
/// read, and it carries nothing that would be worth stealing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    /// `true` once a credential has been configured for this repository — which
    /// is `credential_login` being set, or the save having been marked
    /// unverified.
    pub configured: bool,
    /// The login the token resolved to, or `None` for a save `gh` could not
    /// verify.
    pub login: Option<String>,
    pub label: Option<String>,
    pub added_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether the machine's keychain actually holds the item this row claims.
    ///
    /// The two can disagree — a keychain restored from a different machine, an
    /// item deleted in Keychain Access — and that disagreement is exactly what
    /// makes a run refuse rather than fall back, so the pane has to be able to
    /// show it before 2am rather than after.
    pub store: crate::credentials::StoreStatus,
    /// Whether `origin` is an SSH remote. ADR-0020 point 6: silence here would
    /// let a user believe Rimaia controls an access path it does not — the
    /// credential covers `gh` API calls and any HTTPS remote, and an SSH push
    /// uses the machine's own key regardless.
    pub ssh_remote: bool,
}

/// Records that a repository now carries a credential on this machine.
///
/// **The secret is not this function's business.** The caller stores it in the
/// keychain first and calls this with what the forge said — which is what keeps
/// the token out of every path that touches SQLite, including this one's own
/// error messages.
///
/// `credential_login` is `None` for a save `gh` could not verify, and the
/// checkout still carries a configured credential: `credential_added_at` is
/// what says so, and it is what the spawn path reads. Written to the checkout,
/// because the keychain item it describes is on this machine (ADR-0033 point
/// 6, D25); the keychain stays keyed by repository id until task 054.
pub async fn set_credential_metadata(
    ctx: &ServiceContext,
    machine: &MachineContext,
    id: &str,
    login: Option<&str>,
    label: Option<&str>,
) -> Result<Checkout> {
    let repository = get(ctx, id).await?;
    let patch = CheckoutPatch {
        credential_login: set_or_clear(login),
        credential_label: set_or_clear(label),
        credential_added_at: Patch::Set(machine.clock.now()),
        ..CheckoutPatch::default()
    };
    machine::local::patch_checkout(machine, &repository, &patch).await
}

/// Clears the metadata. The caller deletes the keychain item.
///
/// All three fields together, because a checkout with a label and no
/// `added_at` would read as configured to [`has_credential`] and as nothing to
/// the pane.
pub async fn clear_credential_metadata(
    ctx: &ServiceContext,
    machine: &MachineContext,
    id: &str,
) -> Result<Checkout> {
    let repository = get(ctx, id).await?;
    let patch = CheckoutPatch {
        credential_login: Patch::Clear,
        credential_label: Patch::Clear,
        credential_added_at: Patch::Clear,
        ..CheckoutPatch::default()
    };
    machine::local::patch_checkout(machine, &repository, &patch).await
}

/// A credential field written whole: the value when there is one, cleared
/// when there is not, so a re-save never keeps the previous token's label.
fn set_or_clear(value: Option<&str>) -> Patch<String> {
    match value {
        Some(value) => Patch::Set(value.to_string()),
        None => Patch::Clear,
    }
}

/// Whether this repository's runs must spawn with a token of their own.
///
/// **Read off the checkout, never off the keychain.** A keychain that cannot
/// be reached has to be a *refusal* — ADR-0020's fail-closed rule — and a
/// spawn path that asked the keychain "is anything there" would read a locked
/// one as "no credential configured" and fall straight back to the operator's
/// ambient login, which is the exact failure the rule exists to prevent.
pub fn has_credential(checkout: &Checkout) -> bool {
    checkout.credential_added_at.is_some()
}
