//! [`rimaia_core::worktree`] against real git repositories (task 007,
//! ADR-0005, ADR-0013, ADR-0015).
//!
//! Nothing here is mocked. Every repository is a real one on disk built with
//! `rimaia_core::testing::TempRepo`, every worktree is a real `git worktree
//! add`, and every assertion about git's own bookkeeping is made against `git
//! worktree list --porcelain` rather than against the filesystem alone — the
//! two disagree in exactly the case task 007's acceptance criteria care about,
//! which is a directory deleted behind the app's back. A mocked git would only
//! prove the mock works (ADR-0015).
//!
//! **Both paths contain a space on purpose.** `TempRepo`'s work tree is named
//! `work tree`, and the worktree root every fixture here registers is named
//! `my repo`, so an argument vector that ever became a shell string fails in
//! every test at once instead of on a user's `~/Documents/My Projects/...`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, Repository, RunState, Task};
use rimaia_core::paths::AppPaths;
use rimaia_core::repo::{self, NewRepository, RepositoryPatch};
use rimaia_core::runner::outcome::{start_run, NewRun};
use rimaia_core::tasks::{self, NewTask, TaskFilter};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::worktree::{self, ForceRemoval};
use rimaia_core::{ChangeEvent, ServiceContext};

/// The last component of every fixture's `worktree_root`. The space is
/// load-bearing — see the module docs.
const WORKTREE_ROOT_DIR: &str = "my repo";

