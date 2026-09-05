//! A real git repository in a temporary directory.
//!
//! Worktree, diff and base-ref logic is tested against this rather than against
//! a mocked git, because a mocked git only proves the mock works (ADR-0015).
//! Every invocation below is a `std::process::Command` argument vector — never
//! `sh -c` — and the work tree deliberately sits at a path containing a space,
//! so any code that forgets that rule fails here first instead of on a user's
//! `~/Documents/My Projects/...`.
//!
//! The whole tree, including the optional remote, is removed when the returned
//! value drops.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Matches the branch Rimaia assumes when a repository has no configured
/// default. Kept explicit so tests do not inherit `init.defaultBranch` from the
/// machine they run on.
const DEFAULT_BRANCH: &str = "main";

/// The space is load-bearing — see the module docs.
const WORK_TREE_DIR: &str = "work tree";

const REMOTE_DIR: &str = "origin.git";

/// Re-exported so a test outside this module canonicalizes the way the product
/// does. One rule, in [`crate::paths::git_safe`], not a second copy here.
pub use crate::paths::git_safe as git_path;

pub struct TempRepo {
    /// Held only for its `Drop`; the paths below point inside it.
    _root: TempDir,
    work_tree: PathBuf,
    remote: Option<PathBuf>,
}

impl TempRepo {
    /// A repository on [`DEFAULT_BRANCH`] with exactly one commit.
    pub fn init() -> Self {
        let root = tempfile::Builder::new()
            .prefix("rimaia-repo-")
            .tempdir()
            .expect("temp dir for the test repository");

        // macOS hands out `/var/folders/...`, a symlink to `/private/var/...`,
        // and git reports the resolved form. Resolving once here keeps `path()`
        // comparable with anything git prints back.
        let resolved =
            git_path(fs::canonicalize(root.path()).expect("temp dir must be resolvable"));

        let work_tree = resolved.join(WORK_TREE_DIR);
        fs::create_dir(&work_tree).expect("work tree directory");

        git(&work_tree, &["init", "-b", DEFAULT_BRANCH]);
        // Local, never global: CI runners have no identity configured, and a
        // test has no business writing to the operator's git config.
        git(&work_tree, &["config", "user.name", "Rimaia Test"]);
        // Windows runners default `core.autocrlf` to true, which rewrites every
        // checked-out file's line endings — so a fixture written with `\n` and
        // read back through a clone comes back with `\r\n` and no assertion
        // about file *content* can hold. These are fixtures, not a user's
        // working copy; there is nothing here that wants translating.
        git(&work_tree, &["config", "core.autocrlf", "false"]);
        git(&work_tree, &["config", "user.email", "test@rimaia.invalid"]);
        // The operator's global config may sign every commit; a CI box has no key.
        git(&work_tree, &["config", "commit.gpgsign", "false"]);

        let repo = Self {
            _root: root,
            work_tree,
            remote: None,
        };
        repo.commit("README.md", "# rimaia test repository\n", "Initial commit")
    }

