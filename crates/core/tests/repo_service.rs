//! [`rimaia_core::repo`] against real git repositories (task 003, ADR-0002,
//! ADR-0005, ADR-0012).
//!
//! Every repository here is a real one on disk — built with
//! `rimaia_core::testing::TempRepo` where its shape is enough, and with raw
//! `git` invocations (matching the pattern `crates/core/tests/repo.rs`
//! already uses) for the shapes it does not model: a linked worktree, a
//! branchless repository, a detached `HEAD`, and the `origin/HEAD` symbolic
//! ref a plain `git fetch` never creates on its own. A mocked git would only
//! prove a mock's own assumptions (ADR-0015); the four validations and the
//! branch fallback chain are exactly the git plumbing a mock would have to
//! fake correctly to be worth anything.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use rimaia_core::db::Repository;
use rimaia_core::repo::{self, NewRepository, RepositoryPatch};
use rimaia_core::testing::{TempRepo, TestClock, TestContext};
use rimaia_core::{ChangeEvent, ServiceContext};

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_valid_repository_registers_showing_its_default_branch_and_no_remote() {
    let mut h = TestContext::new().await;
    let source = TempRepo::init();
    let worktrees_dir = scratch_dir("rimaia-worktrees-");

    let repository = register(&h.context, worktrees_dir.path(), source.path())
        .await
        .expect("a valid repository must register");

    assert_eq!(
        repository.name, "work tree",
        "derived from the directory name"
    );
    assert_eq!(repository.path, source.path().to_str().unwrap());
    assert_eq!(repository.default_branch, "main");
    assert_eq!(
        repository.worktree_root,
        worktrees_dir.path().join("work-tree").to_str().unwrap(),
        "the default worktree_root slugifies the derived name"
    );
    assert!(
        !repository.allow_unattended_runs,
        "the opt-in must default to off"
    );

    let remote = repo::remote_info(&repository).await.expect("remote_info");
    assert_eq!(remote.remote_url, None);
    assert_eq!(remote.gh_ready, None);

    assert_eq!(
        h.changes.try_recv().expect("a publication is waiting"),
        ChangeEvent::repositories([repository.id.clone()])
    );
}

#[tokio::test]
async fn registering_reports_the_configured_origin_remote() {
    let h = TestContext::new().await;
    let source = TempRepo::init().with_remote();
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let expected_remote = source
        .remote_path()
        .expect("with_remote sets a remote path")
        .to_str()
        .unwrap()
        .to_string();

    let repository = register(&h.context, worktrees_dir.path(), source.path())
        .await
        .expect("register");

    let remote = repo::remote_info(&repository).await.expect("remote_info");
    assert_eq!(remote.remote_url, Some(expected_remote));
    // TempRepo's origin is a bare local path, not a URL with a host, so there
    // is nothing for `gh auth status --hostname` to check — this is
    // deliberately deterministic and never depends on whether the machine
    // running the test has `gh` installed or authenticated.
    assert_eq!(remote.gh_ready, None);
}

#[tokio::test]
async fn a_repository_whose_path_contains_a_space_round_trips_through_get_and_list() {
    let h = TestContext::new().await;
    // TempRepo's own work tree is named "work tree" for exactly this reason.
    let source = TempRepo::init();
    assert!(source.path().to_string_lossy().contains(' '));
    let worktrees_dir = scratch_dir("rimaia-worktrees-");

    let registered = register(&h.context, worktrees_dir.path(), source.path())
        .await
        .expect("register a path containing a space");

    let fetched = repo::get(&h.context, &registered.id)
        .await
        .expect("get the row back");
    assert_eq!(fetched, registered);

    let listed = repo::list(&h.context).await.expect("list");
    assert_eq!(listed, vec![registered]);
}

#[tokio::test]
async fn a_registered_repository_is_readable_after_a_fresh_pool_reopens_the_same_file() {
    // The one property an in-memory pool cannot prove: task 003's "Registration
    // survives restart" is literally a second process (or here, a second pool)
    // against the same database file, the same way db::mod.rs's own
    // `a_second_launch_applies_no_further_migrations` proves migrations do.
    let db_dir = scratch_dir("rimaia-db-");
    let db_file = db_dir.path().join("rimaia.db");
    let source = TempRepo::init();
    let worktrees_dir = scratch_dir("rimaia-worktrees-");

    let id = {
        let ctx = file_backed_context(&db_file).await;
        let registered = register(&ctx, worktrees_dir.path(), source.path())
            .await
            .expect("register");
        ctx.pool.close().await;
        registered.id
    };

    let reopened = file_backed_context(&db_file).await;
    let fetched = repo::get(&reopened, &id)
        .await
        .expect("the row must still be there after the pool reopens");
    assert_eq!(fetched.id, id);
}

