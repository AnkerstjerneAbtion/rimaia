//! Low-level `git` and `gh` subprocess calls behind repository registration.
//!
//! Every invocation is an argument vector via `tokio::process::Command` —
//! never `sh -c` — because a registered repository's path can contain spaces
//! (task 003's Notes; `rimaia_core::testing::TempRepo` deliberately puts one
//! in its own work tree for exactly this reason).
//!
//! `pub(super)`: this is [`crate::repo`]'s implementation detail, not part of
//! the service's public surface.

use std::path::Path;
use std::process::Output;

use crate::error::{Error, Result};

/// Resolved through `PATH`, like `claude` and for the same reason (ADR-0004):
/// Rimaia drives the tools the operator already has rather than bundling its
/// own. Named constants because task 018's doctor probes the same two binaries
/// this module runs, and a doctor that checked a *different* `git` from the one
/// worktree creation uses would be reassuring about the wrong thing.
pub const GIT_CLI: &str = "git";
pub const GH_CLI: &str = "gh";

/// Runs `git` with `args` in `dir`. A spawn failure — `git` itself missing —
/// is `Error::internal`: unlike every other outcome in this module, no input
/// the user supplies through repository registration can fix a missing `git`
/// binary.
async fn run(dir: &Path, args: &[&str]) -> Result<Output> {
    tokio::process::Command::new(GIT_CLI)
        .current_dir(dir)
        .args(args)
        .output()
        .await
        .map_err(|error| {
            Error::internal(format!("could not run git in {}: {error}", dir.display()))
        })
}

/// `Some(trimmed stdout)` on a zero exit, `None` on a non-zero one — the
/// shape every probe below wants: "does this exist", not "did git itself
/// fail to run". A probe's non-zero exit is an ordinary, expected outcome
/// (no such ref, no such remote, not a repository at all), so it is not an
/// `Err` the way a spawn failure is.
async fn probe(dir: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = run(dir, args).await?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string()))
}

/// `git-dir` and `git-common-dir`, both resolved absolute. Equal for the main
/// working tree (or a bare repository); different for a linked worktree,
/// because a worktree's `git-dir` is `<main>/.git/worktrees/<name>` while its
/// `common-dir` stays `<main>/.git`. `None` when `dir` is not inside a git
/// repository at all.
pub(super) async fn git_dirs(dir: &Path) -> Result<Option<(String, String)>> {
    let Some(stdout) = probe(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    )
    .await?
    else {
        return Ok(None);
    };

    let mut lines = stdout.lines();
    let git_dir = lines.next().unwrap_or_default().to_string();
    let common_dir = lines.next().unwrap_or_default().to_string();
    Ok(Some((git_dir, common_dir)))
}

/// Whether `HEAD` resolves to a real commit — false for a freshly
/// initialized repository that has never committed (an "unborn" branch).
pub(super) async fn has_at_least_one_commit(dir: &Path) -> Result<bool> {
    Ok(probe(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .await?
        .is_some())
}

/// `origin/HEAD`'s target branch name, if that symbolic ref exists — the
/// first link of task 003's default-branch fallback chain. Most clones never
/// set this locally unless `git clone` (non-bare) or `git remote set-head`
/// created it, so `None` here is the common case, not a failure.
async fn origin_head_branch(dir: &Path) -> Result<Option<String>> {
    let target = probe(dir, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]).await?;
    Ok(target.and_then(|target| {
        target
            .strip_prefix("refs/remotes/origin/")
            .map(str::to_string)
    }))
}

async fn branch_exists(dir: &Path, branch: &str) -> Result<bool> {
    let refname = format!("refs/heads/{branch}");
    Ok(probe(dir, &["show-ref", "--verify", "--quiet", &refname])
        .await?
        .is_some())
}