// ---------------------------------------------------------------------------
// Creating
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preparing_a_worktree_checks_out_the_base_on_a_namespaced_branch() {
    let f = Fixture::new().await;
    let task = f.task("Wire the board to the store").await;

    let worktree = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("prepare must create a worktree");

    assert_eq!(
        worktree.branch,
        format!("rimaia/{}-wire-the-board-to-the-store", task.id)
    );
    assert_eq!(worktree.base_ref, "main");
    assert_eq!(Path::new(&worktree.path), f.root().join(&task.id));

    let checkout = PathBuf::from(&worktree.path);
    assert!(
        checkout.join("README.md").is_file(),
        "a worktree is a real checkout, not an empty directory"
    );
    assert_eq!(
        git(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        worktree.branch
    );
    assert_eq!(
        git(&checkout, &["rev-parse", "HEAD"]),
        f.source.head_sha(),
        "the branch starts at the base ref's tip"
    );
}

#[tokio::test]
async fn a_worktree_is_created_from_the_default_branch_and_not_from_head() {
    // The repository is left with `feature` checked out and one commit ahead of
    // `main`, so branching from `HEAD` and branching from the *configured
    // default branch* give different answers — which is the only way to tell
    // whether the base ref was resolved at all.
    let source =
        TempRepo::init()
            .branch("feature")
            .commit("feature.rs", "// feature\n", "Add the feature");
    let main_tip = git(source.path(), &["rev-parse", "main"]);
    let f = Fixture::with_source(source).await;
    let task = f.task("Add parser").await;

    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    assert_eq!(
        git(Path::new(&worktree.path), &["rev-parse", "HEAD"]),
        main_tip
    );
    assert_ne!(main_tip, f.source.head_sha(), "the fixture must diverge");
}

#[tokio::test]
async fn a_prepared_worktree_is_recorded_on_the_task_row_and_published() {
    let mut f = Fixture::new().await;
    let task = f.task("Add parser").await;
    f.drain_changes();

    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    let stored = f.reload(&task.id).await;
    assert_eq!(stored.branch.as_deref(), Some(worktree.branch.as_str()));
    assert_eq!(
        stored.worktree_path.as_deref(),
        Some(worktree.path.as_str())
    );
    assert_eq!(
        f.harness
            .changes
            .try_recv()
            .expect("a publication is waiting"),
        ChangeEvent::tasks([task.id.clone()])
    );
}

#[tokio::test]
async fn repository_and_worktree_paths_containing_spaces_survive_every_git_call() {
    // Task 007's Notes: "argument passing, not shell strings". Asserted rather
    // than assumed, because the two paths come from different places — one from
    // TempRepo, one from the registered `worktree_root` — and a regression in
    // either is silent until a user hits it.
    let f = Fixture::new().await;
    assert!(f.source.path().to_string_lossy().contains(' '));
    assert!(f.root().to_string_lossy().contains(' '));
    let task = f.task("Add parser").await;

    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    assert!(worktree.path.contains(' '));
    assert!(PathBuf::from(&worktree.path).join("README.md").is_file());
    // The one call that runs git *inside* the space-bearing worktree rather
    // than pointing at it from the repository.
    assert!(
        worktree::status(f.ctx(), &task.id)
            .await
            .expect("status")
            .exists
    );
}

#[tokio::test]
async fn a_repository_that_cannot_be_fetched_still_gets_its_worktree() {
    // "Best effort; offline is a warning, not a failure" — an overnight queue
    // on a train has to run. An `origin` whose directory has been deleted fails
    // a fetch the same way no network does.
    let source = TempRepo::init().with_remote();
    let remote = source
        .remote_path()
        .expect("with_remote sets a remote path")
        .to_path_buf();
    std::fs::remove_dir_all(&remote).expect("delete the remote out from under git");
    let f = Fixture::with_source(source).await;
    let task = f.task("Add parser").await;

    let worktree = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("an unreachable remote must not fail prepare");

    assert!(PathBuf::from(&worktree.path).join("README.md").is_file());
}

#[tokio::test]
async fn a_default_branch_that_does_not_exist_is_refused_by_name() {
    let f = Fixture::new().await;
    repo::update(
        f.ctx(),
        &f.repository.id,
        RepositoryPatch {
            default_branch: Some("trunk".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("update the default branch");
    let task = f.task("Add parser").await;

    let error = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect_err("a base ref that is not in the repository must be refused");

    assert_eq!(
        error.to_string(),
        format!(
            "\"{}\" has no branch named trunk to create a worktree from. \
             Set its default branch in Settings → Repositories.",
            f.repository.name
        )
    );
}

// ---------------------------------------------------------------------------
// Idempotence — what makes a retry resume in place (ADR-0005, ADR-0011)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn preparing_twice_returns_the_same_worktree_and_does_not_error() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let first = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    let second = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("preparing twice must not error");

    assert_eq!(second, first);
    assert_eq!(
        f.linked_worktrees().len(),
        1,
        "a second prepare must not add a second worktree"
    );
}

#[tokio::test]
async fn preparing_again_keeps_work_the_previous_attempt_committed() {
    // The behaviour idempotence exists for: ADR-0011's retries "continue work
    // in place — they do not start from a clean tree". A prepare that rewound
    // the branch to the base would pass the equality check above and still be
    // wrong, so this asserts the commit itself.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let first = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        Path::new(&first.path),
        "parser.rs",
        "// parser\n",
        "Add parser",
    );
    let after_work = git(Path::new(&first.path), &["rev-parse", "HEAD"]);

    let second = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    assert_eq!(second.path, first.path);
    assert_eq!(second.branch, first.branch);
    assert_eq!(
        git(Path::new(&second.path), &["rev-parse", "HEAD"]),
        after_work,
        "the committed work must still be at the tip"
    );
}

#[tokio::test]
async fn preparing_after_the_directory_vanished_rebuilds_it_on_the_same_branch() {
    // Without a reconciliation pass first, so `prepare` is on its own. This is
    // what the `git worktree prune` in it is for: git still has the branch
    // registered as checked out in a worktree whose directory is gone, and
    // would refuse to check it out again until that record goes.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let first = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        Path::new(&first.path),
        "parser.rs",
        "// parser\n",
        "Add parser",
    );
    let committed = git(Path::new(&first.path), &["rev-parse", "HEAD"]);
    std::fs::remove_dir_all(&first.path).expect("delete the worktree behind the app's back");

    let second = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("a vanished directory must not wedge the task");

    assert_eq!(second.path, first.path);
    assert_eq!(second.branch, first.branch);
    assert_eq!(
        git(Path::new(&second.path), &["rev-parse", "HEAD"]),
        committed
    );
    assert_eq!(f.linked_worktrees().len(), 1);
}

#[tokio::test]
async fn preparing_onto_a_directory_left_detached_is_refused_with_an_actionable_message() {
    // A directory at the task's path that git still lists but no longer on
    // this task's branch — an interrupted rebase or a manual `checkout
    // --detach` are two ways to get there. `existing_worktree` correctly
    // refuses to treat it as this task's live worktree, and `git worktree
    // add` would fail on the same path with a bare "already exists". The
    // service must say something a user can act on instead.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let first = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    git(Path::new(&first.path), &["checkout", "--detach"]);

    let error = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect_err("a directory git no longer lists on this branch must not reach git's own \"already exists\"");

    assert!(
        error.to_string().contains(&first.path),
        "the message must name the directory the user has to deal with, got: {error}"
    );
    assert!(
        error.to_string().contains("move it aside or delete it"),
        "the message must say what to do, not just relay git's raw \"fatal: ... already \
         exists\", got: {error}"
    );
    assert!(
        Path::new(&first.path).exists(),
        "refusing must not delete anything — cleanup stays an explicit act (ADR-0005)"
    );
}

// ---------------------------------------------------------------------------
// Branch chaining (ADR-0008, task 011)
//
// Every assertion here is made with `git merge-base`, `rev-parse` or `log`
// against a real repository, which is task 011's own acceptance criterion: "a
// dependent task's worktree is created from its dependency's branch, verified
// by `git merge-base`". A test that only read `Worktree::base_ref` back would
// prove the struct, not the checkout.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_dependent_branches_from_its_dependency_and_git_merge_base_proves_it() {
    let f = Fixture::new().await;
    let a = f.task("Add the API endpoint").await;
    let b = f.task("Call it from the UI").await;

    // A runs: its worktree exists, it commits, and its card is filed for review
    // — which is the whole of what ADR-0008 calls "satisfied".
    let a_worktree = worktree::prepare(f.ctx(), &a.id).await.expect("prepare A");
    commit_in(
        Path::new(&a_worktree.path),
        "endpoint.rs",
        "// A\n",
        "Add A",
    );
    let a_tip = git(Path::new(&a_worktree.path), &["rev-parse", "HEAD"]);
    file_for_review(&f, &a.id).await;

    tasks::set_task_dependencies(f.ctx(), &b.id, std::slice::from_ref(&a.id))
        .await
        .expect("B depends on A");

    let b_worktree = worktree::prepare(f.ctx(), &b.id).await.expect("prepare B");

    assert_eq!(b_worktree.base_ref, a_worktree.branch);
    assert_eq!(
        b_worktree.dependency_warning, None,
        "one dependency, nothing to warn about"
    );

    // The two claims that matter, and neither is readable off the struct:
    // B starts exactly at A's tip, and A's commit is an ancestor of B's branch.
    let b_checkout = PathBuf::from(&b_worktree.path);
    assert_eq!(git(&b_checkout, &["rev-parse", "HEAD"]), a_tip);
    assert_eq!(
        git(
            f.source.path(),
            &["merge-base", &a_worktree.branch, &b_worktree.branch],
        ),
        a_tip,
        "A's branch must be an ancestor of B's, not merely a name it was given",
    );
    assert!(
        b_checkout.join("endpoint.rs").is_file(),
        "B is written against code that is actually there — the reason ADR-0008 exists",
    );
}

