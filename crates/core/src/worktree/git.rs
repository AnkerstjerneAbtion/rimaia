//! The `git` subprocess calls behind the worktree service.
//!
//! Every invocation is an argument vector via `tokio::process::Command` —
//! never `sh -c` — because both a repository path and a worktree path can
//! contain spaces (`rimaia_core::testing::TempRepo` puts one in its own work
//! tree for exactly that reason, and task 007's Notes name argument passing as
//! the point).
//!
//! Deliberately **not** [`crate::repo::git`]. That module is `pub(super)` to
//! [`crate::repo`] and unreachable from here, but it also answers a different
//! question: its `probe` collapses a non-zero exit to `None` because every
//! caller there is asking a validation question ("is this a repository?"), for
//! which a non-zero exit is an ordinary "no". Every call here is an *operation*
//! whose failure the user has to read and act on, so [`checked`] carries git's
//! own stderr into the error instead of discarding it. What is *not* duplicated
//! is any rule: the default branch a base ref comes from was resolved once by
//! task 003 and is read off the `repositories` row.
//!
//! `pub(super)`: [`crate::worktree`]'s implementation detail.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::{CommitSummary, DiffStat, FileDiffStat};
use crate::error::{Error, Result};

/// The field separator inside one `git log` record, and the reason records can
/// still be split on newlines: ASCII unit separator cannot appear in a commit
/// subject, an author name or an ISO timestamp, and `%s` is by definition the
/// first line of the message.
const LOG_FIELD_SEPARATOR: char = '\u{1f}';

const LOG_FORMAT: &str = "--format=%H\u{1f}%h\u{1f}%an\u{1f}%cI\u{1f}%s";

