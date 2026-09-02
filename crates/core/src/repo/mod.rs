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

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::context::ServiceContext;
use crate::db::Repository;
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::scheduler::CONCURRENCY_CEILING;

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

/// An edit to an already-registered repository.
///
/// A patch, not a replacement: every field left `None` is left unchanged, the
/// same shape task 004's task edits use and for the same reason — an "edit
/// default branch" form has no business also overwriting the name.
#[derive(Debug, Clone, Default)]
pub struct RepositoryPatch {
    pub name: Option<String>,
    pub default_branch: Option<String>,
    pub worktree_root: Option<String>,
    /// ADR-0010's per-repository opt-out, in the number of runs this repository
    /// will hold at once. Held to [`MIN_REPOSITORY_CONCURRENCY`] and
    /// [`CONCURRENCY_CEILING`] — see [`set_max_concurrency`], which is the door
    /// the Settings control and the MCP tool both use.
    pub max_concurrency: Option<i64>,
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

/// Every registered repository, alphabetically — the order the Settings list
/// shows them in.
pub async fn list(ctx: &ServiceContext) -> Result<Vec<Repository>> {
    let repositories = sqlx::query_as!(
        Repository,
        r#"
        SELECT id, name, path, default_branch, worktree_root, allow_unattended_runs,
               max_concurrency, created_at AS "created_at: chrono::DateTime<chrono::Utc>"
        FROM repositories
        ORDER BY name ASC, created_at ASC
        "#
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(repositories)
}

/// One repository by id. `Error::not_found` when there is none — task 003's
/// removal and edit paths both start here, so both get that message for free
/// rather than reimplementing "does this id exist".
pub async fn get(ctx: &ServiceContext, id: &str) -> Result<Repository> {
    fetch_repository_row(&ctx.pool, id).await
}

/// The one place a repository row is read back — used both inside a
/// transaction (`&mut *tx`, before a write that depends on the current row,
/// the way [`update`] does) and against the bare pool (a plain [`get`]).
/// Generic over [`sqlx::Executor`] for the same reason
/// `tasks::service::fetch_task_row` is: nothing here requires the caller's
/// transaction, so the flexibility is free.
async fn fetch_repository_row<'e, E>(executor: E, id: &str) -> Result<Repository>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query_as!(
        Repository,
        r#"
        SELECT id, name, path, default_branch, worktree_root, allow_unattended_runs,
               max_concurrency, created_at AS "created_at: chrono::DateTime<chrono::Utc>"
        FROM repositories
        WHERE id = ?1
        "#,
        id,
    )
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| Error::not_found(format!("no repository with id {id}")))
}

/// Validates and registers a local repository.
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
/// Two more checks the schema comment on `repositories.path` delegated to
/// this task: a directory already registered under another row is refused
/// rather than silently duplicated (checked first, right after
/// canonicalization — it needs no git introspection, so there is no reason
/// to pay for that before ruling out the cheaper problem), and a
/// caller-supplied `name` or `worktree_root` is held to the same non-blank
/// rule [`update`] already enforces — a blank value must not be storable
/// through one door and refused through the other (ADR-0006).
pub async fn register(
    ctx: &ServiceContext,
    worktrees_dir: &Path,
    new: NewRepository,
) -> Result<Repository> {
    let requested = Path::new(&new.path);
    let canonical = validate_directory(requested).await?;
    let path = path_to_string(&canonical)?;
    ensure_not_already_registered(ctx, &path).await?;
    validate_is_a_registrable_git_repository(&canonical).await?;
    validate_has_at_least_one_commit(&canonical).await?;
    let default_branch = resolve_default_branch(&canonical).await?;

    let name = match new.name {
        Some(name) => require_non_empty(name, "name")?,
        None => naming::derive_name(&canonical),
    };
    let worktree_root = match new.worktree_root {
        Some(root) => require_non_empty(root, "worktree root")?,
        None => path_to_string(&worktrees_dir.join(naming::slugify(&name)))?,
    };

    let id = crate::db::new_id();
    let created_at = ctx.clock.now();

    sqlx::query!(
        r#"
        INSERT INTO repositories
            (id, name, path, default_branch, worktree_root, allow_unattended_runs, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)
        "#,
        id,
        name,
        path,
        default_branch,
        worktree_root,
        created_at,
    )
    .execute(&ctx.pool)
    .await?;

    // Publish before the read-back: the row is already committed (this
    // insert runs in autocommit), so a failure in `get` below must not cost
    // the notification for a mutation that already happened (ADR-0018).
    ctx.publish(ChangeEvent::repositories([id.clone()]));
    let repository = get(ctx, &id).await?;
    Ok(repository)
}