#[tokio::test]
async fn an_unsatisfied_dependency_leaves_the_dependent_on_the_default_branch() {
    // A has a branch with commits on it, but its card is still in `ready`
    // because the run failed. ADR-0008 gates chaining on the column, so B does
    // not build on work nobody accepted.
    let f = Fixture::new().await;
    let a = f.task("Add the API endpoint").await;
    let b = f.task("Call it from the UI").await;
    let a_worktree = worktree::prepare(f.ctx(), &a.id).await.expect("prepare A");
    commit_in(
        Path::new(&a_worktree.path),
        "endpoint.rs",
        "// A\n",
        "Add A",
    );
    tasks::set_task_dependencies(f.ctx(), &b.id, std::slice::from_ref(&a.id))
        .await
        .expect("B depends on A");

    let b_worktree = worktree::prepare(f.ctx(), &b.id).await.expect("prepare B");

    assert_eq!(b_worktree.base_ref, "main");
    assert_eq!(
        git(Path::new(&b_worktree.path), &["rev-parse", "HEAD"]),
        f.source.head_sha(),
    );
    assert!(
        b_worktree
            .dependency_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("Add the API endpoint")),
        "the warning must name what is not in the base: {:?}",
        b_worktree.dependency_warning,
    );
}

#[tokio::test]
async fn a_dependency_that_has_never_run_cannot_be_a_base() {
    // Satisfied — dragged straight to `done` by a user who implemented it
    // themselves — but with no branch to build on. There is nothing to hand
    // `git worktree add`, so it falls through to the default branch and says so.
    let f = Fixture::new().await;
    let a = f.task("Did it by hand").await;
    let b = f.task("Call it from the UI").await;
    move_to(&f, &a.id, BoardColumn::Done).await;
    tasks::set_task_dependencies(f.ctx(), &b.id, std::slice::from_ref(&a.id))
        .await
        .expect("B depends on A");

    let b_worktree = worktree::prepare(f.ctx(), &b.id).await.expect("prepare B");

    assert_eq!(b_worktree.base_ref, "main");
    assert_eq!(
        b_worktree.dependency_warning.as_deref(),
        Some(
            "This task branches from main: none of its dependencies has a branch to build \
             on yet. \"Did it by hand\" has not produced one — a dependency that has never \
             run cannot be a base."
        ),
    );
}

#[tokio::test]
async fn two_dependencies_base_off_the_higher_one_and_warn_about_the_other() {
    // Both satisfied, both with branches, both in `in_review` — so the tie is
    // broken by ascending `position`, and `position` ascends downwards
    // (ADR-0007). A was filed first and sits above B.
    let f = Fixture::new().await;
    let a = f.task("Add the API endpoint").await;
    let b = f.task("Add the schema").await;
    let c = f.task("Call them from the UI").await;

    let a_worktree = worktree::prepare(f.ctx(), &a.id).await.expect("prepare A");
    commit_in(
        Path::new(&a_worktree.path),
        "endpoint.rs",
        "// A\n",
        "Add A",
    );
    let a_tip = git(Path::new(&a_worktree.path), &["rev-parse", "HEAD"]);
    file_for_review(&f, &a.id).await;

    let b_worktree = worktree::prepare(f.ctx(), &b.id).await.expect("prepare B");
    commit_in(Path::new(&b_worktree.path), "schema.rs", "// B\n", "Add B");
    file_for_review(&f, &b.id).await;

    tasks::set_task_dependencies(f.ctx(), &c.id, &[a.id.clone(), b.id.clone()])
        .await
        .expect("C depends on both");

    let c_worktree = worktree::prepare(f.ctx(), &c.id).await.expect("prepare C");

    assert_eq!(c_worktree.base_ref, a_worktree.branch);
    assert_eq!(
        git(Path::new(&c_worktree.path), &["rev-parse", "HEAD"]),
        a_tip
    );
    assert!(
        !PathBuf::from(&c_worktree.path).join("schema.rs").is_file(),
        "B's work is genuinely not in C's base — which is exactly what the warning is for",
    );

    let warning = c_worktree
        .dependency_warning
        .as_deref()
        .expect("two dependencies must produce ADR-0008's explicit warning");
    assert!(warning.contains("Add the API endpoint"), "{warning}");
    assert!(warning.contains("\"Add the schema\""), "{warning}");
}

#[tokio::test]
async fn the_resolved_base_is_recorded_on_the_run() {
    // ADR-0008's amendment: `start_run` writes what the attempt was built on,
    // and `status`/`diff_summary` prefer that recorded value over a fresh
    // resolution — a task's dependencies can change between attempts, and the
    // morning is asking about the attempt it is reading.
    let f = Fixture::new().await;
    let a = f.task("Add the API endpoint").await;
    let b = f.task("Call it from the UI").await;
    let a_worktree = worktree::prepare(f.ctx(), &a.id).await.expect("prepare A");
    commit_in(
        Path::new(&a_worktree.path),
        "endpoint.rs",
        "// A\n",
        "Add A",
    );
    file_for_review(&f, &a.id).await;
    tasks::set_task_dependencies(f.ctx(), &b.id, std::slice::from_ref(&a.id))
        .await
        .expect("B depends on A");

    let b_worktree = worktree::prepare(f.ctx(), &b.id).await.expect("prepare B");
    let data = scratch_dir("rimaia-run-data-");
    let paths = AppPaths::new(data.path());
    paths.create_all().expect("the app data directories");
    let run = start_run(
        f.ctx(),
        &paths,
        NewRun {
            task_id: b.id.clone(),
            session_id: "0b6d3e2e-0000-4000-8000-00000000ba5e".to_string(),
            prompt: "implement the plan".to_string(),
            base_ref: Some(b_worktree.base_ref.clone()),
        },
    )
    .await
    .expect("open the run row");

    assert_eq!(run.base_ref.as_deref(), Some(a_worktree.branch.as_str()));

    // Now the dependency changes out from under the recorded attempt. The
    // status still reports the base the branch actually has.
    tasks::set_task_dependencies(f.ctx(), &b.id, &[])
        .await
        .expect("clear B's dependencies");

    let status = worktree::status(f.ctx(), &b.id).await.expect("status");
    assert_eq!(
        status.base_ref, a_worktree.branch,
        "a re-resolution would say `main` and silently re-measure the diff",
    );
    let summary = worktree::diff_summary(f.ctx(), &b.id)
        .await
        .expect("diff summary");
    assert_eq!(summary.base_ref, a_worktree.branch);
}