/// Both captured streams and whether git succeeded — the shape a caller that
/// wants to *interpret* a non-zero exit needs. [`checked`] is the shape for
/// callers that do not.
pub(super) struct Output {
    pub(super) success: bool,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

/// One linked worktree as `git worktree list --porcelain` reports it.
pub(super) struct WorktreeEntry {
    pub(super) path: PathBuf,
    /// `None` for a detached `HEAD` or a bare repository — neither of which a
    /// Rimaia worktree ever is, but both of which appear in the list output of
    /// a repository that has other worktrees in it.
    pub(super) branch: Option<String>,
}

/// Runs `git` with `args` in `dir`, capturing both streams and logging the
/// command line and exit status at debug level — task 007's Safety section
/// requires that of *every* invocation, which is why there is one place it can
/// be done.
///
/// A spawn failure — `git` itself missing — is [`Error::internal`] for the
/// reason [`crate::repo::git`] gives at its own: no input the user supplies
/// through the app can fix a missing `git` binary.
pub(super) async fn run<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<Output> {
    let output = tokio::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .await
        .map_err(|error| {
            Error::internal(format!("could not run git in {}: {error}", dir.display()))
        })?;

    tracing::debug!(
        dir = %dir.display(),
        command = %command_line(args),
        // `None` only when a signal killed git, which `.code()` cannot spell.
        status = output.status.code().unwrap_or(-1),
        "ran git",
    );

    Ok(Output {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string(),
        stderr: String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .to_string(),
    })
}

/// Runs git and turns a non-zero exit into an error carrying git's own
/// message.
///
/// [`Error::invalid`] and no new `ErrorCode` (seam-contract D8): almost every
/// way one of these fails is something the user can act on — a dirty worktree,
/// a branch already checked out somewhere else, a base ref that no longer
/// exists — and the specificity that matters is in git's sentence, not in a
/// code the frontend would have to branch on.
pub(super) async fn checked<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<String> {
    let output = run(dir, args).await?;
    if output.success {
        return Ok(output.stdout);
    }

    // git writes diagnostics to stderr, but not universally — `worktree
    // remove` on a dirty tree does, `rev-list` on a bad revision does, and a
    // few plumbing commands report on stdout instead. Preferring stderr and
    // falling back keeps the message from ever being empty.
    let detail = if output.stderr.is_empty() {
        output.stdout
    } else {
        output.stderr
    };
    Err(Error::invalid(format!(
        "git {} failed: {detail}",
        command_line(args)
    )))
}

/// `git fetch --prune`, **best effort**: an overnight queue on a train still
/// has to run, so being offline is a logged warning and never a failure
/// (task 007's Scope). A repository with no remote at all fails here too, for
/// a different reason and with the same non-consequence.
pub(super) async fn fetch_prune(dir: &Path) {
    match run(dir, &["fetch", "--prune"]).await {
        Ok(output) if output.success => {}
        Ok(output) => tracing::warn!(
            dir = %dir.display(),
            detail = %output.stderr,
            "git fetch --prune failed; continuing with the refs already on disk",
        ),
        Err(error) => tracing::warn!(
            dir = %dir.display(),
            %error,
            "could not run git fetch --prune; continuing with the refs already on disk",
        ),
    }
}

/// `git worktree add`, either creating `branch` from `base_ref` or attaching
/// to a `branch` that already exists.
///
/// The second form is what makes a retry resume in place rather than rewind
/// (ADR-0005: "a retried or resumed run reuses the existing worktree and
/// branch"): `-b` against an existing branch is an error, and re-creating the
/// branch from the base would discard everything the previous attempt
/// committed.
pub(super) async fn worktree_add(
    dir: &Path,
    path: &Path,
    branch: &str,
    base_ref: &str,
    create_branch: bool,
) -> Result<()> {
    let mut args: Vec<OsString> = vec!["worktree".into(), "add".into()];
    if create_branch {
        args.push("-b".into());
        args.push(branch.into());
        args.push(path.as_os_str().to_owned());
        args.push(base_ref.into());
    } else {
        args.push(path.as_os_str().to_owned());
        args.push(branch.into());
    }
    checked(dir, &args).await.map(|_| ())
}

pub(super) async fn worktree_list(dir: &Path) -> Result<Vec<WorktreeEntry>> {
    let stdout = checked(dir, &["worktree", "list", "--porcelain"]).await?;
    Ok(parse_worktree_list(&stdout))
}

/// `git worktree remove`, with `--force` only when the caller was handed an
/// explicit confirmation to pass on (task 007's Scope).
pub(super) async fn worktree_remove(dir: &Path, path: &Path, force: bool) -> Result<()> {
    let mut args: Vec<OsString> = vec!["worktree".into(), "remove".into()];
    if force {
        args.push("--force".into());
    }
    args.push(path.as_os_str().to_owned());
    checked(dir, &args).await.map(|_| ())
}

/// `git worktree prune` — drops the administrative files under
/// `.git/worktrees/` for worktrees whose directory is gone. Removal runs it
/// after `remove` and reconciliation runs it on its own, which is the half of
/// "cleans up both the directory and git's worktree metadata" that
/// `rm -rf` never does.
pub(super) async fn worktree_prune(dir: &Path) -> Result<()> {
    checked(dir, &["worktree", "prune"]).await.map(|_| ())
}

pub(super) async fn branch_exists(dir: &Path, branch: &str) -> Result<bool> {
    let refname = format!("refs/heads/{branch}");
    let output = run(dir, &["show-ref", "--verify", "--quiet", &refname]).await?;
    Ok(output.success)
}

/// Whether `refname` resolves to a commit — used on the base ref, so that a
/// `default_branch` the user edited into something that does not exist is
/// refused with a sentence rather than with git's `worktree add` failure.
pub(super) async fn commit_exists(dir: &Path, refname: &str) -> Result<bool> {
    let revision = format!("{refname}^{{commit}}");
    let output = run(dir, &["rev-parse", "--verify", "--quiet", &revision]).await?;
    Ok(output.success)
}

/// `git branch -D`. Capital `D`, deliberately: a Rimaia run branch is
/// unmerged by definition until someone merges the PR, so `-d` would refuse
/// nearly every request and make `delete_branch` a parameter that does
/// nothing. Passing it *is* the explicit ask ADR-0005 requires — "the branch
/// is left alone unless the user asks for it to go".
pub(super) async fn delete_branch(dir: &Path, branch: &str) -> Result<()> {
    checked(dir, &["branch", "-D", branch]).await.map(|_| ())
}

/// `(ahead, behind)` for `branch` against `base`.
///
/// `--left-right --count base...branch` prints the left count first — commits
/// reachable from `base` but not `branch`, which is how far the branch is
/// *behind* — then the right, which is how far it is ahead. Getting them the
/// wrong way round is the classic reading error here, hence the explicit
/// naming.
pub(super) async fn ahead_behind(dir: &Path, base: &str, branch: &str) -> Result<(i64, i64)> {
    let range = format!("{base}...{branch}");
    let stdout = checked(dir, &["rev-list", "--left-right", "--count", &range]).await?;

    let mut counts = stdout.split_whitespace();
    let behind = counts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    let ahead = counts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    Ok((ahead, behind))
}

/// Whether the working tree at `worktree` has anything uncommitted —
/// modifications, staged changes or untracked files alike, since all three are
/// work a removal would destroy.
pub(super) async fn is_dirty(worktree: &Path) -> Result<bool> {
    Ok(dirty_file_count(worktree).await? > 0)
}

/// How many paths [`is_dirty`] is answering "yes" about.
///
/// A count rather than a bool because task 016's refusal has to *name* what it
/// is protecting — "3 uncommitted changes" is a sentence the user can check
/// against their own memory of the run, where "the worktree is dirty" is a
/// sentence they can only take on trust. One line of `--porcelain` is one
/// path, including a rename's `R  old -> new`, which is one change.
pub(super) async fn dirty_file_count(worktree: &Path) -> Result<i64> {
    let stdout = checked(worktree, &["status", "--porcelain"]).await?;
    Ok(stdout.lines().filter(|line| !line.is_empty()).count() as i64)
}

/// Commits on `branch`, past `base`, that exist on no remote-tracking ref.
///
/// `--not <base> --remotes` excludes two sets at once: what the branch was
/// created from, and everything any remote already has. What is left is
/// precisely the work that would be gone for good if the branch went with the
/// worktree — which is the question the unpushed-commits guard is asking, and
/// not the same question as "is this branch ahead of its upstream". A branch
/// that was never pushed has no upstream at all, and counting against one would
/// silently answer zero for the case that matters most.
///
/// A repository with no remote reports every commit the run made, which is
/// correct rather than unfortunate: nothing has a second copy of them.
pub(super) async fn unpushed_commits(dir: &Path, base: &str, branch: &str) -> Result<i64> {
    let stdout = checked(
        dir,
        &["rev-list", "--count", branch, "--not", base, "--remotes"],
    )
    .await?;
    Ok(stdout.trim().parse().unwrap_or(0))
}

/// Whether every commit on `branch` is already reachable from `base`.
///
/// `merge-base --is-ancestor` rather than parsing `git branch --merged`: it
/// answers about one branch instead of listing all of them, it needs no output
/// parsing, and its exit status *is* the answer. Both are the same predicate —
/// `--merged` is documented as "branches whose tips are reachable from the
/// specified commit".
///
/// It says **no** for a branch that was squash-merged or rebased onto the
/// default, because those produce different commits and git cannot tell them
/// from work that was never merged at all. That false negative is the one to
/// have: it costs a user one extra click on "delete the branch anyway", where
/// the false positive costs them the only copy of a commit.
pub(super) async fn is_merged(dir: &Path, base: &str, branch: &str) -> Result<bool> {
    // Not `checked`: exit 1 is "not an ancestor", an answer rather than a
    // failure, and `checked` would turn every unmerged branch into an error.
    let output = run(dir, &["merge-base", "--is-ancestor", branch, base]).await?;
    Ok(output.success)
}

/// Files changed, insertions and deletions between the merge base of `base`
/// and `branch` and the branch tip.
///
/// Three dots, not two: this is the "what would this PR change" diff the
/// review view shows (ADR-0013), so a base branch that moved on after the
/// worktree was created must not appear as work the agent undid.
pub(super) async fn diff_stat(dir: &Path, base: &str, branch: &str) -> Result<DiffStat> {
    let (stat, _) = diff(dir, base, branch).await?;
    Ok(stat)
}

/// The same diff as [`diff_stat`], plus the per-file breakdown task 015's run
/// detail view shows — one invocation of `git diff --numstat` rather than two,
/// since the aggregate is nothing but a sum over these same rows.
pub(super) async fn diff(
    dir: &Path,
    base: &str,
    branch: &str,
) -> Result<(DiffStat, Vec<FileDiffStat>)> {
    let range = format!("{base}...{branch}");
    let stdout = checked(dir, &["diff", "--numstat", &range]).await?;
    Ok(parse_numstat(&stdout))
}

/// The commits on `branch` that are not on `base`, newest first — ADR-0013's
/// "the commits made on the branch".
pub(super) async fn commits(dir: &Path, base: &str, branch: &str) -> Result<Vec<CommitSummary>> {
    let range = format!("{base}..{branch}");
    let stdout = checked(dir, &["log", LOG_FORMAT, &range]).await?;
    Ok(parse_log(&stdout))
}

/// The command line for a log line, and **only** for a log line — the vector
/// above is what actually runs, so nothing here needs to be re-parseable or
/// shell-quotable.
fn command_line<S: AsRef<OsStr>>(args: &[S]) -> String {
    args.iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses `git worktree list --porcelain`: one blank-line-separated stanza per
/// worktree, each opening with `worktree <path>` and optionally carrying
/// `branch refs/heads/<name>`, `detached`, `bare`, `locked` or `prunable`.
///
/// Line-oriented rather than `-z`, and safe for the paths this app produces:
/// everything after `worktree ` is the path, so a space in it survives. A path
/// containing a literal newline would not, which is why nothing here *writes*
/// such a path — the components are a task id and a repository slug.
fn parse_worktree_list(stdout: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            if let Some(path) = path.take() {
                entries.push(WorktreeEntry {
                    path,
                    branch: branch.take(),
                });
            }
            path = Some(PathBuf::from(rest));
            branch = None;
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.strip_prefix("refs/heads/").unwrap_or(rest).to_string());
        }
    }

    if let Some(path) = path {
        entries.push(WorktreeEntry { path, branch });
    }
    entries
}