    /// Writes `path` (creating parent directories) and commits just that file.
    /// `path` is relative to the work tree.
    pub fn commit(self, path: &str, contents: &str, message: &str) -> Self {
        let file = self.work_tree.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).expect("parent directory for a committed file");
        }
        fs::write(&file, contents).expect("write a file to commit");

        git(&self.work_tree, &["add", "--", path]);
        git(&self.work_tree, &["commit", "-m", message]);
        self
    }

    /// Creates `name` **and switches to it**, so a following `commit` lands on
    /// the new branch.
    pub fn branch(self, name: &str) -> Self {
        git(&self.work_tree, &["switch", "-c", name]);
        self
    }

    /// Adds a bare clone as `origin`, with the current branch tracking it, so
    /// fetch and push paths are exercisable without a network.
    pub fn with_remote(mut self) -> Self {
        assert!(
            self.remote.is_none(),
            "with_remote is not idempotent; call it once"
        );

        let remote = self
            .work_tree
            .parent()
            .expect("the work tree always sits inside the temp root")
            .join(REMOTE_DIR);

        git(
            &self.work_tree,
            &[
                OsStr::new("clone"),
                OsStr::new("--bare"),
                OsStr::new("."),
                remote.as_os_str(),
            ],
        );
        git(
            &self.work_tree,
            &[
                OsStr::new("remote"),
                OsStr::new("add"),
                OsStr::new("origin"),
                remote.as_os_str(),
            ],
        );
        git(&self.work_tree, &["fetch", "origin"]);

        let branch = self.current_branch();
        git(
            &self.work_tree,
            &[
                "branch",
                "--set-upstream-to",
                &format!("origin/{branch}"),
                &branch,
            ],
        );

        self.remote = Some(remote);
        self
    }

    /// The work tree — what a registered repository's path would be.
    pub fn path(&self) -> &Path {
        &self.work_tree
    }

    /// The bare `origin`, if [`with_remote`](Self::with_remote) was called.
    pub fn remote_path(&self) -> Option<&Path> {
        self.remote.as_deref()
    }

    pub fn head_sha(&self) -> String {
        git(&self.work_tree, &["rev-parse", "HEAD"])
    }

    pub fn current_branch(&self) -> String {
        git(&self.work_tree, &["rev-parse", "--abbrev-ref", "HEAD"])
    }
}

/// Runs git in `dir` and returns trimmed stdout, panicking with both streams on
/// failure — a git error in a test is a broken test, not a handled condition.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_worktree_add_succeeds_against_a_temp_repo() {
        // Task 019's acceptance criterion, and the precondition for every
        // worktree test in task 007. The destination also contains a space.
        let repo = TempRepo::init();
        let elsewhere = tempfile::Builder::new()
            .prefix("rimaia-worktrees-")
            .tempdir()
            .expect("temp dir for the worktree");
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

        assert!(destination.join("README.md").is_file());
        assert_eq!(git(&destination, &["rev-parse", "HEAD"]), repo.head_sha());
    }

    #[test]
    fn init_leaves_one_commit_on_the_default_branch() {
        let repo = TempRepo::init();
        assert_eq!(repo.current_branch(), DEFAULT_BRANCH);
        assert_eq!(git(repo.path(), &["rev-list", "--count", "HEAD"]), "1");
    }

    #[test]
    fn a_work_tree_path_containing_a_space_survives_every_git_call() {
        let repo = TempRepo::init();
        assert!(repo.path().to_string_lossy().contains(' '));
        assert_eq!(repo.head_sha().len(), 40);
    }

    #[test]
    fn commit_moves_head_and_writes_the_file() {
        let repo = TempRepo::init();
        let before = repo.head_sha();

        let repo = repo.commit("src/lib.rs", "pub fn slugify() {}\n", "Add slugify");

        assert_ne!(repo.head_sha(), before);
        assert_eq!(
            fs::read_to_string(repo.path().join("src/lib.rs")).expect("committed file"),
            "pub fn slugify() {}\n"
        );
    }

    #[test]
    fn branch_switches_so_the_next_commit_lands_on_it() {
        let repo = TempRepo::init().branch("feature/parser").commit(
            "parser.rs",
            "// parser\n",
            "Add parser",
        );

        assert_eq!(repo.current_branch(), "feature/parser");
        assert_eq!(
            git(repo.path(), &["rev-list", "--count", DEFAULT_BRANCH]),
            "1",
            "the default branch must not have moved"
        );
    }

    #[test]
    fn with_remote_produces_an_origin_that_accepts_a_push() {
        let repo =
            TempRepo::init()
                .with_remote()
                .commit("CHANGELOG.md", "# changelog\n", "Add changelog");

        git(repo.path(), &["push", "origin", DEFAULT_BRANCH]);

        let remote = repo.remote_path().expect("with_remote sets a remote path");
        assert_eq!(git(remote, &["rev-parse", DEFAULT_BRANCH]), repo.head_sha());
    }

    #[test]
    fn with_remote_makes_the_current_branch_tracking() {
        let repo = TempRepo::init().with_remote();
        assert_eq!(
            git(
                repo.path(),
                &["rev-parse", "--abbrev-ref", "main@{upstream}"]
            ),
            "origin/main"
        );
    }
}