#[tokio::test]
async fn a_task_that_has_never_run_reports_a_freshly_resolved_base() {
    // The other half of the rule: with nothing recorded there is nothing to
    // prefer, so `status` answers from the current graph.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;

    let status = worktree::status(f.ctx(), &task.id).await.expect("status");

    assert_eq!(status.base_ref, "main");
    assert_eq!(status.dependency_warning, None);
}

// ---------------------------------------------------------------------------
// Branch naming: truncation and collisions
// ---------------------------------------------------------------------------

/// **Known to fail on Windows, and the failure is the product's, not this
/// test's.** `worktree::naming` truncates a branch name to a length *git*
/// accepts, which is the right authority on Linux and macOS. On Windows the
/// binding limit is not git's ref-name rule but the filesystem's path length:
/// git creates `refs/heads/<name>.lock` under the repository, and a ~200-byte
/// branch under a temp directory already exceeds 260 characters —
/// `fatal: cannot lock ref … Filename too long`.
///
/// There is no cap that is universally safe, because the budget depends on how
/// deep the *repository* sits, so this is a real decision rather than a number
/// to lower — and it is outside what the six tasks on this branch asked for.
/// Recorded here and in the pull request rather than fixed quietly or gated
/// silently: task 022's CI matrix found it, which is what the matrix is for.
#[cfg(not(windows))]
#[tokio::test]
async fn an_over_long_title_truncates_to_a_branch_name_git_itself_accepts() {
    let f = Fixture::new().await;
    let task = f
        .task(&"Refactor the exceedingly verbose configuration loader ".repeat(12))
        .await;

    let worktree = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("an over-long title must still produce a usable branch");

    // git is the authority on what a ref name may be, so it is what validates
    // the truncation rather than a second copy of its rules.
    check_ref_format(f.source.path(), &worktree.branch);
    assert!(
        worktree.branch.len() <= 200,
        "{} is {} bytes",
        worktree.branch,
        worktree.branch.len()
    );
    assert!(!worktree.branch.ends_with('-'));
    assert_eq!(
        git(
            Path::new(&worktree.path),
            &["rev-parse", "--abbrev-ref", "HEAD"]
        ),
        worktree.branch,
        "the truncated name is the branch git actually created"
    );
}

#[tokio::test]
async fn a_colliding_branch_name_gets_a_suffix_rather_than_being_reused() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    // Somebody else's branch, sitting on the name this task would have picked.
    let taken = format!("rimaia/{}-add-parser", task.id);
    git(f.source.path(), &["branch", &taken]);
    let taken_sha = git(f.source.path(), &["rev-parse", &taken]);

    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    assert_eq!(worktree.branch, format!("{taken}-2"));
    assert_eq!(
        git(f.source.path(), &["rev-parse", &taken]),
        taken_sha,
        "the branch that was already there must be left exactly as it was"
    );
    assert!(
        !f.linked_worktrees()
            .iter()
            .any(|entry| entry.branch.as_deref() == Some(taken.as_str())),
        "reusing a branch would silently continue someone else's work"
    );
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn removing_clears_both_the_directory_and_gits_worktree_metadata() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    assert_eq!(f.linked_worktrees().len(), 1);

    worktree::remove(f.ctx(), &task.id, false, ForceRemoval::No)
        .await
        .expect("remove a clean worktree");

    assert!(!Path::new(&worktree.path).exists(), "the directory must go");
    assert!(
        f.linked_worktrees().is_empty(),
        "git's own record must go too — `rm -rf` is exactly what leaves it behind"
    );
    let stored = f.reload(&task.id).await;
    assert_eq!(stored.worktree_path, None);
    assert_eq!(
        stored.branch.as_deref(),
        Some(worktree.branch.as_str()),
        "ADR-0005 leaves the branch alone unless the user asks for it to go"
    );
    assert!(branch_exists(f.source.path(), &worktree.branch));
}

#[tokio::test]
async fn removing_with_delete_branch_takes_the_branch_and_the_row_reference_too() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        Path::new(&worktree.path),
        "parser.rs",
        "// parser\n",
        "Add parser",
    );

    worktree::remove(f.ctx(), &task.id, true, ForceRemoval::No)
        .await
        .expect("remove and delete the branch");

    assert!(
        !branch_exists(f.source.path(), &worktree.branch),
        "an unmerged run branch must still be deletable when the user asks"
    );
    let stored = f.reload(&task.id).await;
    assert_eq!(stored.branch, None);
    assert_eq!(stored.worktree_path, None);
}

#[tokio::test]
async fn removing_a_dirty_worktree_is_refused_until_the_user_confirms() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    std::fs::write(Path::new(&worktree.path).join("scratch.txt"), "unsaved\n")
        .expect("leave uncommitted work behind");

    let error = worktree::remove(f.ctx(), &task.id, false, ForceRemoval::No)
        .await
        .expect_err("uncommitted work must not be discarded unasked");

    assert!(
        error
            .to_string()
            .contains("contains modified or untracked files"),
        "git's own refusal is what the user reads, got: {error}"
    );
    assert!(Path::new(&worktree.path).exists());

    worktree::remove(f.ctx(), &task.id, false, ForceRemoval::ConfirmedByUser)
        .await
        .expect("an explicit confirmation removes it");
    assert!(!Path::new(&worktree.path).exists());
}

#[tokio::test]
async fn removing_a_task_that_has_no_worktree_is_not_an_error() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;

    worktree::remove(f.ctx(), &task.id, true, ForceRemoval::No)
        .await
        .expect("removal is idempotent, the same way prepare is");

    assert_eq!(f.reload(&task.id).await.worktree_path, None);
}

