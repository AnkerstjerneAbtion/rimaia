//! Where a worktree may and may not live (task 007's Safety section,
//! ADR-0005).
//!
//! Two rules, and both are containment questions: a worktree root may not be
//! inside the repository working tree, and no operation may touch a path
//! outside the configured `worktree_root`.
//!
//! # Why a string prefix is not the answer
//!
//! `worktree_root.starts_with(repository_path)` is wrong in three separate
//! ways, all of which occur in practice on the machines this ships to:
//!
//! - **Symlinks.** On macOS `tempfile::TempDir` hands out `/var/folders/...`,
//!   and `/var` is a symlink to `/private/var`. A repository registered as one
//!   and a worktree root configured as the other are the same directory with
//!   no shared prefix. [`resolve`] canonicalizes both before anything is
//!   compared, which is also why `TempRepo` canonicalizes its own root.
//! - **`..`.** `<repo>/../elsewhere` has the repository as a prefix and is not
//!   inside it; `<outside>/../<repo>/wt` is inside it and does not. Canonical
//!   forms contain no `..` at all, and [`resolve`] refuses an input that does,
//!   because a path whose unresolved tail walks upwards cannot be resolved
//!   without inventing an answer.
//! - **Case-insensitive filesystems.** APFS and NTFS treat `Repo` and `repo`
//!   as one directory, and `realpath(3)` does *not* normalize case — verified,
//!   not assumed: on macOS `os.path.realpath` returns `/private/tmp/x/repo`
//!   and `/private/tmp/x/Repo` unchanged for one directory. So even two
//!   canonical paths can name the same directory and share no prefix. That is
//!   why [`is_within`] compares directory *identity* — device and inode —
//!   rather than text, wherever both sides exist.
//!
//! `pub(super)`: [`crate::worktree`]'s implementation detail.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// Canonicalizes as much of `path` as exists and keeps the rest verbatim.
///
/// A worktree path is checked *before* it is created, so the leaf — and on a
/// first run the root above it — does not exist yet and
/// `tokio::fs::canonicalize` fails outright on the whole path. Resolving the
/// deepest existing ancestor and re-appending the missing components gives a
/// path in canonical form for every part that could have been a symlink,
/// which is every part that is already on disk.
///
/// An absolute path with no `..` in it, because a relative path has no
/// meaning against a process working directory the caller does not control,
/// and a `..` in the unresolved tail cannot be collapsed without guessing
/// what it would have resolved to.
pub(super) async fn resolve(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::invalid(format!(
            "{} must be an absolute path",
            path.display()
        )));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(Error::invalid(format!(
            "{} must not contain \"..\"",
            path.display()
        )));
    }

    let mut existing = path.to_path_buf();
    let mut missing: Vec<OsString> = Vec::new();

    loop {
        if let Ok(resolved) = tokio::fs::canonicalize(&existing).await {
            // Same reason `repo::register` does it: what comes back from here
            // becomes a `git worktree add` argument.
            let mut full = crate::paths::git_safe(resolved);
            full.extend(missing.iter().rev());
            return Ok(full);
        }

        // `file_name` is `None` only for a root or a prefix, and both of those
        // canonicalize, so the loop always terminates through the branch above
        // — but not depending on that is cheaper than proving it at every
        // future edit.
        let Some(name) = existing.file_name().map(ToOwned::to_owned) else {
            return Err(Error::invalid(format!(
                "{} could not be resolved",
                path.display()
            )));
        };
        missing.push(name);
        existing.pop();
    }
}

/// Whether `path` is `ancestor` itself or lives beneath it.
///
/// Walks `path`'s ancestors and asks, of each, whether it is the *same
/// directory* as `ancestor` — by device and inode where both exist, falling
/// back to exact equality where they do not. Both arguments are expected to
/// have been through [`resolve`] first; identity is what covers the case
/// canonicalization cannot, which is a case-insensitive filesystem.
pub(super) async fn is_within(path: &Path, ancestor: &Path) -> bool {
    for candidate in path.ancestors() {
        if same_directory(candidate, ancestor).await {
            return true;
        }
    }
    false
}

/// Refuses a worktree root inside the repository working tree (ADR-0005:
/// worktrees live under the app data directory "so they can't be accidentally
/// staged").
pub(super) async fn ensure_outside_repository(root: &Path, repository: &Path) -> Result<()> {
    if is_within(root, repository).await {
        return Err(Error::invalid(format!(
            "worktrees must live outside the repository, but {} is inside {}",
            root.display(),
            repository.display(),
        )));
    }
    Ok(())
}

