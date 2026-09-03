//! [`rimaia_core::worktree::cleanup`] against real git repositories (task 016,
//! ADR-0005, ADR-0015, ADR-0022, seam-contract D19).
//!
//! Nothing here is mocked, for the reason `tests/worktree.rs` gives at length:
//! a mocked git proves the mock works. Every repository is a real one built by
//! `TempRepo`, every worktree is a real `git worktree add`, and every claim
//! about what survived a removal is checked against git itself — `git worktree
//! list --porcelain` for the administrative record, `git show-ref` for the
//! branch — because those are the two things a plain `rm -rf` gets wrong and
//! the two things task 016's acceptance criteria are about.
//!
//! **Both paths contain a space on purpose**, exactly as in `tests/worktree.rs`:
//! `TempRepo`'s work tree is `work tree` and every fixture's worktree root is
//! `my repo`, so an argument vector that ever became a shell string fails here
//! rather than on somebody's `~/Documents/My Projects/...`.
//!
//! # Why so many of these push first
//!
//! A Rimaia branch is unpushed by definition until somebody merges the PR, so
//! the unpushed-commits guard fires on almost any worktree with work in it.
//! That is the guard doing its job, and it means a test about some *other*
//! guard has to get past it — by having no commits beyond the base, or by
//! pushing to the `TempRepo`'s bare `origin`. Which of the two a test picks is
//! itself information about what it is testing, so it is never hidden in a
//! helper.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, Repository, RunState, Task};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::worktree::{
    self, AutoCleanup, BranchDisposition, ForceRemoval, RemovalAuthorization,
};
use rimaia_core::ServiceContext;

/// The last component of every fixture's `worktree_root`. The space is
/// load-bearing — see the module docs.
const WORKTREE_ROOT_DIR: &str = "my repo";

// ---------------------------------------------------------------------------
// The inventory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_inventory_reports_a_worktrees_size_branch_and_merged_state() {
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    let inventory = worktree::inventory(f.ctx()).await.expect("inventory");

    assert_eq!(inventory.entries.len(), 1);
    let entry = &inventory.entries[0];
    assert_eq!(entry.task_id, task.id);
    assert_eq!(entry.task_title, "Add the parser");
    assert_eq!(entry.repository_name, f.repository.name);
    assert_eq!(entry.branch.as_deref(), Some(worktree.branch.as_str()));
    assert_eq!(entry.base_ref, "main");
    assert!(entry.exists);
    assert!(
        entry.size_bytes > 0,
        "a worktree is a real checkout, so it costs real bytes",
    );
    assert!(
        entry.last_activity.is_some(),
        "a freshly checked-out file has an mtime to read",
    );
    assert!(
        entry.merged,
        "a branch with no commits of its own is already contained by its base",
    );
    assert_eq!(entry.uncommitted_changes, 0);
    assert_eq!(entry.unpushed_commits, 0);
    assert!(!entry.live);
    assert_eq!(inventory.total_bytes, entry.size_bytes);
}

#[tokio::test]
async fn the_inventory_counts_uncommitted_and_unpushed_work_separately() {
    // The two overridable guards read these two numbers, and they are genuinely
    // different quantities: one file that was never committed, one commit that
    // was never pushed.
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    let checkout = PathBuf::from(&worktree.path);
    commit_in(&checkout, "parser.rs", "// parser\n", "Add the parser");
    std::fs::write(checkout.join("scratch.txt"), "notes\n").expect("write an uncommitted file");

    let inventory = worktree::inventory(f.ctx()).await.expect("inventory");
    let entry = &inventory.entries[0];

    assert_eq!(entry.uncommitted_changes, 1);
    assert_eq!(entry.unpushed_commits, 1);
    assert!(!entry.merged, "a branch with a commit of its own is not");
}

#[tokio::test]
async fn a_task_with_no_worktree_is_not_in_the_inventory_at_all() {
    // Driven from `tasks.worktree_path`, not from `git worktree list` — see
    // `cleanup::inventory`'s doc on why listing would surface the user's own
    // worktrees and offer to delete them.
    let f = Fixture::new().await;
    f.task("Never run").await;

    let inventory = worktree::inventory(f.ctx()).await.expect("inventory");

    assert!(inventory.entries.is_empty());
    assert_eq!(inventory.total_bytes, 0);
}