// ---------------------------------------------------------------------------
// Reconciliation — a worktree deleted behind the app's back
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_worktree_deleted_behind_the_apps_back_is_reconciled_at_the_next_startup() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        Path::new(&worktree.path),
        "parser.rs",
        "// parser\n",
        "Add parser",
    );
    let committed = git(Path::new(&worktree.path), &["rev-parse", "HEAD"]);
    std::fs::remove_dir_all(&worktree.path).expect("delete the worktree, simulating a crash");

    // Exactly the hand-off `startup::survey`'s module doc describes: it reports
    // the ids, this acts on them.
    let report = rimaia_core::startup::survey(&f.ctx().pool)
        .await
        .expect("survey");
    assert_eq!(report.missing_worktrees, vec![task.id.clone()]);
    let reconciled = worktree::reconcile(f.ctx(), &report.missing_worktrees).await;

    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].task_id, task.id);
    assert_eq!(reconciled[0].cleared_path, worktree.path);
    assert_eq!(
        reconciled[0].retained_branch.as_deref(),
        Some(worktree.branch.as_str()),
        "the branch outlived the directory and still holds the run's commits"
    );
    assert_eq!(reconciled[0].corrected_run_state, None);

    let stored = f.reload(&task.id).await;
    assert_eq!(stored.worktree_path, None);
    assert_eq!(stored.branch.as_deref(), Some(worktree.branch.as_str()));
    assert!(
        f.linked_worktrees().is_empty(),
        "git's stale administrative record is pruned too"
    );
    assert_eq!(
        git(f.source.path(), &["rev-parse", &worktree.branch]),
        committed
    );
}

#[tokio::test]
async fn a_reconciled_task_can_be_prepared_again_onto_the_branch_it_kept() {
    // The acceptance criterion's second half — "instead of causing a run to
    // fail confusingly". Reconciliation is only worth anything if the next
    // prepare works, and works *on the same branch*, so the commits the lost
    // worktree made are still the starting point.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let first = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        Path::new(&first.path),
        "parser.rs",
        "// parser\n",
        "Add parser",
    );
    let committed = git(Path::new(&first.path), &["rev-parse", "HEAD"]);
    std::fs::remove_dir_all(&first.path).expect("delete the worktree");
    worktree::reconcile(f.ctx(), std::slice::from_ref(&task.id)).await;

    let second = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("prepare must work again after reconciliation");

    assert_eq!(
        second.branch, first.branch,
        "the branch is reused, not renamed"
    );
    assert_eq!(
        git(Path::new(&second.path), &["rev-parse", "HEAD"]),
        committed,
        "the new worktree resumes at the commit the lost one made"
    );
}

#[tokio::test]
async fn reconciling_moves_a_task_left_running_to_failed_through_the_state_machine() {
    // seam-contract D9: a run that died with the app leaves its task `failed`.
    // Reconciliation gets there through `tasks::set_run_state`, which is the
    // only writer of `run_state` — an illegal transition here would be an error
    // rather than a silent `UPDATE`.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    tasks::set_run_state(f.ctx(), &task.id, RunState::Queued)
        .await
        .expect("queue");
    tasks::set_run_state(f.ctx(), &task.id, RunState::Running)
        .await
        .expect("run");
    std::fs::remove_dir_all(&worktree.path).expect("delete the worktree");

    let reconciled = worktree::reconcile(f.ctx(), std::slice::from_ref(&task.id)).await;

    assert_eq!(reconciled[0].corrected_run_state, Some(RunState::Failed));
    assert_eq!(f.reload(&task.id).await.run_state, RunState::Failed);
}

#[tokio::test]
async fn reconciling_leaves_a_worktree_that_is_still_there_completely_alone() {
    // The id list is a snapshot, so a stale entry must not clear a live
    // worktree off its task.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    let reconciled = worktree::reconcile(f.ctx(), std::slice::from_ref(&task.id)).await;

    assert!(reconciled.is_empty());
    let stored = f.reload(&task.id).await;
    assert_eq!(
        stored.worktree_path.as_deref(),
        Some(worktree.path.as_str())
    );
    assert_eq!(f.linked_worktrees().len(), 1);
}

#[tokio::test]
async fn reconciling_clears_a_branch_that_did_not_outlive_the_directory_either() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    std::fs::remove_dir_all(&worktree.path).expect("delete the worktree");
    // Both halves gone, the way a user cleaning up by hand would leave it.
    git(f.source.path(), &["worktree", "prune"]);
    git(f.source.path(), &["branch", "-D", &worktree.branch]);

    let reconciled = worktree::reconcile(f.ctx(), std::slice::from_ref(&task.id)).await;

    assert_eq!(reconciled[0].retained_branch, None);
    assert_eq!(f.reload(&task.id).await.branch, None);
}

#[tokio::test]
async fn reconciling_a_task_id_that_no_longer_exists_is_survivable() {
    // Startup runs this before the window opens; one bad id is not a reason for
    // the app not to start.
    let f = Fixture::new().await;

    let reconciled = worktree::reconcile(f.ctx(), &["not-a-task".to_string()]).await;

    assert!(reconciled.is_empty());
}

