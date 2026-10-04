//! What a run left behind, recorded at its finish and read back without git
//! (task 033, ADR-0033 points 4, 5 and 7, ADR-0036).
//!
//! Three groups, in the order the data flows:
//!
//! 1. **Capture**, against real repositories in a `TempDir`. Every expected
//!    patch is the one `git diff` itself prints for the same range with the
//!    pinned argument vector, spelled out again here rather than imported, so
//!    the test is a second reading of task 033's vector and not the constant
//!    agreeing with itself. A mocked git would only prove the mock works
//!    (ADR-0015).
//! 2. **End to end through `run_task`**, with `FakeCli` replaying recorded
//!    streams and committing real files in the real worktree, under the
//!    `TestClock`. Unix only, for `runner_process.rs`'s reason: the stand-in is
//!    a POSIX shell script.
//! 3. **Reading back.** `get_run` runs no git in any of these, and the ones
//!    that delete the worktree, the branch and the clone prove it: the live
//!    `diff_summary` errors at that point and `get_run` does not.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use pretty_assertions::assert_eq;
use rimaia_core::db::{BoardColumn, ExitClass, Run, RunState, RunStatus};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::events::TokenUsage;
use rimaia_core::runner::outcome::{finish_run, start_run, NewRun, RunOutcome, SpawnedAs};
use rimaia_core::runs::bundle::{PatchInclusion, ReviewBundle, RunCapture, PATCH_CAP_BYTES};
use rimaia_core::runs::{self, PruneCriterion, RunFilter, RunReview};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::worktree::{self, bundle, BranchDisposition, ForceRemoval, RemovalAuthorization};
use rimaia_core::{AppPaths, ErrorCode, ServiceContext};
use tempfile::TempDir;

/// Task 033's argument vector, spelled independently of `worktree::git`.
const PINNED: [&str; 8] = [
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--submodule=short",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--no-relative",
    "-M",
];

// ---------------------------------------------------------------------------
// Capture
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bundle_records_the_files_commits_and_patch_between_the_fork_point_and_head() {
    let f = Fixture::new().await;
    let worktree = worktree::prepare(f.ctx(), &f.task_id)
        .await
        .expect("prepare");
    let checkout = PathBuf::from(&worktree.path);
    commit_in(
        &checkout,
        "src/parser.rs",
        b"pub fn parse() {}\n",
        "Add the parser",
    );
    commit_in(&checkout, "README.md", b"# changed\n", "Rewrite the readme");
    // The base moves on after the fork, which a two-dot diff would report as
    // work the run undid.
    git(f.source.path(), &["switch", "-q", "main"]);
    commit_in(
        f.source.path(),
        "upstream.txt",
        b"later\n",
        "Upstream moves on",
    );

    let base_sha = worktree.base_sha.clone().expect("a fork point");
    let capture = bundle::capture(&checkout, Some(&base_sha)).await;
    let recorded = capture.bundle.expect("a branch with commits gets a bundle");
    let head_sha = capture.head_sha.expect("a head commit");

    assert_eq!(head_sha, git(&checkout, &["rev-parse", "HEAD"]));
    assert_eq!(
        recorded.patch.as_bytes(),
        reference_patch(&checkout, &base_sha, &head_sha),
        "byte-equal to git diff with the pinned vector over base_sha...head_sha"
    );
    assert!(!recorded.patch_truncated);

    let live = worktree::diff_summary(f.ctx(), &f.task_id)
        .await
        .expect("the live summary");
    assert_eq!(recorded.diff, live.diff);
    assert_eq!(
        recorded
            .files
            .iter()
            .map(|file| (file.path.clone(), file.insertions, file.deletions))
            .collect::<Vec<_>>(),
        live.files
            .iter()
            .map(|file| (file.path.clone(), file.insertions, file.deletions))
            .collect::<Vec<_>>(),
    );
    assert_eq!(recorded.commits, live.commits);
    assert_eq!(recorded.commits.len(), 2);
}

#[tokio::test]
async fn a_patch_over_the_cap_keeps_whole_files_in_order_and_omits_the_rest() {
    let repo = TempRepo::init();
    let base_sha = repo.head_sha();
    git(repo.path(), &["switch", "-q", "-c", "work"]);
    // 200 + 200 fit; the third outgrows what is left; the fourth still fits.
    commit_in(repo.path(), "a.txt", &text_of(200 * 1024, "a"), "Add a");
    commit_in(repo.path(), "b.txt", &text_of(200 * 1024, "b"), "Add b");
    commit_in(repo.path(), "c.txt", &text_of(200 * 1024, "c"), "Add c");
    commit_in(repo.path(), "d.txt", &text_of(50 * 1024, "d"), "Add d");
    let head_sha = repo.head_sha();

    let recorded = build(repo.path(), &base_sha, &head_sha).await;

    assert!(recorded.patch.len() <= PATCH_CAP_BYTES);
    assert_eq!(
        inclusions(&recorded),
        vec![
            ("a.txt".to_string(), PatchInclusion::Included),
            ("b.txt".to_string(), PatchInclusion::Included),
            ("c.txt".to_string(), PatchInclusion::TooLarge),
            ("d.txt".to_string(), PatchInclusion::Included),
        ]
    );
    assert!(recorded.patch_truncated);
    assert_included_sections_are_whole(repo.path(), &base_sha, &head_sha, &recorded);
    assert_applies_onto(repo.path(), &base_sha, &recorded.patch);
}