#[tokio::test]
async fn a_running_task_is_listed_as_live_so_the_button_can_be_disabled() {
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    f.set_run_state(&task.id, RunState::Running).await;

    let inventory = worktree::inventory(f.ctx()).await.expect("inventory");

    assert!(inventory.entries[0].live);
    assert_eq!(inventory.entries[0].run_state, RunState::Running);
}

// ---------------------------------------------------------------------------
// Removing one, and what it frees
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cleanup_frees_the_expected_disk_and_leaves_no_stale_worktree_entries() {
    // Task 016's first acceptance criterion, both halves. The size the
    // inventory promised is the size the removal reports, and git's own
    // administrative record under `.git/worktrees/` goes with the directory —
    // which is precisely what `rm -rf` would not do, and why this asserts
    // against `git worktree list --porcelain` rather than against the
    // filesystem.
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    let checkout = PathBuf::from(&worktree.path);

    let promised = worktree::inventory(f.ctx())
        .await
        .expect("inventory")
        .entries[0]
        .size_bytes;
    assert_eq!(f.linked_worktrees().len(), 1);

    let removed = worktree::remove_worktree(f.ctx(), &task.id, RemovalAuthorization::default())
        .await
        .expect("a clean, pushed-up-to-date worktree removes without any force");

    assert_eq!(removed.bytes_freed, promised);
    assert_eq!(removed.branch_deleted, None);
    assert!(!checkout.exists(), "the directory is gone");
    assert!(
        f.linked_worktrees().is_empty(),
        "and so is git's administrative record of it",
    );
    assert_eq!(
        worktree::inventory(f.ctx())
            .await
            .expect("inventory")
            .total_bytes,
        0,
        "the reported total agrees with what was actually freed",
    );
    assert_eq!(f.reload(&task.id).await.worktree_path, None);
}

#[tokio::test]
async fn removing_a_worktree_for_a_task_that_has_none_is_not_an_error() {
    // Idempotent for `worktree::remove`'s reason: the state the caller wanted
    // is the state it is in.
    let f = Fixture::new().await;
    let task = f.task("Never run").await;

    let removed = worktree::remove_worktree(f.ctx(), &task.id, RemovalAuthorization::default())
        .await
        .expect("nothing to remove is not a failure");

    assert_eq!(removed.bytes_freed, 0);
}

// ---------------------------------------------------------------------------
// The guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_worktree_with_uncommitted_changes_is_refused_until_forced_and_the_count_is_in_the_message(
) {
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    let checkout = PathBuf::from(&worktree.path);
    std::fs::write(checkout.join("first.txt"), "one\n").expect("write");
    std::fs::write(checkout.join("second.txt"), "two\n").expect("write");
    std::fs::write(checkout.join("third.txt"), "three\n").expect("write");

    let refusal = worktree::remove_worktree(f.ctx(), &task.id, RemovalAuthorization::default())
        .await
        .expect_err("uncommitted work is refused");

    let message = refusal.to_string();
    assert!(
        message.contains("3 uncommitted changes"),
        "the count is the part that lets a user decide, and it must be in the sentence: {message}",
    );
    assert!(message.contains("Add the parser"), "{message}");
    assert!(checkout.exists(), "and nothing was removed");

    // Forced, and only forced, it goes.
    worktree::remove_worktree(
        f.ctx(),
        &task.id,
        RemovalAuthorization {
            uncommitted_changes: ForceRemoval::ConfirmedByUser,
            ..RemovalAuthorization::default()
        },
    )
    .await
    .expect("an explicit confirmation removes it");

    assert!(!checkout.exists());
    assert!(f.linked_worktrees().is_empty());
}

#[tokio::test]
async fn one_uncommitted_change_is_refused_in_the_singular() {
    // Not pedantry: this message is the whole of what the user has to go on,
    // and "1 uncommitted changes" reads as a bug in the thing about to delete
    // their work.
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    std::fs::write(PathBuf::from(&worktree.path).join("only.txt"), "one\n").expect("write");

    let refusal = worktree::remove_worktree(f.ctx(), &task.id, RemovalAuthorization::default())
        .await
        .expect_err("uncommitted work is refused");

    let message = refusal.to_string();
    assert!(message.contains("1 uncommitted change in"), "{message}");
    assert!(message.contains("discard it for good"), "{message}");
}