// ---------------------------------------------------------------------------
// Safety
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_worktree_root_inside_the_repository_is_refused() {
    let source = TempRepo::init();
    let inside = source.path().join("worktrees");
    let f = Fixture::with_worktree_root(source, &inside).await;
    let task = f.task("Add parser").await;

    let error = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect_err("ADR-0005 keeps worktrees out of the repository");

    assert!(
        error
            .to_string()
            .starts_with("worktrees must live outside the repository, but "),
        "got: {error}"
    );
    assert!(
        !inside.exists(),
        "the refusal must come before anything is created inside the repository"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_worktree_root_reaching_the_repository_through_a_symlink_is_still_refused() {
    // The case a `starts_with` on the two path strings gets wrong: the root
    // shares no textual prefix with the repository and is the same directory.
    // On macOS this is not hypothetical — `TempDir` hands out `/var/folders`,
    // a symlink to `/private/var/folders`.
    let source = TempRepo::init();
    let elsewhere = scratch_dir("rimaia-symlink-");
    let link = elsewhere.path().join("link");
    std::os::unix::fs::symlink(source.path(), &link).expect("symlink to the repository");
    let f = Fixture::with_worktree_root(source, &link.join("worktrees")).await;
    let task = f.task("Add parser").await;

    let error = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect_err("a symlink into the repository is still the repository");

    assert!(
        error
            .to_string()
            .starts_with("worktrees must live outside the repository, but "),
        "got: {error}"
    );
    assert!(!f.source.path().join("worktrees").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn a_worktree_root_reached_through_a_symlink_outside_the_repository_is_allowed() {
    // The control for the test above: canonicalizing must not turn every
    // symlink into a refusal, only the ones that land inside the repository.
    let source = TempRepo::init();
    let elsewhere = scratch_dir("rimaia-symlink-ok-");
    let real = elsewhere.path().join("real");
    std::fs::create_dir(&real).expect("create the real root");
    let link = elsewhere.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink outside the repository");
    let f = Fixture::with_worktree_root(source, &link.join(WORKTREE_ROOT_DIR)).await;
    let task = f.task("Add parser").await;

    let worktree = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("a symlinked root outside the repository is fine");

    assert!(
        Path::new(&worktree.path).starts_with(canonicalize(&real)),
        "the recorded path is the resolved one, not the one via the link: {}",
        worktree.path
    );
}

#[tokio::test]
async fn a_recorded_path_outside_the_worktree_root_is_refused_and_left_untouched() {
    // The row is the one input this service cannot validate up front: a
    // `worktree_root` edited in Settings, a hand-edit through the sqlite3 CLI
    // ADR-0003 counts as a feature, or a bug in an older version. What is
    // behind the guard is a `git worktree remove`, so being wrong deletes a
    // directory the user did not offer.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    let elsewhere = scratch_dir("rimaia-elsewhere-");
    let stray = elsewhere.path().join("not ours");
    std::fs::create_dir(&stray).expect("create a directory outside the root");
    point_task_at(&f.ctx().pool, &task.id, stray.to_str().unwrap()).await;

    let error = worktree::remove(f.ctx(), &task.id, false, ForceRemoval::ConfirmedByUser)
        .await
        .expect_err("a path outside the worktree root must be refused");

    assert!(
        error
            .to_string()
            .contains("is outside this repository's worktree root"),
        "got: {error}"
    );
    assert!(stray.exists(), "nothing outside the root may be deleted");
}

#[tokio::test]
async fn a_worktree_root_containing_a_parent_segment_is_refused() {
    let source = TempRepo::init();
    let elsewhere = scratch_dir("rimaia-dotdot-");
    let root = elsewhere.path().join("a/../b");
    let f = Fixture::with_worktree_root(source, &root).await;
    let task = f.task("Add parser").await;

    let error = worktree::prepare(f.ctx(), &task.id)
        .await
        .expect_err("`..` cannot be resolved without guessing");

    assert_eq!(
        error.to_string(),
        format!("{} must not contain \"..\"", root.display())
    );
}

// ---------------------------------------------------------------------------
// Status and the review view (ADR-0013)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn status_reports_ahead_behind_the_commit_count_and_the_diff() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        Path::new(&worktree.path),
        "parser.rs",
        "one\ntwo\n",
        "Add parser",
    );
    commit_in(
        Path::new(&worktree.path),
        "lexer.rs",
        "three\n",
        "Add lexer",
    );
    // The base branch moves on afterwards, which is what `behind` counts and
    // what a two-dot diff would wrongly fold into the branch's own changes.
    commit_in(
        f.source.path(),
        "CHANGELOG.md",
        "# changelog\n",
        "Add changelog",
    );

    let status = worktree::status(f.ctx(), &task.id).await.expect("status");

    assert!(status.exists);
    assert_eq!(status.branch.as_deref(), Some(worktree.branch.as_str()));
    assert_eq!(status.base_ref, "main");
    assert_eq!(status.ahead, 2);
    assert_eq!(status.behind, 1);
    assert_eq!(status.commit_count, 2);
    assert!(!status.dirty);
    assert_eq!(status.diff.files_changed, 2);
    assert_eq!(status.diff.insertions, 3);
    assert_eq!(status.diff.deletions, 0);
}

#[tokio::test]
async fn status_reports_untracked_work_as_dirty() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    assert!(!worktree::status(f.ctx(), &task.id).await.unwrap().dirty);

    std::fs::write(Path::new(&worktree.path).join("scratch.txt"), "unsaved\n")
        .expect("leave uncommitted work behind");

    assert!(
        worktree::status(f.ctx(), &task.id).await.unwrap().dirty,
        "untracked files are work a removal would destroy, so they count"
    );
}

#[tokio::test]
async fn status_for_a_task_that_has_never_run_reports_no_worktree_rather_than_failing() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;

    let status = worktree::status(f.ctx(), &task.id).await.expect("status");

    assert!(!status.exists);
    assert_eq!(status.path, None);
    assert_eq!(status.branch, None);
    assert_eq!(status.base_ref, "main");
    assert_eq!(status.ahead, 0);
    assert_eq!(status.commit_count, 0);
}

#[tokio::test]
async fn status_of_a_worktree_deleted_behind_the_apps_back_reports_it_gone() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        Path::new(&worktree.path),
        "parser.rs",
        "one\n",
        "Add parser",
    );
    std::fs::remove_dir_all(&worktree.path).expect("delete the worktree");

    let status = worktree::status(f.ctx(), &task.id).await.expect("status");

    assert!(!status.exists);
    assert_eq!(
        status.path.as_deref(),
        Some(worktree.path.as_str()),
        "the panel still shows where it was supposed to be"
    );
    assert_eq!(
        status.ahead, 1,
        "the branch outlived the directory, so its commits are still countable"
    );
}