/// The migration's own delegated question, answered: whether `path` may be
/// registered twice. It may not — two rows naming one directory would give
/// the Settings list two identical entries, and let task 007 create
/// worktrees for "different" repositories against the one git repository.
async fn ensure_not_already_registered(ctx: &ServiceContext, path: &str) -> Result<()> {
    let count: i64 = sqlx::query_scalar!("SELECT count(*) FROM repositories WHERE path = ?1", path)
        .fetch_one(&ctx.pool)
        .await?;

    if count > 0 {
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
    let mut tx = ctx.pool.begin().await?;
    let mut repository = fetch_repository_row(&mut *tx, id).await?;

    if let Some(name) = patch.name {
        repository.name = require_non_empty(name, "name")?;
    }
    if let Some(default_branch) = patch.default_branch {
        repository.default_branch = require_non_empty(default_branch, "default branch")?;
    }
    if let Some(worktree_root) = patch.worktree_root {
        repository.worktree_root = require_non_empty(worktree_root, "worktree root")?;
    }
    if let Some(max_concurrency) = patch.max_concurrency {
        repository.max_concurrency = require_usable_concurrency(max_concurrency)?;
    }

    sqlx::query!(
        r#"
        UPDATE repositories
        SET name = ?1, default_branch = ?2, worktree_root = ?3, max_concurrency = ?4
        WHERE id = ?5
        "#,
        repository.name,
        repository.default_branch,
        repository.worktree_root,
        repository.max_concurrency,
        id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    ctx.publish(ChangeEvent::repositories([id.to_string()]));
    Ok(repository)
}

/// Flips ADR-0012's per-repository opt-in to unattended runs.
///
/// The confirmation dialog that states plainly what enabling this permits —
/// "the agent can run any command in this repository's worktree, including
/// network access and package installation, without asking" (ADR-0012's own
/// wording) — is the caller's job. This function is the explicit act itself,
/// called only once the user has agreed to that, never the thing that
/// decides whether to ask.
pub async fn set_allow_unattended_runs(
    ctx: &ServiceContext,
    id: &str,
    allow: bool,
) -> Result<Repository> {
    let mut repository = get(ctx, id).await?;

    sqlx::query!(
        "UPDATE repositories SET allow_unattended_runs = ?1 WHERE id = ?2",
        allow,
        id,
    )
    .execute(&ctx.pool)
    .await?;

    repository.allow_unattended_runs = allow;
    ctx.publish(ChangeEvent::repositories([id.to_string()]));
    Ok(repository)
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
pub async fn set_max_concurrency(
    ctx: &ServiceContext,
    id: &str,
    max_concurrency: i64,
) -> Result<Repository> {
    update(
        ctx,
        id,
        RepositoryPatch {
            max_concurrency: Some(max_concurrency),
            ..RepositoryPatch::default()
        },
    )
    .await
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

/// Whether `repository` may be used to start a task unattended (ADR-0012).
/// A plain read of the flag, named so a call site reads as a question rather
/// than a field access — [`ensure_unattended_runs_allowed`] is the version
/// that turns "no" into the `Error` a caller can propagate directly.
pub fn allows_unattended_runs(repository: &Repository) -> bool {
    repository.allow_unattended_runs
}

/// Refuses to let a task start unless its repository has opted in to
/// unattended runs. The runner (task 008) and the scheduler (task 009) both
/// call this rather than re-deriving the rule, so a repository is runnable —
/// or not — the same way from whichever path asks (ADR-0006), and the run
/// button's explanation is this message rather than a second one invented at
/// the call site.
pub fn ensure_unattended_runs_allowed(repository: &Repository) -> Result<()> {
    if repository.allow_unattended_runs {
        Ok(())
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
/// `sqlite3` CLI); this is the message the user actually reads.
///
/// It also deletes the repository's strategy default, which lives in `settings`
/// under a key rather than in a column (seam-contract D17.1). A settings key is
/// not a foreign key and nothing cascades, so this is the only thing standing
/// between removing a repository and leaving configuration behind that no
/// screen will ever show again. Inside the transaction, so a removal refused
/// two statements later has not thrown that configuration away on its way to
/// the refusal.
pub async fn remove(ctx: &ServiceContext, id: &str) -> Result<()> {
    let mut tx = ctx.pool.begin().await?;

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

    let deleted = sqlx::query!("DELETE FROM repositories WHERE id = ?1", id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    if deleted == 0 {
        return Err(Error::not_found(format!("no repository with id {id}")));
    }

    // Spelled through the module that owns the key, not with a `format!` here:
    // two spellings of `strategy_default.<id>` would leak a row per removed
    // repository and nothing would ever notice (seam-contract D3, D17.1).
    crate::strategy::settings::delete_repository_default(&mut *tx, id).await?;

    tx.commit().await?;
    ctx.publish(ChangeEvent::repositories([id.to_string()]));
    Ok(())
}

/// Fresh inspection of `repository`'s remote and PR readiness. Never fails on
/// a missing or unauthenticated `gh` — see [`RemoteInfo::gh_ready`] — so the
/// only propagated error is `git` itself being unrunnable.
pub async fn remote_info(repository: &Repository) -> Result<RemoteInfo> {
    let path = Path::new(&repository.path);
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

    tokio::fs::canonicalize(path).await.map_err(|error| {
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