#[tokio::test]
async fn a_worktree_with_unpushed_commits_is_refused_until_forced() {
    let f = Fixture::with_source(TempRepo::init().with_remote()).await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    let checkout = PathBuf::from(&worktree.path);
    commit_in(&checkout, "parser.rs", "// parser\n", "Add the parser");

    let refusal = worktree::remove_worktree(f.ctx(), &task.id, RemovalAuthorization::default())
        .await
        .expect_err("a commit no remote has is refused");

    let message = refusal.to_string();
    assert!(message.contains("1 commit on"), "{message}");
    assert!(message.contains("no remote has"), "{message}");
    assert!(checkout.exists());

    worktree::remove_worktree(
        f.ctx(),
        &task.id,
        RemovalAuthorization {
            unpushed_commits: ForceRemoval::ConfirmedByUser,
            ..RemovalAuthorization::default()
        },
    )
    .await
    .expect("an explicit confirmation removes it");

    assert!(!checkout.exists());
    assert!(
        branch_exists(f.source.path(), &worktree.branch),
        "and the commit survives on the branch, which is the point of keeping it",
    );
}

#[tokio::test]
async fn a_pushed_branch_needs_no_force_at_all() {
    // The other side of the guard above: once `origin` has the commits, there
    // is nothing left that only this disk holds, and the default authorization
    // is enough.
    let f = Fixture::with_source(TempRepo::init().with_remote()).await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    let checkout = PathBuf::from(&worktree.path);
    commit_in(&checkout, "parser.rs", "// parser\n", "Add the parser");
    git(&checkout, &["push", "origin", &worktree.branch]);

    worktree::remove_worktree(f.ctx(), &task.id, RemovalAuthorization::default())
        .await
        .expect("nothing here exists in only one place");

    assert!(!checkout.exists());
}

#[tokio::test]
async fn a_running_task_keeps_its_worktree_even_when_forced() {
    // **The guard with no override.** Every force named, and it still refuses:
    // a Claude Code process is writing in that directory, and there is no
    // answer to "are you sure?" that makes pulling it out from under one a good
    // idea.
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    f.set_run_state(&task.id, RunState::Running).await;

    let refusal = worktree::remove_worktree(
        f.ctx(),
        &task.id,
        RemovalAuthorization {
            uncommitted_changes: ForceRemoval::ConfirmedByUser,
            unpushed_commits: ForceRemoval::ConfirmedByUser,
            branch: BranchDisposition::DeleteEvenIfUnmerged,
        },
    )
    .await
    .expect_err("a running task keeps its worktree whatever the caller confirms");

    let message = refusal.to_string();
    assert!(message.contains("no way to force this one"), "{message}");
    assert!(PathBuf::from(&worktree.path).exists());
    assert_eq!(f.linked_worktrees().len(), 1);
    assert!(branch_exists(f.source.path(), &worktree.branch));
}

#[tokio::test]
async fn a_waiting_retry_task_keeps_its_worktree_even_when_forced() {
    // `waiting_retry` means "a process is about to be writing in there", and
    // the gap before the next attempt is not a window in which the directory is
    // spare — `worktree::correct_run_state` treats the two states alike for the
    // same reason.
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    f.set_run_state(&task.id, RunState::Running).await;
    f.set_run_state(&task.id, RunState::WaitingRetry).await;

    let refusal = worktree::remove_worktree(
        f.ctx(),
        &task.id,
        RemovalAuthorization {
            uncommitted_changes: ForceRemoval::ConfirmedByUser,
            unpushed_commits: ForceRemoval::ConfirmedByUser,
            branch: BranchDisposition::DeleteEvenIfUnmerged,
        },
    )
    .await
    .expect_err("a task waiting to retry keeps its worktree whatever the caller confirms");

    assert!(refusal.to_string().contains("waiting to retry"));
    assert!(PathBuf::from(&worktree.path).exists());
}