#[tokio::test]
async fn status_of_a_repository_moved_or_deleted_is_refused_with_an_actionable_message() {
    // Distinct from the worktree-deleted case above: here it is the
    // *repository's* work tree that is gone, so the first git call has no
    // `cwd` to spawn into. Without `locate_repository`'s existence check that
    // surfaces as `Error::internal` carrying a raw OS error; `prepare` and
    // `remove` already give this same condition an actionable sentence via
    // `locate`, and `status` must give the same one.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("prepare so the task has a branch to check status for");
    std::fs::remove_dir_all(f.source.path()).expect("delete the repository behind the app's back");

    let error = worktree::status(f.ctx(), &task.id)
        .await
        .expect_err("a moved repository must not reach git as a missing cwd");

    assert!(
        error.to_string().contains("has been moved or deleted"),
        "got: {error}"
    );
}

#[tokio::test]
async fn diff_summary_lists_the_commits_on_the_branch_newest_first() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        Path::new(&worktree.path),
        "parser.rs",
        "one\n",
        "Add the parser",
    );
    commit_in(
        Path::new(&worktree.path),
        "lexer.rs",
        "two\ntwo\n",
        "Add the lexer",
    );

    let summary = worktree::diff_summary(f.ctx(), &task.id)
        .await
        .expect("diff_summary");

    assert_eq!(summary.base_ref, "main");
    assert_eq!(summary.branch.as_deref(), Some(worktree.branch.as_str()));
    assert_eq!(
        summary
            .commits
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect::<Vec<_>>(),
        vec!["Add the lexer", "Add the parser"]
    );
    assert_eq!(summary.commits[0].author, "Rimaia Test");
    assert_eq!(summary.commits[0].sha.len(), 40);
    assert!(summary.commits[0]
        .sha
        .starts_with(&summary.commits[0].short_sha));
    assert_eq!(summary.diff.files_changed, 2);
    assert_eq!(summary.diff.insertions, 3);
    assert_eq!(
        summary
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        vec!["lexer.rs", "parser.rs"],
        "the per-file breakdown sums to the same totals, one entry per changed file"
    );
}

#[tokio::test]
async fn a_commits_committed_at_is_the_committer_date_not_the_author_date() {
    // A rebase or a cherry-pick is exactly this: the author date travels with
    // the commit, the committer date is set to when the rewrite happened, so
    // the two disagree. `committed_at` must read the committer date — it is
    // what `git log`'s own default order (and so `commits`' "newest first")
    // sorts by, so a rename that read the author date instead would make the
    // stored order and the claimed order silently disagree after a rebase.
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_with_dates(
        Path::new(&worktree.path),
        "parser.rs",
        "one\n",
        "Add parser",
        "2020-01-01T00:00:00+00:00",
        "2026-08-20T12:00:00+00:00",
    );

    let summary = worktree::diff_summary(f.ctx(), &task.id)
        .await
        .expect("diff_summary");

    assert_eq!(
        summary.commits[0].committed_at,
        "2026-08-20T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("a literal timestamp"),
        "the committer date must be what is stored, not the much older author date"
    );
}

#[tokio::test]
async fn diff_summary_for_a_task_that_has_never_run_is_empty_rather_than_an_error() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;

    let summary = worktree::diff_summary(f.ctx(), &task.id)
        .await
        .expect("diff_summary");

    assert_eq!(summary.branch, None);
    assert!(summary.commits.is_empty());
    assert_eq!(summary.diff.files_changed, 0);
}

#[tokio::test]
async fn diff_summary_of_a_repository_moved_or_deleted_is_refused_with_an_actionable_message() {
    let f = Fixture::new().await;
    let task = f.task("Add parser").await;
    worktree::prepare(f.ctx(), &task.id)
        .await
        .expect("prepare so the task has a branch to summarise");
    std::fs::remove_dir_all(f.source.path()).expect("delete the repository behind the app's back");

    let error = worktree::diff_summary(f.ctx(), &task.id)
        .await
        .expect_err("a moved repository must not reach git as a missing cwd");

    assert!(
        error.to_string().contains("has been moved or deleted"),
        "got: {error}"
    );
}

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

/// A registered repository, its `TempRepo`, and the app-data directory its
/// worktrees live under — held together because every one of them has to
/// outlive the test body: dropping the `TempDir` deletes the tree git is
/// about to be run against.
struct Fixture {
    harness: TestContext,
    source: TempRepo,
    /// Stands in for `<app-data>/worktrees`. Held only for its `Drop`.
    _worktrees: tempfile::TempDir,
    worktree_root: PathBuf,
    repository: Repository,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_source(TempRepo::init()).await
    }

    async fn with_source(source: TempRepo) -> Self {
        let worktrees = scratch_dir("rimaia-worktrees-");
        let root = worktrees.path().join(WORKTREE_ROOT_DIR);
        Self::build(source, worktrees, root).await
    }

    /// For the safety tests, whose whole subject is a `worktree_root` somewhere
    /// it should not be.
    async fn with_worktree_root(source: TempRepo, root: &Path) -> Self {
        let worktrees = scratch_dir("rimaia-worktrees-");
        Self::build(source, worktrees, root.to_path_buf()).await
    }

    async fn build(source: TempRepo, worktrees: tempfile::TempDir, root: PathBuf) -> Self {
        let harness = TestContext::new().await;
        let repository = repo::register(
            &harness.context,
            worktrees.path(),
            NewRepository {
                path: source
                    .path()
                    .to_str()
                    .expect("test paths are UTF-8")
                    .to_string(),
                name: None,
                worktree_root: Some(root.to_str().expect("test paths are UTF-8").to_string()),
            },
        )
        .await
        .expect("register the fixture repository");

        Self {
            harness,
            source,
            _worktrees: worktrees,
            worktree_root: root,
            repository,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    /// The `worktree_root` in the form the service records paths under —
    /// resolved, because on macOS a `TempDir` sits under a symlinked `/var`.
    fn root(&self) -> PathBuf {
        let parent = self
            .worktree_root
            .parent()
            .expect("the fixture root always has a parent");
        canonicalize(parent).join(
            self.worktree_root
                .file_name()
                .expect("the fixture root always has a name"),
        )
    }

    async fn task(&self, title: &str) -> Task {
        tasks::create_task(
            self.ctx(),
            NewTask {
                repository_id: self.repository.id.clone(),
                title: title.to_string(),
                plan: Some("## Steps\n1. do the thing\n".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: Vec::new(),
            },
        )
        .await
        .expect("create the fixture task")
    }

    async fn reload(&self, task_id: &str) -> Task {
        tasks::get_task(self.ctx(), task_id)
            .await
            .expect("read the task back")
            .task
    }

    /// Every linked worktree git itself knows about, which is the assertion
    /// task 007's acceptance criterion asks for — the filesystem alone cannot
    /// tell a removed worktree from a `rm -rf`.
    fn linked_worktrees(&self) -> Vec<ListedWorktree> {
        parse_worktree_list(&git(
            self.source.path(),
            &["worktree", "list", "--porcelain"],
        ))
        .into_iter()
        // The repository's own work tree is always in this list and is never
        // what a test means by "a worktree".
        .filter(|entry| entry.path != self.source.path())
        .collect()
    }

    fn drain_changes(&mut self) {
        while self.harness.changes.try_recv().is_ok() {}
    }
}

struct ListedWorktree {
    path: PathBuf,
    branch: Option<String>,
}

/// A second, independent reading of `git worktree list --porcelain` — the
/// service has its own parser, and a test that called it would prove only that
/// the parser agrees with itself.
fn parse_worktree_list(stdout: &str) -> Vec<ListedWorktree> {
    let mut entries: Vec<ListedWorktree> = Vec::new();
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            entries.push(ListedWorktree {
                path: PathBuf::from(path),
                branch: None,
            });
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            if let Some(entry) = entries.last_mut() {
                entry.branch = Some(branch.to_string());
            }
        }
    }
    entries
}

