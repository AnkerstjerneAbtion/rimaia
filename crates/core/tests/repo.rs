//! [`TempRepo`] from the outside, as tasks 007-009 will use it.
//!
//! The unit tests next to the builder can reach its private `git` helper; these
//! cannot, so they also prove the public surface is enough to *inspect* a
//! repository the builder produced — a worktree test that can only assert
//! through the builder's own eyes is asserting nothing.
//!
//! Every git call below is an argument vector, and both the repository and the
//! worktree destination sit at paths containing a space. That is the whole
//! reason these paths look the way they do.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;

use pretty_assertions::{assert_eq, assert_ne};
use rimaia_core::testing::{self, TempRepo};

/// The branch `TempRepo::init` leaves a fresh repository on.
const DEFAULT_BRANCH: &str = "main";

#[test]
fn git_worktree_add_succeeds_against_a_temp_repo() {
    // Task 019's acceptance criterion, and the precondition for every worktree
    // operation in task 007. Worktrees live outside the repository (ADR-0005),
    // so the destination is a second temporary directory, not a subdirectory.
    let repo = TempRepo::init();
    let elsewhere = scratch_dir("rimaia-worktrees-");
    let destination = elsewhere.path().join("task 42");

    git(
        repo.path(),
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new("rimaia/task-42"),
            destination.as_os_str(),
            OsStr::new(DEFAULT_BRANCH),
        ],
    );

    assert!(
        destination.join("README.md").is_file(),
        "the worktree should be checked out, not just registered"
    );
    assert_eq!(
        git(&destination, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "rimaia/task-42"
    );
    assert_eq!(git(&destination, &["rev-parse", "HEAD"]), repo.head_sha());

    // Through `git_path`, because `fs::canonicalize` returns a Windows
    // extended-length `\\?\` path and `git worktree list` prints the plain
    // one — the same directory, two strings.
    let registered =
        testing::git_path(fs::canonicalize(&destination).expect("the worktree must exist on disk"));
    assert!(
        git(repo.path(), &["worktree", "list", "--porcelain"])
            .contains(&registered.to_string_lossy().into_owned()),
        "the repository should know about the worktree it just created"
    );
}

#[test]
fn a_worktree_can_start_from_a_branch_the_builder_created() {
    // Task 007 resolves a base ref that is usually not the default branch; the
    // builder has to be able to produce that situation.
    let repo =
        TempRepo::init()
            .branch("feature/base")
            .commit("base.rs", "// base\n", "Add a base commit");
    let base_tip = repo.head_sha();
    let elsewhere = scratch_dir("rimaia-worktrees-");
    let destination = elsewhere.path().join("task 7");

    git(
        repo.path(),
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new("rimaia/task-7"),
            destination.as_os_str(),
            OsStr::new("feature/base"),
        ],
    );

    assert_eq!(git(&destination, &["rev-parse", "HEAD"]), base_tip);
    assert!(destination.join("base.rs").is_file());
}

#[test]
fn committed_files_are_reachable_from_head_in_the_order_they_were_written() {
    let repo = TempRepo::init()
        .commit("src/lib.rs", "pub fn slugify() {}\n", "Add slugify")
        .commit("src/main.rs", "fn main() {}\n", "Add a binary");

    assert_eq!(
        git(repo.path(), &["log", "--format=%s"]),
        "Add a binary\nAdd slugify\nInitial commit"
    );
    assert_eq!(
        git(repo.path(), &["ls-tree", "-r", "--name-only", "HEAD"]),
        "README.md\nsrc/lib.rs\nsrc/main.rs"
    );
    assert_eq!(
        git(repo.path(), &["status", "--porcelain"]),
        "",
        "commit must leave nothing staged or untracked behind"
    );
}

#[test]
fn head_sha_is_the_full_commit_object_git_rev_parse_reports() {
    let repo = TempRepo::init().commit("NOTES.md", "notes\n", "Add notes");
    let sha = repo.head_sha();

    assert_eq!(sha, git(repo.path(), &["rev-parse", "HEAD"]));
    assert_eq!(sha.len(), 40, "an abbreviated sha would break run records");
    assert_eq!(git(repo.path(), &["cat-file", "-t", &sha]), "commit");
}

#[test]
fn branch_switches_the_checkout_and_leaves_the_default_branch_where_it_was() {
    let repo = TempRepo::init();
    let default_tip = repo.head_sha();

    let repo = repo
        .branch("feature/parser")
        .commit("parser.rs", "// parser\n", "Add a parser");

    assert_eq!(repo.current_branch(), "feature/parser");
    assert_ne!(repo.head_sha(), default_tip);
    assert_eq!(
        git(repo.path(), &["rev-parse", DEFAULT_BRANCH]),
        default_tip
    );
}

#[test]
fn a_fresh_clone_of_the_remote_sees_every_pushed_commit() {
    let repo =
        TempRepo::init()
            .with_remote()
            .commit("CHANGELOG.md", "# changelog\n", "Add a changelog");
    git(repo.path(), &["push", "origin", DEFAULT_BRANCH]);

    let remote = repo.remote_path().expect("with_remote records the remote");
    let elsewhere = scratch_dir("rimaia-clone-");
    let clone = elsewhere.path().join("clone of origin");
    git(
        elsewhere.path(),
        &[OsStr::new("clone"), remote.as_os_str(), clone.as_os_str()],
    );

    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), repo.head_sha());
    assert_eq!(
        fs::read_to_string(clone.join("CHANGELOG.md")).expect("the pushed file"),
        "# changelog\n"
    );
}

#[test]
fn a_repository_without_a_remote_reports_none() {
    // Most tests do not want the extra bare clone; the absence has to be
    // observable so a fetch-path test can assert it opted in.
    assert!(TempRepo::init().remote_path().is_none());
}

/// A temporary directory outside any repository, for worktrees and clones.
fn scratch_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .unwrap_or_else(|error| panic!("temp dir for {prefix}: {error}"))
}

/// Runs git in `dir` and returns trimmed stdout. A git failure here is a broken
/// test, not a handled condition, so both streams go into the panic message.
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