#[tokio::test]
async fn a_patch_with_a_binary_file_still_applies_as_a_whole() {
    let repo = TempRepo::init();
    let base_sha = repo.head_sha();
    git(repo.path(), &["switch", "-q", "-c", "work"]);
    commit_in(repo.path(), "a.txt", &text_of(300 * 1024, "a"), "Add a");
    commit_in(repo.path(), "b.png", &binary_bytes(), "Add a picture");
    commit_in(repo.path(), "c.txt", &text_of(300 * 1024, "c"), "Add c");
    commit_in(repo.path(), "d.txt", &text_of(10 * 1024, "d"), "Add d");
    let head_sha = repo.head_sha();

    let recorded = build(repo.path(), &base_sha, &head_sha).await;

    assert!(recorded.patch.len() <= PATCH_CAP_BYTES);
    assert_eq!(
        inclusions(&recorded),
        vec![
            ("a.txt".to_string(), PatchInclusion::Included),
            ("b.png".to_string(), PatchInclusion::Binary),
            ("c.txt".to_string(), PatchInclusion::TooLarge),
            ("d.txt".to_string(), PatchInclusion::Included),
        ]
    );
    assert!(!recorded.patch.contains("Binary files"));
    assert_included_sections_are_whole(repo.path(), &base_sha, &head_sha, &recorded);
    assert_applies_onto(repo.path(), &base_sha, &recorded.patch);
}

#[tokio::test]
async fn a_file_larger_than_the_whole_cap_is_omitted_and_later_files_still_fit() {
    let repo = TempRepo::init();
    let base_sha = repo.head_sha();
    git(repo.path(), &["switch", "-q", "-c", "work"]);
    // `package-lock.json` sorts before `src/`, which is the case the whole-file
    // rule exists for: a prefix cut would spend the budget on the lockfile.
    commit_in(
        repo.path(),
        "package-lock.json",
        &text_of(PATCH_CAP_BYTES + 64 * 1024, "lock"),
        "Regenerate the lockfile",
    );
    commit_in(repo.path(), "src/main.rs", b"fn main() {}\n", "Add main");
    let head_sha = repo.head_sha();

    let recorded = build(repo.path(), &base_sha, &head_sha).await;

    assert_eq!(
        inclusions(&recorded),
        vec![
            ("package-lock.json".to_string(), PatchInclusion::TooLarge),
            ("src/main.rs".to_string(), PatchInclusion::Included),
        ]
    );
    assert!(recorded.patch_truncated);
    assert!(recorded
        .patch
        .starts_with("diff --git a/src/main.rs b/src/main.rs\n"));
    assert_applies_onto(repo.path(), &base_sha, &recorded.patch);
}

#[tokio::test]
async fn patch_bytes_counts_the_whole_diff_not_the_stored_part() {
    let repo = TempRepo::init();
    let base_sha = repo.head_sha();
    git(repo.path(), &["switch", "-q", "-c", "work"]);
    commit_in(
        repo.path(),
        "big.txt",
        &text_of(PATCH_CAP_BYTES * 2, "big"),
        "Add big",
    );
    commit_in(repo.path(), "logo.png", &binary_bytes(), "Add a logo");
    commit_in(repo.path(), "small.txt", b"small\n", "Add small");
    let head_sha = repo.head_sha();

    let recorded = build(repo.path(), &base_sha, &head_sha).await;

    let whole = reference_patch(repo.path(), &base_sha, &head_sha);
    assert_eq!(recorded.patch_bytes, whole.len() as i64);
    assert!(recorded.patch_bytes > recorded.patch.len() as i64);
}

#[tokio::test]
async fn a_file_that_is_not_utf8_is_listed_but_left_out_of_the_patch() {
    let repo = TempRepo::init();
    let base_sha = repo.head_sha();
    git(repo.path(), &["switch", "-q", "-c", "work"]);
    // Latin-1 text: no NUL, so git treats it as text and counts its lines, but
    // it is not UTF-8 and the column is `TEXT`.
    commit_in(
        repo.path(),
        "latin1.txt",
        b"caf\xe9\nna\xefve\n",
        "Add Latin-1 text",
    );
    commit_in(
        repo.path(),
        "utf8.txt",
        "café\n".as_bytes(),
        "Add UTF-8 text",
    );
    let head_sha = repo.head_sha();

    let recorded = build(repo.path(), &base_sha, &head_sha).await;

    assert_eq!(
        inclusions(&recorded),
        vec![
            ("latin1.txt".to_string(), PatchInclusion::NotUtf8),
            ("utf8.txt".to_string(), PatchInclusion::Included),
        ]
    );
    assert_eq!(
        recorded.files[0].insertions,
        Some(2),
        "still counted as text"
    );
    assert!(!recorded.patch_truncated, "the cap cut nothing");
    assert_applies_onto(repo.path(), &base_sha, &recorded.patch);
}