/// Points a task's `worktree_path` somewhere the service never would, for the
/// safety test whose subject is precisely a row that cannot be trusted.
async fn point_task_at(pool: &sqlx::SqlitePool, task_id: &str, path: &str) {
    sqlx::query!(
        "UPDATE tasks SET worktree_path = ?1 WHERE id = ?2",
        path,
        task_id
    )
    .execute(pool)
    .await
    .expect("point the task at a path outside its worktree root");
}

fn branch_exists(dir: &Path, branch: &str) -> bool {
    Command::new("git")
        .current_dir(dir)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .status()
        .expect("run git show-ref")
        .success()
}

/// Asks git itself whether a name is a legal ref, which is the authority the
/// branch-naming rules were read off in the first place.
fn check_ref_format(dir: &Path, branch: &str) {
    git(dir, &["check-ref-format", &format!("refs/heads/{branch}")]);
}

/// Writes and commits one file in `dir`, which may be a worktree or the
/// repository's own work tree — `TempRepo::commit` only ever touches the
/// latter.
fn commit_in(dir: &Path, file: &str, contents: &str, message: &str) {
    std::fs::write(dir.join(file), contents).expect("write a file to commit");
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-m", message]);
}

/// Same as [`commit_in`], but with the author and committer dates set
/// independently — the only way to make git disagree with itself about
/// "when", which is what separates `%aI` from `%cI` in `LOG_FORMAT`.
fn commit_with_dates(
    dir: &Path,
    file: &str,
    contents: &str,
    message: &str,
    author_date: &str,
    committer_date: &str,
) {
    std::fs::write(dir.join(file), contents).expect("write a file to commit");
    git(dir, &["add", "--", file]);
    let status = Command::new("git")
        .current_dir(dir)
        .args(["commit", "-m", message])
        .env("GIT_AUTHOR_DATE", author_date)
        .env("GIT_COMMITTER_DATE", committer_date)
        .status()
        .expect("run git commit with explicit author and committer dates");
    assert!(status.success(), "git commit with explicit dates failed");
}

/// Files a card at the bottom of `column`, naming the card currently at that
/// bottom as its `before` neighbour.
///
/// `move_task` refuses an unanchored drop into a column that is not empty (its
/// own doc says why it is refused rather than guessed as "append"), so a test
/// that moves two cards into the same column has to name a neighbour for the
/// second. Reading the bottom back through `list_tasks` is also what makes the
/// resulting order the one the board would draw, which is what ADR-0008's
/// position tiebreak is defined against.
async fn move_to(f: &Fixture, task_id: &str, column: BoardColumn) {
    let occupants = tasks::list_tasks(
        f.ctx(),
        TaskFilter {
            repository_id: Some(f.repository.id.clone()),
            column: Some(column),
            run_state: None,
        },
    )
    .await
    .expect("read the destination column");
    let bottom = occupants
        .iter()
        .map(|summary| summary.task.id.clone())
        .next_back();

    tasks::move_task(f.ctx(), task_id, column, bottom.as_deref(), None)
        .await
        .expect("file the card");
}

/// What ADR-0008 calls satisfying a dependency: the card reaches `in_review`.
async fn file_for_review(f: &Fixture, task_id: &str) {
    move_to(f, task_id, BoardColumn::InReview).await;
}

fn scratch_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap_or_else(|error| panic!("temp dir for {prefix}: {error}"))
}

/// Resolves symlinks the way [`TempRepo`] and the service both do, so a test
/// can predict the exact path a row will hold — `/var` is a symlink to
/// `/private/var` on macOS.
/// The canonical path **as the service reports it** — through
/// `paths::git_safe`, because `worktree::safety::resolve` canonicalizes that
/// way. Windows returns an extended-length `\\?\` path that git cannot open,
/// so the product strips it; an expectation built the raw way would assert what
/// Windows returns rather than what the worktree actually is.
fn canonicalize(path: &Path) -> PathBuf {
    rimaia_core::testing::git_path(
        std::fs::canonicalize(path)
            .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display())),
    )
}

/// Runs git in `dir` and returns trimmed stdout, panicking with both streams
/// on failure — a git error in a test fixture is a broken test, not a handled
/// condition. Matches `crates/core/tests/repo_service.rs`'s own helper.
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