// ---------------------------------------------------------------------------
// The branch is a separate decision
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unmerged_branch_survives_a_worktree_removal_that_did_not_ask_for_it() {
    // ADR-0005: "the branch is left alone unless the user asks for it to go."
    // The branch is what holds everything the run committed, so a removal that
    // took it silently would turn "reclaim some disk" into "throw away the
    // work" — with no separate moment at which anybody said so.
    let f = Fixture::with_source(TempRepo::init().with_remote()).await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    let checkout = PathBuf::from(&worktree.path);
    commit_in(&checkout, "parser.rs", "// parser\n", "Add the parser");
    let tip = git(&checkout, &["rev-parse", "HEAD"]);

    worktree::remove_worktree(
        f.ctx(),
        &task.id,
        RemovalAuthorization {
            unpushed_commits: ForceRemoval::ConfirmedByUser,
            // `branch` deliberately left at its default.
            ..RemovalAuthorization::default()
        },
    )
    .await
    .expect("remove the worktree");

    assert!(!checkout.exists());
    assert!(
        branch_exists(f.source.path(), &worktree.branch),
        "the unmerged branch outlives its worktree",
    );
    assert_eq!(
        git(f.source.path(), &["rev-parse", &worktree.branch]),
        tip,
        "with every commit still on it",
    );
    assert_eq!(
        f.reload(&task.id).await.branch.as_deref(),
        Some(worktree.branch.as_str()),
        "and the row still points at it, so the work is not orphaned from the card",
    );
}

#[tokio::test]
async fn deleting_an_unmerged_branch_is_refused_without_the_second_confirmation() {
    // Task 016: "never delete a branch that is not merged, without a separate
    // confirmation" — separate from the one that authorised removing the
    // worktree, which this call has already given.
    let f = Fixture::with_source(TempRepo::init().with_remote()).await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        &PathBuf::from(&worktree.path),
        "parser.rs",
        "// parser\n",
        "Add the parser",
    );

    let refusal = worktree::remove_worktree(
        f.ctx(),
        &task.id,
        RemovalAuthorization {
            unpushed_commits: ForceRemoval::ConfirmedByUser,
            branch: BranchDisposition::DeleteIfMerged,
            ..RemovalAuthorization::default()
        },
    )
    .await
    .expect_err("an unmerged branch is not deleted by a confirmation about the worktree");

    let message = refusal.to_string();
    assert!(message.contains("is not merged into main"), "{message}");
    assert!(message.contains("separate decision"), "{message}");
    assert!(branch_exists(f.source.path(), &worktree.branch));
    assert!(
        PathBuf::from(&worktree.path).exists(),
        "the guard runs before anything is removed, so the refusal leaves no half-done state",
    );
}

#[tokio::test]
async fn a_merged_branch_is_deleted_when_the_caller_asked_for_that() {
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    let removed = worktree::remove_worktree(
        f.ctx(),
        &task.id,
        RemovalAuthorization {
            branch: BranchDisposition::DeleteIfMerged,
            ..RemovalAuthorization::default()
        },
    )
    .await
    .expect("a branch main already contains is safe to delete");

    assert_eq!(
        removed.branch_deleted.as_deref(),
        Some(worktree.branch.as_str())
    );
    assert!(!branch_exists(f.source.path(), &worktree.branch));
    assert_eq!(f.reload(&task.id).await.branch, None);
}

#[tokio::test]
async fn an_unmerged_branch_goes_only_on_the_confirmation_named_for_it() {
    let f = Fixture::with_source(TempRepo::init().with_remote()).await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    commit_in(
        &PathBuf::from(&worktree.path),
        "parser.rs",
        "// parser\n",
        "Add the parser",
    );

    let removed = worktree::remove_worktree(
        f.ctx(),
        &task.id,
        RemovalAuthorization {
            unpushed_commits: ForceRemoval::ConfirmedByUser,
            branch: BranchDisposition::DeleteEvenIfUnmerged,
            ..RemovalAuthorization::default()
        },
    )
    .await
    .expect("the confirmation that names unmerged branches deletes one");

    assert_eq!(
        removed.branch_deleted.as_deref(),
        Some(worktree.branch.as_str())
    );
    assert!(!branch_exists(f.source.path(), &worktree.branch));
}

// ---------------------------------------------------------------------------
// The bulk actions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cleaning_up_done_tasks_leaves_every_other_column_alone() {
    let f = Fixture::new().await;
    let finished = f.task("Add the parser").await;
    let still_going = f.task("Wire the board").await;
    let finished_tree = worktree::prepare(f.ctx(), &finished.id)
        .await
        .expect("prepare");
    let other_tree = worktree::prepare(f.ctx(), &still_going.id)
        .await
        .expect("prepare");
    f.move_to_done(&finished.id).await;

    let report = worktree::remove_done_worktrees(f.ctx())
        .await
        .expect("cleanup");

    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].task_id, finished.id);
    assert!(report.refused.is_empty());
    assert!(report.bytes_freed > 0);
    assert!(!PathBuf::from(&finished_tree.path).exists());
    assert!(
        PathBuf::from(&other_tree.path).exists(),
        "a task still in `ready` keeps its worktree",
    );
}