// ---------------------------------------------------------------------------
// Validation — each case its own message
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registering_a_path_that_does_not_exist_names_that_as_the_problem() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let missing = scratch_dir("rimaia-missing-").path().join("does-not-exist");

    let error = register(&h.context, worktrees_dir.path(), &missing)
        .await
        .expect_err("a nonexistent path must be refused");

    assert_eq!(
        error.to_string(),
        format!("{} does not exist", missing.display())
    );
}

#[tokio::test]
async fn registering_a_file_instead_of_a_directory_names_that_as_the_problem() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let dir = scratch_dir("rimaia-file-");
    let file = dir.path().join("not-a-directory");
    std::fs::write(&file, b"just a file").expect("write a plain file");

    let error = register(&h.context, worktrees_dir.path(), &file)
        .await
        .expect_err("a file must be refused");

    assert_eq!(
        error.to_string(),
        format!("{} is not a directory", file.display())
    );
}

#[tokio::test]
async fn registering_a_directory_that_is_not_a_git_repository_names_that_as_the_problem() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let plain = scratch_dir("rimaia-plain-dir-");
    let canonical = canonicalize(plain.path());

    let error = register(&h.context, worktrees_dir.path(), plain.path())
        .await
        .expect_err("a plain directory must be refused");

    assert_eq!(
        error.to_string(),
        format!("{} is not a git repository", canonical.display())
    );
}

#[tokio::test]
async fn registering_a_worktree_of_another_repository_names_that_as_the_problem() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let main = TempRepo::init();
    let elsewhere = scratch_dir("rimaia-linked-worktree-");
    // A destination containing a space too, matching TempRepo's own habit of
    // never giving argument-vector bugs an easy pass.
    let linked = elsewhere.path().join("task 7");
    git(
        main.path(),
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new("feature"),
            linked.as_os_str(),
        ],
    );
    let canonical = canonicalize(&linked);

    let error = register(&h.context, worktrees_dir.path(), &linked)
        .await
        .expect_err("a linked worktree must be refused");

    assert_eq!(
        error.to_string(),
        format!(
            "{} is a worktree of another repository; register that repository instead",
            canonical.display()
        )
    );
}

#[tokio::test]
async fn registering_a_repository_with_no_commits_names_that_as_the_problem() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let empty = init_repo_on_branch("main");
    let canonical = canonicalize(empty.path());

    let error = register(&h.context, worktrees_dir.path(), empty.path())
        .await
        .expect_err("a repository with no commits must be refused");

    assert_eq!(
        error.to_string(),
        format!("{} has no commits yet", canonical.display())
    );
}

#[tokio::test]
async fn registering_a_repository_whose_default_branch_cannot_be_determined_names_that_as_the_problem(
) {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    // Neither "main" nor "master", no origin, and detached — nothing in the
    // fallback chain applies.
    let repo = init_repo_on_branch("trunk");
    commit(repo.path(), "f.txt", "hello");
    git(repo.path(), &["checkout", "--detach", "HEAD"]);
    let canonical = canonicalize(repo.path());

    let error = register(&h.context, worktrees_dir.path(), repo.path())
        .await
        .expect_err("an undeterminable default branch must be refused");

    assert_eq!(
        error.to_string(),
        format!(
            "{} has no origin/HEAD, main, or master branch, and HEAD is detached — \
             a default branch cannot be determined",
            canonical.display()
        )
    );
}

// ---------------------------------------------------------------------------
// Duplicate registration and blank fields — delegated by the migration's own
// comment on `repositories.path` to this task
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registering_the_same_directory_twice_is_refused() {
    let h = TestContext::new().await;
    let source = TempRepo::init();
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    register(&h.context, worktrees_dir.path(), source.path())
        .await
        .expect("the first registration must succeed");

    let error = register(&h.context, worktrees_dir.path(), source.path())
        .await
        .expect_err("registering the same directory twice must be refused");

    assert_eq!(
        error.to_string(),
        format!("{} is already registered", source.path().to_str().unwrap())
    );

    let rows = repo::list(&h.context).await.expect("list");
    assert_eq!(rows.len(), 1, "the duplicate must not have been inserted");
}