#[tokio::test]
async fn a_binary_file_is_listed_with_no_line_counts() {
    let repo = TempRepo::init();
    let base_sha = repo.head_sha();
    git(repo.path(), &["switch", "-q", "-c", "work"]);
    commit_in(repo.path(), "logo.png", &binary_bytes(), "Add a logo");
    let head_sha = repo.head_sha();

    let recorded = build(repo.path(), &base_sha, &head_sha).await;

    assert_eq!(recorded.files.len(), 1);
    assert_eq!(recorded.files[0].path, "logo.png");
    assert_eq!(recorded.files[0].insertions, None);
    assert_eq!(recorded.files[0].deletions, None);
    assert_eq!(recorded.files[0].patch, PatchInclusion::Binary);
    assert_eq!(recorded.patch, "");
    assert!(!recorded.patch_truncated);
    assert_eq!(recorded.diff.files_changed, 1);
}

#[tokio::test]
async fn the_patch_ignores_the_operators_diff_config() {
    let repo = TempRepo::init().commit("notes.txt", "one\ntwo\n", "Add notes");
    let base_sha = repo.head_sha();
    git(repo.path(), &["switch", "-q", "-c", "work"]);
    commit_in(repo.path(), "notes.txt", b"one\nthree\n", "Change notes");
    commit_in(repo.path(), "new.txt", b"new\n", "Add new");
    let head_sha = repo.head_sha();
    let plain = build(repo.path(), &base_sha, &head_sha).await;

    for (key, value) in [
        ("color.ui", "always"),
        // A program that does not exist: invoked at all, the diff would fail.
        ("diff.external", "/nonexistent/rimaia-external-differ"),
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
    ] {
        git(repo.path(), &["config", key, value]);
    }
    let configured = build(repo.path(), &base_sha, &head_sha).await;

    assert!(!configured.patch.contains('\x1b'), "no ANSI escapes");
    assert!(configured
        .patch
        .contains("diff --git a/notes.txt b/notes.txt\n"));
    assert!(configured
        .patch
        .contains("\n--- a/notes.txt\n+++ b/notes.txt\n"));
    assert_eq!(configured.patch, plain.patch);
    assert_eq!(configured.files, plain.files);
}

#[tokio::test]
async fn a_submodule_change_is_one_file_and_one_section_under_diff_submodule_diff() {
    let inner = TempRepo::init();
    let pinned_first = inner.head_sha();
    let inner = inner
        .commit("one.txt", "1\n", "Add one")
        .commit("two.txt", "2\n", "Add two");
    let pinned_second = inner.head_sha();

    let outer = TempRepo::init();
    git(
        outer.path(),
        &[
            OsStr::new("-c"),
            OsStr::new("protocol.file.allow=always"),
            OsStr::new("submodule"),
            OsStr::new("add"),
            OsStr::new("-q"),
            inner.path().as_os_str(),
            OsStr::new("vendor/inner"),
        ],
    );
    let submodule = outer.path().join("vendor/inner");
    git(&submodule, &["checkout", "-q", &pinned_first]);
    git(outer.path(), &["add", "vendor/inner", ".gitmodules"]);
    git(outer.path(), &["commit", "-q", "-m", "Add the submodule"]);
    let base_sha = outer.head_sha();

    git(outer.path(), &["switch", "-q", "-c", "bump"]);
    git(&submodule, &["checkout", "-q", &pinned_second]);
    git(outer.path(), &["add", "vendor/inner"]);
    git(outer.path(), &["commit", "-q", "-m", "Bump the submodule"]);
    let head_sha = outer.head_sha();

    git(outer.path(), &["config", "diff.submodule", "diff"]);
    // The premise: under this setting a plain `git diff` expands the one bump
    // into a section per file changed inside the submodule.
    let expanded = git_bytes(outer.path(), &["diff", &format!("{base_sha}...{head_sha}")]);
    assert!(
        sections(&expanded).len() > 1,
        "the fixture must make diff.submodule=diff expand: {}",
        String::from_utf8_lossy(&expanded)
    );

    let recorded = bundle::build(outer.path(), &base_sha, &head_sha)
        .await
        .expect("a submodule bump pairs with its one section")
        .expect("a bundle");

    assert_eq!(
        inclusions(&recorded),
        vec![("vendor/inner".to_string(), PatchInclusion::Included)]
    );
    assert_eq!(sections(recorded.patch.as_bytes()).len(), 1);
    assert!(recorded
        .patch
        .contains(&format!("+Subproject commit {pinned_second}")));
}

#[tokio::test]
async fn a_branch_with_no_commits_ahead_records_head_but_no_bundle() {
    let f = Fixture::new().await;
    let worktree = worktree::prepare(f.ctx(), &f.task_id)
        .await
        .expect("prepare");
    let checkout = PathBuf::from(&worktree.path);

    let capture = bundle::capture(&checkout, worktree.base_sha.as_deref()).await;

    assert_eq!(
        capture,
        RunCapture {
            head_sha: Some(git(&checkout, &["rev-parse", "HEAD"])),
            bundle: None,
        }
    );
    assert_eq!(capture.head_sha, worktree.base_sha);
}