#[tokio::test]
async fn a_bulk_cleanup_reports_what_it_refused_instead_of_stopping_at_it() {
    // The reason a bulk action returns a report rather than a `Result`: one
    // dirty worktree must not cost the user the nine clean ones, and it must
    // not vanish silently either.
    let f = Fixture::new().await;
    let clean = f.task("Add the parser").await;
    let dirty = f.task("Wire the board").await;
    let clean_tree = worktree::prepare(f.ctx(), &clean.id)
        .await
        .expect("prepare");
    let dirty_tree = worktree::prepare(f.ctx(), &dirty.id)
        .await
        .expect("prepare");
    std::fs::write(
        PathBuf::from(&dirty_tree.path).join("scratch.txt"),
        "notes\n",
    )
    .expect("write an uncommitted file");
    f.move_to_done(&clean.id).await;
    f.move_to_done(&dirty.id).await;

    let report = worktree::remove_done_worktrees(f.ctx())
        .await
        .expect("cleanup");

    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].task_id, clean.id);
    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].task_id, dirty.id);
    assert_eq!(report.refused[0].task_title, "Wire the board");
    assert!(
        report.refused[0].reason.contains("1 uncommitted change"),
        "the bulk refusal carries the same sentence the individual one would: {}",
        report.refused[0].reason,
    );
    assert!(!PathBuf::from(&clean_tree.path).exists());
    assert!(PathBuf::from(&dirty_tree.path).exists());
}

#[tokio::test]
async fn a_bulk_cleanup_never_deletes_a_branch_even_a_merged_one() {
    // A single click standing in for N decisions may not carry more authority
    // than the user would have granted one at a time — and deleting a branch is
    // never something they granted by asking for disk back.
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    f.move_to_done(&task.id).await;

    let report = worktree::remove_done_worktrees(f.ctx())
        .await
        .expect("cleanup");

    assert_eq!(report.removed[0].branch_deleted, None);
    assert!(branch_exists(f.source.path(), &worktree.branch));
}

#[tokio::test]
async fn cleaning_up_merged_branches_spares_one_with_commits_of_its_own() {
    let f = Fixture::with_source(TempRepo::init().with_remote()).await;
    let merged = f.task("Add the parser").await;
    let diverged = f.task("Wire the board").await;
    let merged_tree = worktree::prepare(f.ctx(), &merged.id)
        .await
        .expect("prepare");
    let diverged_tree = worktree::prepare(f.ctx(), &diverged.id)
        .await
        .expect("prepare");
    commit_in(
        &PathBuf::from(&diverged_tree.path),
        "board.rs",
        "// board\n",
        "Wire the board",
    );

    let report = worktree::remove_merged_worktrees(f.ctx())
        .await
        .expect("cleanup");

    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].task_id, merged.id);
    assert!(!PathBuf::from(&merged_tree.path).exists());
    assert!(
        PathBuf::from(&diverged_tree.path).exists(),
        "a branch main does not contain is not one this action is about",
    );
}

// ---------------------------------------------------------------------------
// The auto-removal policy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auto_cleanup_is_off_by_default() {
    // Task 016's acceptance criterion, against a database nobody has
    // configured: there is no seeded row, and an absent key *is* `Off`.
    let f = Fixture::new().await;

    assert_eq!(
        worktree::auto_cleanup(&f.ctx().pool)
            .await
            .expect("read the policy"),
        AutoCleanup::Off
    );
}

#[tokio::test]
async fn moving_a_task_to_done_keeps_its_worktree_while_the_policy_is_off() {
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    f.move_to_done(&task.id).await;

    assert!(
        PathBuf::from(&worktree.path).exists(),
        "off by default means the default board move deletes nothing",
    );
    assert_eq!(
        f.reload(&task.id).await.worktree_path.as_deref(),
        Some(worktree.path.as_str())
    );
}