#[tokio::test]
async fn registering_with_a_blank_name_is_refused() {
    let h = TestContext::new().await;
    let source = TempRepo::init();
    let worktrees_dir = scratch_dir("rimaia-worktrees-");

    let error = repo::register(
        &h.context,
        worktrees_dir.path(),
        NewRepository {
            path: source.path().to_str().unwrap().to_string(),
            name: Some("   ".to_string()),
            worktree_root: None,
        },
    )
    .await
    .expect_err("a blank name must be refused");

    assert_eq!(error.to_string(), "name must not be empty");
}

// ---------------------------------------------------------------------------
// Default branch: the full fallback chain
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_branch_prefers_origin_head_over_main_and_master() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let repo = init_repo_on_branch("trunk");
    commit(repo.path(), "f.txt", "hello");
    // Both conventional names exist too, so a fallback checked before
    // origin/HEAD would pick one of these instead and this test would catch it.
    git(repo.path(), &["branch", "main"]);
    git(repo.path(), &["branch", "master"]);
    point_origin_head_at(repo.path(), "trunk");

    let repository = register(&h.context, worktrees_dir.path(), repo.path())
        .await
        .expect("register");

    assert_eq!(repository.default_branch, "trunk");
}

#[tokio::test]
async fn default_branch_falls_back_to_main_when_there_is_no_origin_head() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let repo = init_repo_on_branch("trunk");
    commit(repo.path(), "f.txt", "hello");
    git(repo.path(), &["branch", "main"]);
    git(repo.path(), &["branch", "master"]);

    let repository = register(&h.context, worktrees_dir.path(), repo.path())
        .await
        .expect("register");

    assert_eq!(repository.default_branch, "main");
}

#[tokio::test]
async fn default_branch_falls_back_to_master_when_there_is_no_main() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let repo = init_repo_on_branch("trunk");
    commit(repo.path(), "f.txt", "hello");
    git(repo.path(), &["branch", "master"]);

    let repository = register(&h.context, worktrees_dir.path(), repo.path())
        .await
        .expect("register");

    assert_eq!(repository.default_branch, "master");
}