#[tokio::test]
async fn a_bundle_that_cannot_be_built_keeps_head_sha() {
    let repo = TempRepo::init()
        .branch("work")
        .commit("a.txt", "a\n", "Add a");
    let nowhere = "0123456789abcdef0123456789abcdef01234567";

    let capture = bundle::capture(repo.path(), Some(nowhere)).await;

    assert_eq!(
        capture,
        RunCapture {
            head_sha: Some(repo.head_sha()),
            bundle: None,
        }
    );
    let error = bundle::build(repo.path(), nowhere, &repo.head_sha())
        .await
        .expect_err("build reports what capture logged");
    assert!(error.to_string().contains(nowhere), "got: {error}");
}

#[tokio::test]
async fn the_fork_point_of_a_resumed_worktree_is_where_it_branched_not_where_the_base_is_now() {
    let f = Fixture::new().await;
    let original_tip = f.source.head_sha();
    let first = worktree::prepare(f.ctx(), &f.task_id)
        .await
        .expect("prepare");
    commit_in(
        Path::new(&first.path),
        "work.txt",
        b"attempt one\n",
        "Attempt one",
    );

    // Overnight, the base moves on.
    commit_in(
        f.source.path(),
        "upstream.txt",
        b"later\n",
        "Upstream moves on",
    );
    let moved_tip = f.source.head_sha();
    assert_ne!(moved_tip, original_tip);

    let resumed = worktree::prepare(f.ctx(), &f.task_id)
        .await
        .expect("prepare again");

    assert_eq!(resumed.path, first.path, "the same worktree, resumed");
    assert_eq!(first.base_sha.as_deref(), Some(original_tip.as_str()));
    assert_eq!(resumed.base_sha.as_deref(), Some(original_tip.as_str()));
}

#[tokio::test]
async fn a_rename_and_a_path_with_a_space_pair_with_their_own_patch_sections() {
    let lines: String = (1..=20).map(|n| format!("line {n}\n")).collect();
    let repo = TempRepo::init().commit("old name.txt", &lines, "Add the old name");
    let base_sha = repo.head_sha();
    git(repo.path(), &["switch", "-q", "-c", "work"]);
    git(repo.path(), &["mv", "old name.txt", "new name.txt"]);
    std::fs::write(
        repo.path().join("new name.txt"),
        lines.replace("line 7\n", "line seven\n"),
    )
    .expect("edit the renamed file");
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "Rename and edit"]);
    commit_in(
        repo.path(),
        "a dir/read me.md",
        b"# hello\n",
        "Add a readme",
    );
    let head_sha = repo.head_sha();

    let recorded = build(repo.path(), &base_sha, &head_sha).await;

    let patch_sections = sections(recorded.patch.as_bytes());
    assert_eq!(recorded.files.len(), 2, "a rename is one file, not two");
    assert_eq!(patch_sections.len(), 2);
    for (file, section) in recorded.files.iter().zip(&patch_sections) {
        let destination = file.path.rsplit("=> ").next().expect("a path");
        let header =
            String::from_utf8_lossy(section.split(|b| *b == b'\n').next().expect("a header"))
                .into_owned();
        assert!(
            header.ends_with(&format!("b/{destination}")),
            "{} is paired with {header}",
            file.path
        );
        assert_eq!(file.patch, PatchInclusion::Included);
    }
    assert!(recorded.files.iter().any(|file| file.path.contains("=>")));
    assert_applies_onto(repo.path(), &base_sha, &recorded.patch);
}