#[tokio::test]
async fn enabling_the_policy_removes_the_worktree_when_the_card_reaches_done() {
    let f = Fixture::new().await;
    worktree::set_auto_cleanup(f.ctx(), AutoCleanup::OnDoneAcknowledged)
        .await
        .expect("enable auto cleanup");
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");

    f.move_to_done(&task.id).await;

    assert!(!PathBuf::from(&worktree.path).exists());
    assert!(
        f.linked_worktrees().is_empty(),
        "git's administrative record goes too, the same as a manual removal",
    );
    assert_eq!(f.reload(&task.id).await.worktree_path, None);
}

#[tokio::test]
async fn auto_removal_never_deletes_a_branch_and_never_forces() {
    // An automatic action gets strictly less authority than a human clicking a
    // button, because there is nobody there to read the refusal it would
    // otherwise be overriding. Both halves are asserted at once: the dirty
    // worktree survives (no force), and so does its branch.
    let f = Fixture::new().await;
    worktree::set_auto_cleanup(f.ctx(), AutoCleanup::OnDoneAcknowledged)
        .await
        .expect("enable auto cleanup");
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    let checkout = PathBuf::from(&worktree.path);
    std::fs::write(checkout.join("scratch.txt"), "notes\n").expect("write an uncommitted file");

    let moved = f.move_to_done(&task.id).await;

    assert_eq!(
        moved.column,
        BoardColumn::Done,
        "the move itself succeeded — a refused cleanup must never report it as failed",
    );
    assert!(
        checkout.exists(),
        "automatic removal does not force past uncommitted work",
    );
    assert!(branch_exists(f.source.path(), &worktree.branch));

    // And with the work committed and the branch merged into main, the same
    // move does remove it — while *still* keeping the branch.
    std::fs::remove_file(checkout.join("scratch.txt")).expect("clean the worktree");
    f.move_back_to_ready(&task.id).await;
    f.move_to_done(&task.id).await;

    assert!(!checkout.exists());
    assert!(
        branch_exists(f.source.path(), &worktree.branch),
        "the branch is kept even on the path where the worktree does go",
    );
}

#[tokio::test]
async fn automatic_removal_leaves_a_running_task_alone() {
    // The hard guard reached through the automatic door, which is the one place
    // it could plausibly be bypassed: nobody is watching, and the card is
    // moving for some other reason.
    let f = Fixture::new().await;
    worktree::set_auto_cleanup(f.ctx(), AutoCleanup::OnDoneAcknowledged)
        .await
        .expect("enable auto cleanup");
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    f.set_run_state(&task.id, RunState::Running).await;

    f.move_to_done(&task.id).await;

    assert!(PathBuf::from(&worktree.path).exists());
    assert_eq!(f.linked_worktrees().len(), 1);
}

// ---------------------------------------------------------------------------
// Reconciliation of a directory deleted outside the app
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_worktree_directory_deleted_outside_the_app_is_reconciled_at_startup() {
    // Task 016's fourth acceptance criterion, through the machinery task 007
    // already built — `worktree::reconcile`, which startup calls with
    // `survey`'s `missing_worktrees`. What task 016 adds is the *other* half of
    // the same fact: the inventory has to show such a worktree as gone rather
    // than quietly offering to delete it again, or the user's storage report
    // stays wrong until they restart.
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    std::fs::remove_dir_all(&worktree.path).expect("delete the worktree behind the app's back");

    let before = worktree::inventory(f.ctx()).await.expect("inventory");
    assert_eq!(before.entries.len(), 1);
    assert!(
        !before.entries[0].exists,
        "the row still records it, and the inventory says the disk does not",
    );
    assert_eq!(before.entries[0].size_bytes, 0);
    assert_eq!(before.total_bytes, 0);

    let reconciled = worktree::reconcile(f.ctx(), std::slice::from_ref(&task.id)).await;

    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].cleared_path, worktree.path);
    assert_eq!(
        reconciled[0].retained_branch.as_deref(),
        Some(worktree.branch.as_str()),
        "the branch outlived the directory and still holds whatever was committed",
    );
    assert!(
        worktree::inventory(f.ctx())
            .await
            .expect("inventory")
            .entries
            .is_empty(),
        "and once the row is cleared there is nothing left to list",
    );
    assert!(
        f.linked_worktrees().is_empty(),
        "reconciliation prunes git's stale administrative entry too",
    );
}