#[tokio::test]
async fn default_branch_falls_back_to_the_current_branch_when_nothing_else_applies() {
    let h = TestContext::new().await;
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    let repo = init_repo_on_branch("trunk");
    commit(repo.path(), "f.txt", "hello");

    let repository = register(&h.context, worktrees_dir.path(), repo.path())
        .await
        .expect("register");

    assert_eq!(repository.default_branch, "trunk");
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn updating_name_default_branch_and_worktree_root_is_readable_afterward() {
    let mut h = TestContext::new().await;
    let repository = register_temp_repo(&h.context).await;
    h.changes.try_recv().expect("the registration publication");

    let updated = repo::update(
        &h.context,
        &repository.id,
        RepositoryPatch {
            name: Some("Renamed".to_string()),
            default_branch: Some("develop".to_string()),
            worktree_root: Some("/tmp/somewhere-else".to_string()),
        },
    )
    .await
    .expect("update");

    assert_eq!(updated.name, "Renamed");
    assert_eq!(updated.default_branch, "develop");
    assert_eq!(updated.worktree_root, "/tmp/somewhere-else");
    assert_eq!(
        repo::get(&h.context, &repository.id).await.unwrap(),
        updated
    );
    assert_eq!(
        h.changes.try_recv().expect("the update publication"),
        ChangeEvent::repositories([repository.id])
    );
}

#[tokio::test]
async fn updating_a_repository_with_a_blank_name_is_refused() {
    let h = TestContext::new().await;
    let repository = register_temp_repo(&h.context).await;

    let error = repo::update(
        &h.context,
        &repository.id,
        RepositoryPatch {
            name: Some("   ".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect_err("a blank name must be refused");

    assert_eq!(error.to_string(), "name must not be empty");
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removing_a_repository_with_no_tasks_succeeds() {
    let mut h = TestContext::new().await;
    let repository = register_temp_repo(&h.context).await;
    h.changes.try_recv().expect("the registration publication");

    repo::remove(&h.context, &repository.id)
        .await
        .expect("removal with no referencing tasks must succeed");

    let error = repo::get(&h.context, &repository.id)
        .await
        .expect_err("the row must be gone");
    assert_eq!(
        error.to_string(),
        format!("no repository with id {}", repository.id)
    );
    assert_eq!(
        h.changes.try_recv().expect("the removal publication"),
        ChangeEvent::repositories([repository.id])
    );
}

#[tokio::test]
async fn removing_a_repository_with_tasks_is_refused_and_names_the_count() {
    let h = TestContext::new().await;
    let repository = register_temp_repo(&h.context).await;
    insert_task(&h.context.pool, &repository.id).await;
    insert_task(&h.context.pool, &repository.id).await;

    let error = repo::remove(&h.context, &repository.id)
        .await
        .expect_err("removal must be refused while tasks reference it");

    assert_eq!(
        error.to_string(),
        "cannot remove this repository: 2 tasks still reference it"
    );
    repo::get(&h.context, &repository.id)
        .await
        .expect("a refused removal must leave the row in place");
}

#[tokio::test]
async fn removing_a_repository_with_exactly_one_task_uses_the_singular_noun() {
    let h = TestContext::new().await;
    let repository = register_temp_repo(&h.context).await;
    insert_task(&h.context.pool, &repository.id).await;

    let error = repo::remove(&h.context, &repository.id)
        .await
        .expect_err("removal must be refused");

    assert_eq!(
        error.to_string(),
        "cannot remove this repository: 1 task still references it"
    );
}

// ---------------------------------------------------------------------------
// The unattended-runs opt-in (ADR-0012)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_newly_registered_repository_defaults_to_no_unattended_runs() {
    let h = TestContext::new().await;
    let repository = register_temp_repo(&h.context).await;

    assert!(!repo::allows_unattended_runs(&repository));
    assert!(repo::ensure_unattended_runs_allowed(&repository).is_err());
}

#[tokio::test]
async fn set_allow_unattended_runs_flips_the_flag_and_publishes() {
    let mut h = TestContext::new().await;
    let repository = register_temp_repo(&h.context).await;
    h.changes.try_recv().expect("the registration publication");

    let enabled = repo::set_allow_unattended_runs(&h.context, &repository.id, true)
        .await
        .expect("enable the opt-in");
    assert!(enabled.allow_unattended_runs);
    assert!(repo::allows_unattended_runs(&enabled));
    assert!(repo::ensure_unattended_runs_allowed(&enabled).is_ok());
    assert_eq!(
        h.changes.try_recv().expect("the opt-in publication"),
        ChangeEvent::repositories([repository.id.clone()])
    );

    let disabled = repo::set_allow_unattended_runs(&h.context, &repository.id, false)
        .await
        .expect("disable the opt-in");
    assert!(!disabled.allow_unattended_runs);
}

#[tokio::test]
async fn ensure_unattended_runs_allowed_names_the_repository_when_refusing() {
    let h = TestContext::new().await;
    let mut repository = register_temp_repo(&h.context).await;
    repository.name = "Point of Sale".to_string();

    let error = repo::ensure_unattended_runs_allowed(&repository).expect_err("must refuse");

    assert_eq!(
        error.to_string(),
        "\"Point of Sale\" has not enabled unattended agent runs. \
         Enable it in Settings → Repositories before starting tasks here."
    );
}

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

/// Registers `path` under `worktrees_dir` with no name or worktree_root
/// override — the shape most tests want.
async fn register(
    ctx: &ServiceContext,
    worktrees_dir: &Path,
    path: &Path,
) -> rimaia_core::Result<Repository> {
    repo::register(
        ctx,
        worktrees_dir,
        NewRepository {
            path: path.to_str().expect("test paths are UTF-8").to_string(),
            name: None,
            worktree_root: None,
        },
    )
    .await
}

/// A fresh [`TempRepo`] registered with a throwaway `worktrees_dir` — for the
/// many tests here whose subject is not registration itself.
async fn register_temp_repo(ctx: &ServiceContext) -> Repository {
    let source = TempRepo::init();
    let worktrees_dir = scratch_dir("rimaia-worktrees-");
    register(ctx, worktrees_dir.path(), source.path())
        .await
        .expect("register a fresh TempRepo")
}

/// A context backed by a real file on disk, for the one test that needs a
/// second pool to prove persistence rather than an in-memory database that
/// cannot outlive the first.
async fn file_backed_context(db_file: &Path) -> ServiceContext {
    let pool = rimaia_core::db::connect(db_file)
        .await
        .expect("connect to the file-backed database");
    rimaia_core::db::migrate(&pool)
        .await
        .expect("migrate the file-backed database");
    ServiceContext::new(
        pool,
        Arc::new(TestClock::new(rimaia_core::testing::test_epoch())),
        rimaia_core::db::MutationSource::Ui,
    )
}

/// A minimal `tasks` row referencing `repository_id` — enough to exercise the
/// `ON DELETE RESTRICT` removal refuses against, nothing more.
async fn insert_task(pool: &sqlx::SqlitePool, repository_id: &str) {
    let id = rimaia_core::db::new_id();
    const NOW: &str = "2026-08-20T12:00:00+00:00";
    sqlx::query!(
        r#"
        INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at)
        VALUES (?1, ?2, 'a task', 'ready', 1.0, 'idle', ?3, ?3)
        "#,
        id,
        repository_id,
        NOW,
    )
    .execute(pool)
    .await
    .expect("insert a task fixture");
}

/// A temporary directory outside any repository — for worktree destinations
/// and app-data worktree roots alike, matching `crates/core/tests/repo.rs`'s
/// own `scratch_dir`.
fn scratch_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap_or_else(|error| panic!("temp dir for {prefix}: {error}"))
}

/// Resolves symlinks the same way [`TempRepo`] and repository registration
/// both do, so a test can predict the exact path an error message names.
fn canonicalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()))
}