/// The branch `HEAD` is on, or `None` when `HEAD` is detached — `rev-parse
/// --abbrev-ref` prints the literal string `"HEAD"` in that case, which is
/// not a branch name and must not be mistaken for one.
async fn current_branch(dir: &Path) -> Result<Option<String>> {
    let branch = probe(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    Ok(branch.filter(|branch| branch != "HEAD"))
}

/// Task 003's fallback chain: `origin/HEAD`, else `main`, else `master`, else
/// the current branch. `None` only when none apply — a detached `HEAD` with
/// no `origin/HEAD` and neither conventional branch name, which is the case
/// registration refuses with its own message.
pub(super) async fn default_branch(dir: &Path) -> Result<Option<String>> {
    if let Some(branch) = origin_head_branch(dir).await? {
        return Ok(Some(branch));
    }
    for candidate in ["main", "master"] {
        if branch_exists(dir, candidate).await? {
            return Ok(Some(candidate.to_string()));
        }
    }
    current_branch(dir).await
}

/// `git remote get-url origin`, or `None` when there is no `origin` remote.
/// Task 003 only ever looks at `origin`; a repository whose only remote is
/// named something else is out of scope for the MVP.
pub(super) async fn remote_url(dir: &Path) -> Result<Option<String>> {
    probe(dir, &["remote", "get-url", "origin"]).await
}

/// The host segment of a remote URL, for `gh auth status --hostname`.
/// Handles the shapes git itself accepts — `scheme://[user@]host[:port]/path`
/// and the scp-like `[user@]host:path` — and correctly returns `None` for a
/// bare local filesystem path, which has no host at all (the shape
/// `rimaia_core::testing::TempRepo::with_remote` uses, and the reason its own
/// tests never depend on a real `gh` install or its auth state).
pub(super) fn host_from_remote_url(url: &str) -> Option<String> {
    for scheme in ["ssh://", "https://", "http://", "git://"] {
        if let Some(rest) = url.strip_prefix(scheme) {
            let after_user = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
            let host = after_user.split(['/', ':']).next()?;
            return (!host.is_empty()).then(|| host.to_string());
        }
    }

    // scp-like syntax, `[user@]host:path` — everything before the first
    // colon that is not itself a path (ruling out a bare filesystem path
    // that merely happens to contain one, such as a Windows drive letter).
    let (host_part, _) = url.split_once(':')?;
    if host_part.is_empty() || host_part.len() == 1 || host_part.contains(['/', '\\']) {
        return None;
    }
    let host = host_part
        .rsplit_once('@')
        .map_or(host_part, |(_, host)| host);
    (!host.is_empty()).then(|| host.to_string())
}

/// The three answers `gh auth status --hostname <host>` can give, kept apart.
///
/// [`gh_authenticated`] deliberately collapses the first two into `false`,
/// which is all task 003's warning needs. Task 018's doctor needs them apart:
/// "install the GitHub CLI" and "run `gh auth login`" are different
/// remediations, and a doctor row that offers the wrong one is worse than no
/// row at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GhProbe {
    /// No `gh` on `PATH` — the spawn itself failed with `NotFound`.
    NotInstalled,
    /// `gh` ran and said no: no credentials for this host.
    NotAuthenticated,
    Ready,
}

/// Whether `gh` is installed and authenticated for `host`, told apart.
///
/// Never a hard failure, for the reason [`gh_authenticated`] gives. The
/// distinction rests on `Command::output` returning `Err(NotFound)` when the
/// binary is not on `PATH` versus `Ok` with a non-zero status when it ran and
/// refused — a difference the operating system makes for us, not one inferred
/// from parsing `gh`'s prose. Any *other* spawn error (a permission problem on
/// the binary, say) is reported as `NotInstalled` too: from the caller's side
/// the effect is identical, and inventing a fourth state for it would buy no
/// remediation the user could act on differently.
///
/// `program` is a path rather than a bare name so a test can point at a
/// stand-in that exits non-zero — the same injection
/// [`RunnerConfig::program`](crate::runner::RunnerConfig::program) already
/// allows for `claude`, and the only way to test the unauthenticated branch
/// without depending on the developer's own `gh` login state.
pub(super) async fn gh_probe(program: &Path, host: &str) -> GhProbe {
    match tokio::process::Command::new(program)
        .args(["auth", "status", "--hostname", host])
        .output()
        .await
    {
        Ok(output) if output.status.success() => GhProbe::Ready,
        Ok(_) => GhProbe::NotAuthenticated,
        Err(_) => GhProbe::NotInstalled,
    }
}

/// Whether `gh` is installed and authenticated for `host`. Never a hard
/// failure: task 003 treats a missing or unauthenticated `gh` as a warning on
/// the repository, not an error, so even "the binary is not on `PATH`"
/// collapses to `false` here rather than propagating the way a missing `git`
/// does in [`run`] — the two tools have different blast radii, since `gh` is
/// optional infrastructure and `git` is not.
pub(super) async fn gh_authenticated(host: &str) -> bool {
    gh_probe(Path::new(GH_CLI), host).await == GhProbe::Ready
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_host_is_read_from_an_https_url() {
        assert_eq!(
            host_from_remote_url("https://github.com/rimaia/rimaia.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn a_host_is_read_from_an_https_url_carrying_a_port() {
        assert_eq!(
            host_from_remote_url("https://gitlab.example.com:8443/rimaia/rimaia.git"),
            Some("gitlab.example.com".to_string())
        );
    }

    #[test]
    fn a_host_is_read_from_an_ssh_scheme_url() {
        assert_eq!(
            host_from_remote_url("ssh://git@github.com/rimaia/rimaia.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn a_host_is_read_from_scp_like_syntax() {
        assert_eq!(
            host_from_remote_url("git@github.com:rimaia/rimaia.git"),
            Some("github.com".to_string())
        );
    }

    #[test]
    fn a_bare_local_path_has_no_host() {
        // The shape `TempRepo::with_remote` uses, and why this module's own
        // tests never depend on a real `gh` install or its auth state.
        assert_eq!(host_from_remote_url("/Users/someone/Code/origin.git"), None);
    }

    #[test]
    fn a_windows_drive_letter_is_not_mistaken_for_a_host() {
        assert_eq!(host_from_remote_url(r"C:\Users\someone\repo"), None);
    }
}