/// Parses `git diff --numstat`: `<added>\t<deleted>\t<path>` per file, with
/// `-` in both counts for a binary file. A binary file still counts as one
/// file changed — it is a change the reviewer has to look at — and contributes
/// no lines, because it has none; [`FileDiffStat`] carries that as `None`
/// rather than `0`, so a per-file breakdown can tell "no lines" from "not a
/// text file" apart.
///
/// `splitn(3, ...)` rather than `split('\t')`: the path is the third field and
/// is taken whole, including any tab a pathological filename might itself
/// contain, instead of being cut at the first one.
fn parse_numstat(stdout: &str) -> (DiffStat, Vec<FileDiffStat>) {
    let mut stat = DiffStat::default();
    let mut files = Vec::new();

    for line in stdout.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.splitn(3, '\t');
        let insertions = fields.next().and_then(|n| n.parse::<i64>().ok());
        let deletions = fields.next().and_then(|n| n.parse::<i64>().ok());
        let path = fields.next().unwrap_or_default().to_string();

        stat.files_changed += 1;
        stat.insertions += insertions.unwrap_or(0);
        stat.deletions += deletions.unwrap_or(0);
        files.push(FileDiffStat {
            path,
            insertions,
            deletions,
        });
    }

    (stat, files)
}

/// Parses [`LOG_FORMAT`]. A record whose timestamp does not parse is dropped
/// rather than failing the whole read: a review view that shows nothing
/// because one commit had an unrepresentable commit date is worse than one
/// that shows the rest.
fn parse_log(stdout: &str) -> Vec<CommitSummary> {
    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let mut fields = line.split(LOG_FIELD_SEPARATOR);
            let sha = fields.next()?.to_string();
            let short_sha = fields.next()?.to_string();
            let author = fields.next()?.to_string();
            let committed_at = DateTime::parse_from_rfc3339(fields.next()?)
                .ok()?
                .with_timezone(&Utc);
            let subject = fields.next().unwrap_or_default().to_string();
            Some(CommitSummary {
                sha,
                short_sha,
                author,
                committed_at,
                subject,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_worktree_list_reports_the_main_tree_and_every_linked_one() {
        let stdout = "\
worktree /Users/someone/Code/work tree
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /Users/someone/Library/Application Support/rimaia/worktrees/repo/task-1
HEAD 2222222222222222222222222222222222222222
branch refs/heads/rimaia/task-1-add-the-parser
";

        let entries = parse_worktree_list(stdout);

        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].path,
            PathBuf::from("/Users/someone/Code/work tree"),
            "a path containing a space is the whole rest of the line"
        );
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(
            entries[1].branch.as_deref(),
            Some("rimaia/task-1-add-the-parser"),
            "refs/heads/ is stripped, the namespaced remainder is not"
        );
    }

    #[test]
    fn a_detached_worktree_reports_no_branch() {
        let stdout = "\
worktree /tmp/detached
HEAD 3333333333333333333333333333333333333333
detached
";

        let entries = parse_worktree_list(stdout);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch, None);
    }

    #[test]
    fn an_empty_worktree_list_parses_to_no_entries() {
        assert!(parse_worktree_list("").is_empty());
    }

    #[test]
    fn a_numstat_sums_insertions_and_deletions_across_files() {
        let (stat, files) = parse_numstat("12\t3\tsrc/lib.rs\n0\t7\tsrc/old.rs\n");

        assert_eq!(
            stat,
            DiffStat {
                files_changed: 2,
                insertions: 12,
                deletions: 10,
            }
        );
        assert_eq!(
            files,
            vec![
                FileDiffStat {
                    path: "src/lib.rs".to_string(),
                    insertions: Some(12),
                    deletions: Some(3),
                },
                FileDiffStat {
                    path: "src/old.rs".to_string(),
                    insertions: Some(0),
                    deletions: Some(7),
                },
            ]
        );
    }

    #[test]
    fn a_binary_file_counts_as_changed_but_contributes_no_lines() {
        let (stat, files) = parse_numstat("-\t-\tlogo.png\n4\t0\tREADME.md\n");

        assert_eq!(
            stat,
            DiffStat {
                files_changed: 2,
                insertions: 4,
                deletions: 0,
            }
        );
        assert_eq!(
            files[0],
            FileDiffStat {
                path: "logo.png".to_string(),
                // `None`, not `0` — a binary file has no line counts at all,
                // which is a different fact from "zero lines changed".
                insertions: None,
                deletions: None,
            }
        );
    }

    #[test]
    fn a_path_containing_a_tab_is_still_taken_whole() {
        let (_, files) = parse_numstat("1\t2\tsrc/weird\tname.rs\n");

        assert_eq!(files[0].path, "src/weird\tname.rs");
    }

    #[test]
    fn an_empty_diff_is_all_zeroes() {
        let (stat, files) = parse_numstat("");
        assert_eq!(stat, DiffStat::default());
        assert!(files.is_empty());
    }

    #[test]
    fn a_log_record_splits_into_its_five_fields() {
        let stdout = "\
4444444444444444444444444444444444444444\u{1f}4444444\u{1f}Rimaia Test\u{1f}2026-08-20T14:05:00+02:00\u{1f}Add the parser
5555555555555555555555555555555555555555\u{1f}5555555\u{1f}Rimaia Test\u{1f}2026-08-20T13:00:00+00:00\u{1f}Initial commit
";

        let commits = parse_log(stdout);

        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].short_sha, "4444444");
        assert_eq!(commits[0].author, "Rimaia Test");
        assert_eq!(commits[0].subject, "Add the parser");
        assert_eq!(
            commits[0].committed_at,
            "2026-08-20T12:05:00Z"
                .parse::<DateTime<Utc>>()
                .expect("a literal timestamp"),
            "the committer's offset is normalized to UTC, not carried"
        );
    }

    #[test]
    fn a_subject_containing_the_field_separator_would_not_split_a_record_short() {
        // ASCII unit separator cannot reach a commit subject through git, so the
        // only thing this pins is that extra fields are ignored rather than
        // shifting the parse — a defence against a future format string that
        // appends something.
        let stdout = "6666666666666666666666666666666666666666\u{1f}6666666\u{1f}A\u{1f}2026-08-20T00:00:00+00:00\u{1f}Subject\u{1f}extra\n";

        let commits = parse_log(stdout);

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "Subject");
    }

    #[test]
    fn a_record_with_an_unparseable_timestamp_is_dropped_and_the_rest_survive() {
        let stdout = "\
7777777777777777777777777777777777777777\u{1f}7777777\u{1f}A\u{1f}not-a-timestamp\u{1f}Broken
8888888888888888888888888888888888888888\u{1f}8888888\u{1f}A\u{1f}2026-08-20T00:00:00+00:00\u{1f}Fine
";

        let commits = parse_log(stdout);

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "Fine");
    }
}