/// A repository this suite builds by hand, for shapes [`TempRepo`] does not
/// model: a branch name that is neither `main` nor `master`, or a repository
/// with no commit at all. Holds its temp directory alive for as long as the
/// value lives, exactly as `TempRepo` does for its own — dropping it before
/// a git call below would run git against a directory that had just been
/// deleted out from under it.
struct RawRepo {
    _root: tempfile::TempDir,
    path: PathBuf,
}

impl RawRepo {
    fn path(&self) -> &Path {
        &self.path
    }
}

/// A bare-bones repository on `branch`, canonicalized, with no commits —
/// callers add their own. Distinct from [`TempRepo::init`], which always
/// commits once on `main`: several tests here need a branch name that is
/// neither `main` nor `master`, or a repository with no commit at all.
fn init_repo_on_branch(branch: &str) -> RawRepo {
    let root = scratch_dir("rimaia-raw-repo-");
    let canonical = canonicalize(root.path());
    git(&canonical, &["init", "-q", "-b", branch]);
    git(&canonical, &["config", "user.name", "Rimaia Test"]);
    git(&canonical, &["config", "user.email", "test@rimaia.invalid"]);
    git(&canonical, &["config", "commit.gpgsign", "false"]);
    RawRepo {
        _root: root,
        path: canonical,
    }
}

/// Writes and commits one file — the same shape `TempRepo::commit` offers,
/// available here for repositories `init_repo_on_branch` built instead.
fn commit(dir: &Path, file: &str, contents: &str) {
    std::fs::write(dir.join(file), contents).expect("write a file to commit");
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-m", "test commit"]);
}

/// Points `refs/remotes/origin/HEAD` at `branch`, the way `git remote
/// set-head origin -a` would once the remote actually advertised it — done
/// by hand because a plain `git fetch` never creates this ref on its own,
/// and the dance through a real bare remote's own `HEAD` is more fixture than
/// the test after it needs.
fn point_origin_head_at(dir: &Path, branch: &str) {
    let tracking_ref = format!("refs/remotes/origin/{branch}");
    git(dir, &["update-ref", &tracking_ref, "HEAD"]);
    git(
        dir,
        &["symbolic-ref", "refs/remotes/origin/HEAD", &tracking_ref],
    );
}

/// Runs git in `dir` and returns trimmed stdout, panicking with both streams
/// on failure — a git error in a test fixture is a broken test, not a
/// handled condition. Matches `crates/core/tests/repo.rs`'s own helper.
fn git<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("could not run git in {}: {error}", dir.display()));

    if !output.status.success() {
        panic!(
            "git {} failed in {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            args.iter()
                .map(|arg| arg.as_ref().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