/// Refuses to operate on a path outside the configured `worktree_root`.
///
/// The path this guards is read out of the database, where a hand-edit, a
/// changed `worktree_root` or a bug in an earlier version could have left
/// anything at all — and the operations behind it delete directories.
pub(super) async fn ensure_within_root(path: &Path, root: &Path) -> Result<()> {
    if !is_within(path, root).await {
        return Err(Error::invalid(format!(
            "{} is outside this repository's worktree root {}",
            path.display(),
            root.display(),
        )));
    }
    Ok(())
}

/// Whether two paths name one directory.
///
/// Device and inode on unix, which is exact: it sees through symlinks the
/// caller forgot to resolve, through a case-insensitive filesystem's two
/// spellings, and through a bind mount. Elsewhere — and for any path that
/// does not exist, such as the worktree about to be created — it degrades to
/// comparing the canonical forms, which is what the rest of this module is
/// careful to hand it.
///
/// Also used directly to match a path against what `git worktree list`
/// printed, which is a question of identity and not of text for exactly the
/// same reasons.
pub(super) async fn same_directory(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if let (Ok(a), Ok(b)) = (tokio::fs::metadata(a).await, tokio::fs::metadata(b).await) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn a_relative_path_is_refused() {
        let error = resolve(Path::new("worktrees/task-1"))
            .await
            .expect_err("a relative path has no meaning here");

        assert_eq!(
            error.to_string(),
            "worktrees/task-1 must be an absolute path"
        );
    }

    #[tokio::test]
    async fn a_path_containing_a_parent_segment_is_refused() {
        // Built from a real temp directory rather than from a `/tmp/...`
        // literal: the absoluteness guard runs first, and on Windows a
        // POSIX-looking path is not absolute at all — so the literal version of
        // this test asserted the `..` rule while actually exercising the one
        // before it.
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("repo").join("..").join("elsewhere");

        let error = resolve(&path)
            .await
            .expect_err("`..` cannot be resolved without guessing");

        assert_eq!(
            error.to_string(),
            format!("{} must not contain \"..\"", path.display())
        );
    }

    #[tokio::test]
    async fn resolving_keeps_the_components_that_do_not_exist_yet() {
        let temp = tempfile::tempdir().expect("temp dir");
        // Through `git_safe`, because `resolve` does — the expectation has to
        // be the path git would be handed, not the extended-length one Windows
        // canonicalization returns.
        let canonical = crate::paths::git_safe(
            tokio::fs::canonicalize(temp.path())
                .await
                .expect("a temp dir resolves"),
        );

        let resolved = resolve(&temp.path().join("worktrees/repo/task-1"))
            .await
            .expect("an unborn leaf resolves through its existing ancestor");

        assert_eq!(resolved, canonical.join("worktrees/repo/task-1"));
    }

    #[tokio::test]
    async fn a_directory_is_within_itself() {
        let temp = tempfile::tempdir().expect("temp dir");
        let canonical = resolve(temp.path()).await.expect("resolve");

        assert!(is_within(&canonical, &canonical).await);
    }

    #[tokio::test]
    async fn a_sibling_is_not_within() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = resolve(temp.path()).await.expect("resolve");
        std::fs::create_dir(root.join("one")).expect("create one");
        std::fs::create_dir(root.join("two")).expect("create two");

        assert!(!is_within(&root.join("one"), &root.join("two")).await);
    }

    #[tokio::test]
    async fn a_name_that_merely_shares_a_textual_prefix_is_not_within() {
        // `/x/repository` starts with `/x/repo` as a string and is not inside
        // it. Component-wise ancestry, not `str::starts_with`.
        let temp = tempfile::tempdir().expect("temp dir");
        let root = resolve(temp.path()).await.expect("resolve");
        std::fs::create_dir(root.join("repo")).expect("create repo");
        std::fs::create_dir(root.join("repository")).expect("create repository");

        assert!(!is_within(&root.join("repository"), &root.join("repo")).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_path_reached_through_a_symlink_is_recognised_as_within() {
        // The macOS `/var` → `/private/var` case in miniature, and the reason
        // this module resolves before it compares.
        let temp = tempfile::tempdir().expect("temp dir");
        let root = resolve(temp.path()).await.expect("resolve");
        let real = root.join("real");
        std::fs::create_dir(&real).expect("create real");
        std::os::unix::fs::symlink(&real, root.join("link")).expect("symlink");

        let through_the_link = resolve(&root.join("link/inside"))
            .await
            .expect("resolve through the link");

        assert!(is_within(&through_the_link, &real).await);
    }
}