#[tokio::test]
async fn a_worktree_whose_directory_vanished_is_removable_without_any_force() {
    // Nothing on disk is nothing to lose, so the uncommitted-changes guard has
    // nothing to protect — and asking git about a directory that is not there
    // would fail rather than refuse.
    let f = Fixture::new().await;
    let task = f.task("Add the parser").await;
    let worktree = worktree::prepare(f.ctx(), &task.id).await.expect("prepare");
    std::fs::remove_dir_all(&worktree.path).expect("delete the worktree");

    worktree::remove_worktree(f.ctx(), &task.id, RemovalAuthorization::default())
        .await
        .expect("a directory that is already gone removes cleanly");

    assert_eq!(f.reload(&task.id).await.worktree_path, None);
    assert!(f.linked_worktrees().is_empty());
}

// ---------------------------------------------------------------------------
// Fixture — the same shape `tests/worktree.rs` uses
// ---------------------------------------------------------------------------

struct Fixture {
    harness: TestContext,
    source: TempRepo,
    /// Stands in for `<app-data>/worktrees`. Held only for its `Drop`.
    _worktrees: tempfile::TempDir,
    repository: Repository,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_source(TempRepo::init()).await
    }

    async fn with_source(source: TempRepo) -> Self {
        let worktrees = tempfile::Builder::new()
            .prefix("rimaia-worktrees-")
            .tempdir()
            .expect("temp dir for the worktree root");
        let root = worktrees.path().join(WORKTREE_ROOT_DIR);
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
            repository,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
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

    /// Through `tasks::move_task`, never through an `UPDATE`, because the
    /// auto-removal hook this file is largely about lives *in* that function —
    /// a test that wrote the column directly would assert about a code path
    /// nothing in the app takes.
    async fn move_to_done(&self, task_id: &str) -> Task {
        // Naming the bottom-of-column neighbour, the same thing
        // `RimaiaServer::bottom_of_column` does for MCP callers:
        // `tasks::move_task` requires one unless the destination is empty, and
        // that rule is not relaxed for a test any more than it is for an
        // adapter.
        let after = self.bottom_of(BoardColumn::Done, task_id).await;
        tasks::move_task(
            self.ctx(),
            task_id,
            BoardColumn::Done,
            None,
            after.as_deref(),
        )
        .await
        .expect("move the task to done")
    }

    async fn bottom_of(&self, column: BoardColumn, excluding: &str) -> Option<String> {
        tasks::list_tasks(
            self.ctx(),
            tasks::TaskFilter {
                repository_id: Some(self.repository.id.clone()),
                column: Some(column),
                run_state: None,
            },
        )
        .await
        .expect("list the destination column")
        .into_iter()
        .map(|summary| summary.task.id)
        .rfind(|id| id != excluding)
    }

    async fn move_back_to_ready(&self, task_id: &str) {
        let after = self.bottom_of(BoardColumn::Ready, task_id).await;
        tasks::move_task(
            self.ctx(),
            task_id,
            BoardColumn::Ready,
            None,
            after.as_deref(),
        )
        .await
        .expect("move the task back to ready");
    }

    /// Walks the state machine rather than jumping, because
    /// `tasks::set_run_state` is its only writer and rejects an illegal edge —
    /// `Idle -> Running` among them.
    async fn set_run_state(&self, task_id: &str, target: RunState) {
        let path = match target {
            RunState::Running => vec![RunState::Queued, RunState::Running],
            RunState::WaitingRetry => vec![RunState::WaitingRetry],
            other => vec![other],
        };
        for state in path {
            tasks::set_run_state(self.ctx(), task_id, state)
                .await
                .unwrap_or_else(|error| panic!("move the task to {state:?}: {error}"));
        }
    }

    /// Every linked worktree git itself knows about — the filesystem alone
    /// cannot tell a removed worktree from an `rm -rf`, and task 016's first
    /// acceptance criterion is precisely about the difference.
    fn linked_worktrees(&self) -> Vec<PathBuf> {
        git(self.source.path(), &["worktree", "list", "--porcelain"])
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .filter(|path| path != self.source.path())
            .collect()
    }
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

/// Writes and commits one file in `dir`, which here is always a worktree —
/// `TempRepo::commit` only ever touches the repository's own work tree.
fn commit_in(dir: &Path, file: &str, contents: &str, message: &str) {
    std::fs::write(dir.join(file), contents).expect("write a file to commit");
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-m", message]);
}

/// Runs git in `dir` and returns trimmed stdout, panicking with both streams on
/// failure — a git error in a test fixture is a broken test, not a handled
/// condition. Matches `tests/worktree.rs`'s own helper.
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