// ---------------------------------------------------------------------------
// End to end through `run_task`
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod end_to_end {
    use super::*;
    use pretty_assertions::assert_eq;

    use std::time::Duration;

    use rimaia_core::runner::events::RunTail;
    use rimaia_core::runner::{run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
    use rimaia_core::testing::{open_gate, FakeCli};
    use tokio::sync::broadcast::Receiver;

    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn a_run_that_commits_records_its_head_and_a_bundle_at_finish() {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        cli.commits_a_file_on_attempt(&f.task_id, 1, "notes.txt", "Write notes", "success", 0);

        let run = f.run(&cli).await.expect("the run completes");

        let checkout = f.worktree_path().await;
        assert_eq!(run.exit_class, Some(ExitClass::Success));
        assert_eq!(
            run.head_sha.as_deref(),
            Some(git(&checkout, &["rev-parse", "HEAD"]).as_str())
        );
        assert_eq!(
            run.base_sha.as_deref(),
            Some(git(&checkout, &["merge-base", "main", "HEAD"]).as_str())
        );
        let stored = f.recorded_bundle(&run.id).await;
        assert_eq!(stored.created_at, f.ctx().clock.now());
        assert_eq!(stored.commits.len(), 1);
        assert_eq!(stored.commits[0].subject, "Write notes");
        assert_eq!(stored.files.len(), 1);
        assert_eq!(stored.files[0].path, "notes.txt");
        assert!(stored
            .patch
            .as_deref()
            .expect("an unpruned patch")
            .contains("+Write notes\n"));
    }

    #[tokio::test]
    async fn a_failed_run_that_committed_still_records_its_bundle() {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        cli.commits_a_file_on_attempt(&f.task_id, 1, "partial.txt", "Half done", "max-turns", 1);

        let run = f.run(&cli).await.expect("the run is recorded");

        assert_eq!(run.exit_class, Some(ExitClass::Fatal));
        assert_eq!(run.status, RunStatus::Failed);
        let stored = f.recorded_bundle(&run.id).await;
        assert_eq!(stored.commits[0].subject, "Half done");
    }

    #[tokio::test]
    async fn each_attempt_records_the_branch_as_that_attempt_left_it() {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        cli.commits_a_file_on_attempt(&f.task_id, 1, "notes.txt", "Step one", "success", 0);
        cli.commits_a_file_on_attempt(&f.task_id, 2, "notes.txt", "Step two", "success", 0);

        let first = f.run(&cli).await.expect("attempt one");
        let second = f.run(&cli).await.expect("attempt two");

        assert_eq!(second.attempt, 2);
        assert_eq!(second.base_sha, first.base_sha, "the same fork point");
        let first_bundle = f.recorded_bundle(&first.id).await;
        assert_eq!(
            first_bundle
                .commits
                .iter()
                .map(|commit| commit.subject.as_str())
                .collect::<Vec<_>>(),
            vec!["Step one"],
            "attempt one still shows one commit after attempt two finished"
        );
        assert!(!first_bundle
            .patch
            .as_deref()
            .expect("a patch")
            .contains("Step two"));
        let second_bundle = f.recorded_bundle(&second.id).await;
        assert_eq!(
            second_bundle
                .commits
                .iter()
                .map(|commit| commit.subject.as_str())
                .collect::<Vec<_>>(),
            vec!["Step two", "Step one"],
        );
    }

    #[tokio::test]
    async fn a_run_whose_worktree_cannot_be_read_at_finish_keeps_its_outcome_and_records_nothing() {
        let f = Fixture::new().await;
        let cli = FakeCli::new();
        let gate = cli.gates(&f.task_id, "success", 5);

        let tail = f.ctx().subscribe_tail();
        let (run, ()) = tokio::join!(f.run(&cli), async {
            once_the_run_is_live(tail).await;
            let checkout = f.worktree_path().await;
            std::fs::remove_dir_all(&checkout).expect("delete the worktree underneath the run");
            open_gate(&gate);
        });
        let run = run.expect("the run is recorded");

        assert_eq!(run.exit_class, Some(ExitClass::Success));
        let task = f.task().await;
        assert_eq!(task.run_state, RunState::Idle);
        assert_eq!(task.column, BoardColumn::InReview);
        assert_eq!(run.head_sha, None);
        assert_eq!(
            runs::get_run(f.ctx(), &run.id)
                .await
                .expect("readable")
                .review,
            RunReview::NotRecorded
        );
    }

    async fn once_the_run_is_live(mut tail: Receiver<RunTail>) {
        tokio::time::timeout(TEST_TIMEOUT, tail.recv())
            .await
            .expect("a run must report itself in flight")
            .expect("the tail sender outlives the run");
    }

    impl Fixture {
        /// A queued run, the trigger every recording was captured under (see
        /// `runner_process.rs`'s header).
        async fn run(&self, cli: &FakeCli) -> rimaia_core::Result<Run> {
            tokio::time::timeout(
                TEST_TIMEOUT,
                run_task(
                    self.ctx(),
                    &self.paths,
                    &RunnerConfig {
                        program: cli.program(),
                        ..RunnerConfig::default()
                    },
                    RunRequest {
                        task_id: self.task_id.clone(),
                        trigger: RunTrigger::Queued,
                        resume: None,
                        cancel: CancelSignal::new(),
                        in_flight: None,
                    },
                ),
            )
            .await
            .expect("a run must finish inside the test timeout")
        }
    }
}

// ---------------------------------------------------------------------------
// Reading back
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_run_stays_reviewable_after_its_worktree_and_branch_are_deleted() {
    let f = Fixture::new().await;
    let run = f.record_a_run_that_commits().await;
    let before = runs::get_run(f.ctx(), &run.id).await.expect("get_run");
    assert!(matches!(
        before.review,
        RunReview::Recorded { bundle: Some(_) }
    ));

    worktree::remove_worktree(
        f.ctx(),
        &f.task_id,
        RemovalAuthorization {
            uncommitted_changes: ForceRemoval::ConfirmedByUser,
            // The test repository has no remote, so every commit is unpushed
            // and D20's guard would otherwise refuse.
            unpushed_commits: ForceRemoval::ConfirmedByUser,
            branch: BranchDisposition::DeleteEvenIfUnmerged,
        },
    )
    .await
    .expect("remove the worktree and its branch");
    std::fs::remove_dir_all(f.source.path()).expect("delete the clone");

    // What `main`'s `get_run` called, which is how this proves no git ran.
    assert!(worktree::diff_summary(f.ctx(), &f.task_id).await.is_err());
    let after = runs::get_run(f.ctx(), &run.id)
        .await
        .expect("get_run needs no clone");
    assert_eq!(after.review, before.review);
    assert_eq!(after.run, before.run);
}

#[tokio::test]
async fn a_run_recorded_before_bundles_reads_as_not_recorded() {
    let f = Fixture::new().await;
    f.claim().await;
    let run = f.start(None).await;
    std::fs::create_dir_all(Path::new(&run.log_path).parent().expect("a directory"))
        .expect("the transcript directory");
    std::fs::write(&run.log_path, "{}\n").expect("a transcript");
    finish(f.ctx(), &run.id, &RunCapture::default()).await;
    std::fs::remove_dir_all(f.source.path()).expect("delete the clone");

    let detail = runs::get_run(f.ctx(), &run.id).await.expect("get_run");

    assert_eq!(detail.review, RunReview::NotRecorded);
    assert_eq!(detail.run.head_sha, None);
    assert_eq!(detail.run.exit_class, Some(ExitClass::Success));
    assert!(detail.log_available);
}

#[tokio::test]
async fn an_in_flight_run_reads_as_not_recorded() {
    let f = Fixture::new().await;
    let worktree = worktree::prepare(f.ctx(), &f.task_id)
        .await
        .expect("prepare");
    f.claim().await;
    let run = f.start(worktree.base_sha.clone()).await;

    let detail = runs::get_run(f.ctx(), &run.id).await.expect("get_run");

    assert_eq!(detail.review, RunReview::NotRecorded);
    assert!(detail.run.base_sha.is_some(), "written at the open");
}

#[tokio::test]
async fn a_run_that_ended_with_nothing_to_review_reads_as_recorded_and_empty() {
    let f = Fixture::new().await;
    let worktree = worktree::prepare(f.ctx(), &f.task_id)
        .await
        .expect("prepare");
    f.claim().await;
    let run = f.start(worktree.base_sha.clone()).await;
    let capture = bundle::capture(Path::new(&worktree.path), worktree.base_sha.as_deref()).await;
    finish(f.ctx(), &run.id, &capture).await;

    let detail = runs::get_run(f.ctx(), &run.id).await.expect("get_run");

    assert!(detail.run.head_sha.is_some());
    assert_eq!(detail.run.head_sha, detail.run.base_sha);
    assert_eq!(detail.review, RunReview::Recorded { bundle: None });
}

#[tokio::test]
async fn a_run_whose_fork_point_is_unknown_is_not_reported_as_empty() {
    let f = Fixture::new().await;
    f.claim().await;
    let run = f.start(None).await;
    finish(
        f.ctx(),
        &run.id,
        &RunCapture {
            head_sha: Some(f.source.head_sha()),
            bundle: None,
        },
    )
    .await;

    let detail = runs::get_run(f.ctx(), &run.id).await.expect("get_run");

    assert_eq!(detail.run.head_sha, Some(f.source.head_sha()));
    assert_eq!(detail.review, RunReview::NotRecorded);
}

#[tokio::test]
async fn a_bundle_that_cannot_be_built_keeps_head_sha_and_reads_as_not_recorded() {
    let f = Fixture::new().await;
    f.claim().await;
    let base = f.source.head_sha();
    let run = f.start(Some(base.clone())).await;
    let head = "89abcdef0123456789abcdef0123456789abcdef".to_string();
    finish(
        f.ctx(),
        &run.id,
        &RunCapture {
            head_sha: Some(head.clone()),
            bundle: None,
        },
    )
    .await;

    let detail = runs::get_run(f.ctx(), &run.id).await.expect("get_run");

    assert_eq!(detail.run.head_sha, Some(head));
    assert_eq!(detail.run.base_sha, Some(base));
    assert_eq!(detail.review, RunReview::NotRecorded);
}

#[tokio::test]
async fn a_bundle_without_a_head_commit_is_dropped_and_the_finish_still_succeeds() {
    let f = Fixture::new().await;
    let recorded = f.a_bundle().await;
    f.claim().await;
    let run = f.start(Some(f.source.head_sha())).await;

    finish(
        f.ctx(),
        &run.id,
        &RunCapture {
            head_sha: None,
            bundle: Some(recorded),
        },
    )
    .await;

    assert_eq!(f.bundle_rows().await, 0);
    assert_eq!(
        runs::get_run(f.ctx(), &run.id)
            .await
            .expect("get_run")
            .review,
        RunReview::NotRecorded
    );
}

#[tokio::test]
async fn a_stored_bundle_whose_json_does_not_parse_is_an_internal_error() {
    let f = Fixture::new().await;
    let run = f.record_a_run_that_commits().await;
    sqlx::query("UPDATE review_bundles SET files = 'not json' WHERE run_id = ?1")
        .bind(&run.id)
        .execute(&f.ctx().pool)
        .await
        .expect("corrupt the row");

    let error = runs::get_run(f.ctx(), &run.id)
        .await
        .expect_err("never an empty list");

    assert_eq!(error.code(), ErrorCode::Internal);
}

#[tokio::test]
async fn pruning_transcripts_keeps_every_bundle() {
    let f = Fixture::new().await;
    let run = f.record_a_run_that_commits().await;
    std::fs::create_dir_all(Path::new(&run.log_path).parent().expect("a directory"))
        .expect("the transcript directory");
    std::fs::write(&run.log_path, "{}\n").expect("a transcript");
    let before = runs::get_run(f.ctx(), &run.id).await.expect("get_run");

    let pruned = runs::prune_logs(f.ctx(), &f.paths, PruneCriterion::Task(f.task_id.clone()))
        .await
        .expect("prune");

    assert_eq!(pruned.runs_pruned, 1);
    let after = runs::get_run(f.ctx(), &run.id).await.expect("get_run");
    assert!(!after.log_available, "the transcript went");
    assert_eq!(after.review, before.review, "the bundle did not");
    let RunReview::Recorded {
        bundle: Some(stored),
    } = after.review
    else {
        panic!("a recorded bundle");
    };
    assert!(stored.patch.is_some());
    assert_eq!(stored.patch_pruned_at, None);
}

#[tokio::test]
async fn deleting_a_task_deletes_its_runs_bundles() {
    let f = Fixture::new().await;
    f.record_a_run_that_commits().await;
    assert_eq!(f.bundle_rows().await, 1);

    tasks::delete_task(f.ctx(), &f.task_id)
        .await
        .expect("delete the task");

    assert_eq!(f.bundle_rows().await, 0);
}

#[tokio::test]
async fn listing_runs_never_reads_review_bundles() {
    let f = Fixture::new().await;
    let run = f.record_a_run_that_commits().await;
    sqlx::query("DROP TABLE review_bundles")
        .execute(&f.ctx().pool)
        .await
        .expect("drop the table");

    let listed = runs::list_runs(f.ctx(), RunFilter::default())
        .await
        .expect("list_runs never joins review_bundles");
    let for_task = runs::list_runs_for_task(f.ctx(), &f.task_id)
        .await
        .expect("nor does list_runs_for_task");

    assert_eq!(listed.len(), 1);
    assert_eq!(for_task.len(), 1);
    assert_eq!(for_task[0].id, run.id);
    let wire = serde_json::to_value(&listed[0]).expect("serialize");
    let keys: Vec<&str> = wire
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    for absent in ["review", "bundle", "patch", "files", "commits"] {
        assert!(!keys.contains(&absent), "a list entry carries no {absent}");
    }
}

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

/// A registered, opted-in repository with one ready task, and the app data
/// directory its worktrees and transcripts live under.
struct Fixture {
    harness: TestContext,
    source: TempRepo,
    /// Held for its `Drop`.
    _data: TempDir,
    paths: AppPaths,
    task_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let source = TempRepo::init();
        let data = tempfile::Builder::new()
            .prefix("rimaia-data-")
            .tempdir()
            .expect("temp dir for the app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");

        let registered = repo::register(
            &harness.context,
            &paths.worktrees_dir(),
            NewRepository {
                path: source.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register the test repository");
        repo::set_allow_unattended_runs(&harness.context, &registered.id, true)
            .await
            .expect("ADR-0012's per-repository opt-in");

        let task = tasks::create_task(
            &harness.context,
            NewTask {
                repository_id: registered.id.clone(),
                title: "Write the notes".to_string(),
                plan: Some("1. Write the notes".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task");

        Self {
            harness,
            source,
            _data: data,
            paths,
            task_id: task.id,
        }
    }

    fn ctx(&self) -> &ServiceContext {
        &self.harness.context
    }

    async fn task(&self) -> rimaia_core::db::Task {
        tasks::get_task(self.ctx(), &self.task_id)
            .await
            .expect("the task is readable")
            .task
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    async fn worktree_path(&self) -> PathBuf {
        PathBuf::from(
            self.task()
                .await
                .worktree_path
                .expect("the run prepared a worktree"),
        )
    }

    /// `Idle -> Queued -> Running`, the path every run takes and the state
    /// `finish_run` closes out.
    async fn claim(&self) {
        for state in [RunState::Queued, RunState::Running] {
            tasks::set_run_state(self.ctx(), &self.task_id, state)
                .await
                .expect("walk the card into running");
        }
    }

    async fn start(&self, base_sha: Option<String>) -> Run {
        start_run(
            self.ctx(),
            &self.paths,
            NewRun {
                task_id: self.task_id.clone(),
                session_id: "0b6d3e2e-0000-4000-8000-00000000ba5e".to_string(),
                prompt: "write the notes".to_string(),
                base_ref: Some("main".to_string()),
                base_sha,
            },
        )
        .await
        .expect("open the run row")
    }

    /// The runner's sequence without the child: prepare, open, commit in the
    /// worktree, capture, finish.
    async fn record_a_run_that_commits(&self) -> Run {
        let worktree = worktree::prepare(self.ctx(), &self.task_id)
            .await
            .expect("prepare");
        self.claim().await;
        let run = self.start(worktree.base_sha.clone()).await;
        commit_in(
            Path::new(&worktree.path),
            "notes.txt",
            b"the notes\n",
            "Write the notes",
        );
        let capture =
            bundle::capture(Path::new(&worktree.path), worktree.base_sha.as_deref()).await;
        assert!(
            capture.bundle.is_some(),
            "the fixture must produce a bundle"
        );
        finish(self.ctx(), &run.id, &capture).await
    }

    /// A real bundle, built from a scratch repository of its own.
    async fn a_bundle(&self) -> ReviewBundle {
        let repo = TempRepo::init();
        let base = repo.head_sha();
        let repo = repo.commit("a.txt", "a\n", "Add a");
        build(repo.path(), &base, &repo.head_sha()).await
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    async fn recorded_bundle(&self, run_id: &str) -> rimaia_core::runs::bundle::StoredBundle {
        match runs::get_run(self.ctx(), run_id)
            .await
            .expect("get_run")
            .review
        {
            RunReview::Recorded {
                bundle: Some(stored),
            } => stored,
            other => panic!("expected a recorded bundle, got {other:?}"),
        }
    }

    async fn bundle_rows(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM review_bundles")
            .fetch_one(&self.ctx().pool)
            .await
            .expect("count bundles")
    }
}

async fn finish(ctx: &ServiceContext, run_id: &str, capture: &RunCapture) -> Run {
    finish_run(
        ctx,
        run_id,
        &RunOutcome {
            exit_class: ExitClass::Success,
            status: RunStatus::Succeeded,
            error_message: None,
            num_turns: Some(4),
            cost_usd: Some(0.5),
            duration_ms: None,
            pr_url: None,
            usage_limit_resets_at: None,
            resume_after: None,
            spawned_as: SpawnedAs::default(),
            usage: TokenUsage::default(),
        },
        capture,
    )
    .await
    .expect("close the run row")
}

async fn build(dir: &Path, base_sha: &str, head_sha: &str) -> ReviewBundle {
    bundle::build(dir, base_sha, head_sha)
        .await
        .expect("build a bundle")
        .expect("the range has commits")
}

fn inclusions(recorded: &ReviewBundle) -> Vec<(String, PatchInclusion)> {
    recorded
        .files
        .iter()
        .map(|file| (file.path.clone(), file.patch))
        .collect()
}

/// `git diff` with the pinned vector over `base...head`, as raw bytes.
fn reference_patch(dir: &Path, base_sha: &str, head_sha: &str) -> Vec<u8> {
    let range = format!("{base_sha}...{head_sha}");
    let mut args = vec!["diff"];
    args.extend(PINNED);
    args.push("--patch");
    args.push(&range);
    git_bytes(dir, &args)
}

/// Splits a patch at its `diff --git ` header lines — a second reading of the
/// rule `worktree::bundle` applies.
fn sections(patch: &[u8]) -> Vec<Vec<u8>> {
    let mut sections: Vec<Vec<u8>> = Vec::new();
    for line in patch.split_inclusive(|byte| *byte == b'\n') {
        if line.starts_with(b"diff --git ") || sections.is_empty() {
            sections.push(Vec::new());
        }
        sections
            .last_mut()
            .expect("a section was just pushed")
            .extend_from_slice(line);
    }
    sections
}

/// Every included file's section in the stored patch is byte-equal to git's
/// own section for it, in git's order.
fn assert_included_sections_are_whole(
    dir: &Path,
    base_sha: &str,
    head_sha: &str,
    recorded: &ReviewBundle,
) {
    let whole = sections(&reference_patch(dir, base_sha, head_sha));
    assert_eq!(whole.len(), recorded.files.len());
    let expected: Vec<u8> = recorded
        .files
        .iter()
        .zip(whole)
        .filter(|(file, _)| file.patch == PatchInclusion::Included)
        .flat_map(|(_, section)| section)
        .collect();
    assert_eq!(recorded.patch.as_bytes(), expected.as_slice());
}

/// `git apply --check` of `patch` onto a detached checkout of `base_sha`.
fn assert_applies_onto(dir: &Path, base_sha: &str, patch: &str) {
    let scratch = tempfile::Builder::new()
        .prefix("rimaia-apply-")
        .tempdir()
        .expect("a scratch directory");
    let checkout = scratch.path().join("at base");
    git(
        dir,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-q"),
            OsStr::new("--detach"),
            checkout.as_os_str(),
            OsStr::new(base_sha),
        ],
    );
    let patch_file = scratch.path().join("bundle.patch");
    std::fs::write(&patch_file, patch).expect("write the patch");

    let output = Command::new("git")
        .current_dir(&checkout)
        .args([
            OsStr::new("apply"),
            OsStr::new("--check"),
            patch_file.as_os_str(),
        ])
        .output()
        .expect("run git apply");
    assert!(
        output.status.success(),
        "git apply --check refused the stored patch: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// About `bytes` bytes of distinct text lines, so each section is that large.
fn text_of(bytes: usize, label: &str) -> Vec<u8> {
    let mut text = String::new();
    let mut n = 0;
    while text.len() < bytes {
        n += 1;
        text.push_str(&format!(
            "{label} line {n:06} of a file sized to test the cap\n"
        ));
    }
    text.into_bytes()
}

/// Bytes git's binary detection recognises: a NUL in the first 8000.
fn binary_bytes() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bytes.extend((0..=255u8).cycle().take(4096));
    bytes
}

/// Writes `contents` to `file` in `dir`, creating its parent directories, and
/// commits just that file.
fn commit_in(dir: &Path, file: &str, contents: &[u8], message: &str) {
    let path = dir.join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the file's directory");
    }
    std::fs::write(path, contents).expect("write a file to commit");
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn git<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> String {
    String::from_utf8_lossy(&git_bytes(dir, args))
        .trim()
        .to_owned()
}

/// Runs git in `dir` and returns stdout untouched, panicking with both streams
/// on failure — a git error in a fixture is a broken test.
fn git_bytes<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Vec<u8> {
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
    output.stdout
}
