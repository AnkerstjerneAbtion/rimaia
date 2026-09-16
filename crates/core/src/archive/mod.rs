//! What one archive cleans up (ADR-0025 point 4, task 030).
//!
//! `tasks::service` owns *archiving* — the stamp, the guard, the report. This
//! module owns the thing that happens **after** the stamp has committed: none
//! of it, Rimaia's own guarded worktree removal, or an executable the
//! repository named.
//!
//! # The split, and why it runs this way round
//!
//! Seam-contract D20 point 3 already put "the worktree disappears when I move
//! the card" in `tasks::move_task` rather than in a command, so the board and
//! the MCP server get it identically. The same argument puts this here: the
//! policy belongs to the *transition*, and a rule enforced on one door is a bug
//! (ADR-0006).
//!
//! The action cannot fail the archive. The row is stamped, committed and
//! published before anything here is called — a cleanup a guard declined must
//! not be able to report the archive as having failed. It differs from
//! [`worktree::cleanup::auto_remove_on_done`](crate::worktree::cleanup) in
//! exactly one way, and the difference is about who asked: automatic cleanup on
//! `done` is silent because the user was moving a card, while an archive's
//! action is **reported**, because the user clicked a button whose label said
//! what it would do.
//!
//! # The two modes are not two implementations of one thing
//!
//! [`OnArchive::RemoveWorktree`] delegates to
//! [`worktree::cleanup::remove_worktree`](crate::worktree::cleanup::remove_worktree)
//! with `RemovalAuthorization::default()`, so all four of D20 point 1's guards
//! hold and the branch is always kept. [`OnArchive::Script`] hands the task's
//! paths to a program Rimaia did not write and **bypasses every one of those
//! guards**. That is the deal ADR-0025 point 4 states rather than an oversight:
//! a script Rimaia guarded could not do the teardown it was written for, and a
//! script Rimaia ran cleanup after would be Rimaia removing a directory its
//! owner had already removed. The obligation the asymmetry creates is on the
//! Settings copy, which has to say so in those words.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::context::ServiceContext;
use crate::db::{OnArchive, Repository, Task};
use crate::error::{Error, Result};
use crate::runner::process::{set_process_group, signal_group, strip_process_identity, Signal};
use crate::worktree::cleanup::{remove_worktree, RemovalAuthorization};

/// How long a configured script may run before it is stopped (seam-contract
/// D26.5).
///
/// **The first wall-clock timeout in this codebase, and scoped to this.**
/// Nothing else here has one: `worktree::git::run` is bounded by git, and a run
/// is bounded by ADR-0010's window and `MAX_TURNS` *on purpose* — the runner's
/// own grace period says in its doc that it "is not a timeout on the run
/// itself". An archive hook has neither bound and sits in front of a user
/// waiting for a board to update, so it gets the ordinary answer.
pub const SCRIPT_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the script has to exit after `TERM` before it is sent `KILL`.
///
/// The runner's number, deliberately: a script that traps `TERM` to flush
/// something is doing what the runner's own children do, and two different
/// answers to "how long is politeness" would be two numbers to keep in step.
pub use crate::runner::process::DEFAULT_GRACE_PERIOD as SCRIPT_GRACE_PERIOD;

/// How much of a script's output is carried back to the caller.
///
/// A tail rather than the whole thing: the useful part of a failing cleanup is
/// the last thing it said, and a script that prints a megabyte would otherwise
/// put a megabyte through a Tauri event on every archive.
const OUTPUT_TAIL_BYTES: usize = 4 * 1024;

/// What the configured cleanup did, carried back on the archive itself.
///
/// Reported rather than logged, because ADR-0025 point 6 makes this the half of
/// the operation the user explicitly asked for. `Failed` is a *reported*
/// failure and never an `Err`: the archive it belongs to has already committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum OnArchiveOutcome {
    /// The repository asked for nothing, or the task had no worktree to act on.
    Nothing,
    /// [`OnArchive::RemoveWorktree`], and every guard was satisfied.
    WorktreeRemoved { bytes_freed: u64 },
    /// [`OnArchive::Script`] ran to completion. A non-zero `exit_code` is still
    /// this variant — the script ran, it just did not like what it found, and
    /// that is information rather than a Rimaia failure.
    ///
    /// `exit_code` is `None` for a script killed by a signal, which includes
    /// the one this module sends at [`SCRIPT_TIMEOUT`].
    ScriptRan {
        exit_code: Option<i32>,
        output: String,
    },
    /// A guard refused, or the script could not be started at all. The sentence
    /// is the one the user reads.
    Failed { reason: String },
}

impl OnArchiveOutcome {
    /// Whether this is worth putting in front of the user as a problem.
    ///
    /// A non-zero exit counts. The user configured a cleanup and it reported
    /// that it did not work, which is the same news as a guard refusing — the
    /// distinction between the two matters to this module and not to the person
    /// reading the board.
    pub fn needs_attention(&self) -> bool {
        match self {
            OnArchiveOutcome::Nothing | OnArchiveOutcome::WorktreeRemoved { .. } => false,
            OnArchiveOutcome::ScriptRan { exit_code, .. } => *exit_code != Some(0),
            OnArchiveOutcome::Failed { .. } => true,
        }
    }
}

// ---------------------------------------------------------------------------
// The policy, per repository
// ---------------------------------------------------------------------------

/// Sets what archiving a task in this repository cleans up.
///
/// The script path is validated **here**, when the setting is written, rather
/// than when an archive fires: a path that is relative, missing, a directory or
/// not executable is a form error at 11am, not a surprise at 3am. It is also
/// the only moment at which there is a human present to read the sentence.
///
/// The two arguments are one decision — passing [`OnArchive::Script`] without a
/// path, or a path with any other mode, is refused rather than silently
/// half-applied, because ADR-0025 point 4 makes this one slot and a row that
/// spells `script` with nothing to run is not one of its three states.
pub async fn set_repository_on_archive(
    ctx: &ServiceContext,
    repository_id: &str,
    on_archive: OnArchive,
    script: Option<String>,
) -> Result<Repository> {
    let script = match (on_archive, script) {
        (OnArchive::Script, Some(path)) => Some(validate_script_path(&path).await?),
        (OnArchive::Script, None) => {
            return Err(Error::invalid(
                "running a script on archive needs the path of the script to run",
            ))
        }
        // Not an error, and deliberately not preserved either. Switching away
        // from `script` clears the path, so the row cannot hold a stale command
        // that a later switch back would silently re-arm without anyone having
        // looked at it again.
        (_, _) => None,
    };

    let stored = on_archive.as_str();
    let updated = sqlx::query!(
        "UPDATE repositories SET on_archive = ?1, on_archive_script = ?2 WHERE id = ?3",
        stored,
        script,
        repository_id,
    )
    .execute(&ctx.pool)
    .await?
    .rows_affected();

    if updated == 0 {
        return Err(Error::not_found(format!(
            "no repository with id {repository_id}"
        )));
    }

    ctx.publish(crate::events::ChangeEvent::repositories([
        repository_id.to_string()
    ]));
    crate::repo::get(ctx, repository_id).await
}

/// The four properties a stored script path must have, checked against the
/// filesystem rather than against the string.
///
/// Absolute and `..`-free comes from [`crate::worktree::safety::resolve`],
/// which already owns that rule and already explains why a text prefix check is
/// the wrong tool. The remaining two are this module's: it has to be a file,
/// and it has to be executable, because `Command::new` on either of the other
/// two fails at 3am with an `io::Error` nobody is awake to read.
async fn validate_script_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid(
            "running a script on archive needs the path of the script to run",
        ));
    }

    let resolved = crate::worktree::safety::resolve(Path::new(trimmed)).await?;

    let metadata = tokio::fs::metadata(&resolved).await.map_err(|error| {
        Error::invalid(format!(
            "cannot use {} as an on-archive script: {error}",
            resolved.display()
        ))
    })?;

    if !metadata.is_file() {
        return Err(Error::invalid(format!(
            "cannot use {} as an on-archive script: it is a directory, not a program",
            resolved.display()
        )));
    }

    if !is_executable(&metadata) {
        return Err(Error::invalid(format!(
            "cannot use {} as an on-archive script: it is not executable. \
             `chmod +x` it, and give it a `#!` line naming the shell it expects.",
            resolved.display()
        )));
    }

    Ok(resolved.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

/// No executable bit exists to check. ADR-0004 already records that Windows
/// needs its own answer in the process module; this is honestly the same gap
/// rather than a check pretending to have run.
#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

// ---------------------------------------------------------------------------
// Running it
// ---------------------------------------------------------------------------

/// Runs whatever `task`'s repository configured, after the archive committed.
///
/// Never returns `Err`. Everything that can go wrong is an
/// [`OnArchiveOutcome::Failed`] the caller carries back to the user, for the
/// reason the module header gives: the archive has already happened, and a
/// `?` here would be a subprocess putting a committed transaction in doubt.
pub async fn run_on_archive(ctx: &ServiceContext, task: &Task) -> OnArchiveOutcome {
    let repository = match crate::repo::get(ctx, &task.repository_id).await {
        Ok(repository) => repository,
        Err(error) => {
            return OnArchiveOutcome::Failed {
                reason: format!("could not read the repository's archive policy: {error}"),
            }
        }
    };

    match repository.on_archive {
        OnArchive::None => OnArchiveOutcome::Nothing,
        OnArchive::RemoveWorktree => remove_this_worktree(ctx, task).await,
        OnArchive::Script => match repository.on_archive_script.as_deref() {
            Some(script) => run_script(ctx, &repository, task, script).await,
            // A row spelling `script` with no path should be unreachable —
            // `set_repository_on_archive` refuses to write one — so this is the
            // hand-edited-database case D-for-tolerance covers everywhere else:
            // say so, do nothing, and never panic on the user's own sqlite3.
            None => OnArchiveOutcome::Failed {
                reason: "this repository is set to run a script on archive but names none"
                    .to_string(),
            },
        },
    }
}

/// [`OnArchive::RemoveWorktree`], with every force off and the branch kept.
///
/// `RemovalAuthorization::default()` and nothing else, for D20 point 3's
/// reason: an automatic action gets strictly less authority than a human
/// clicking a button, because there is nobody present to read the refusal it
/// would otherwise be overriding.
async fn remove_this_worktree(ctx: &ServiceContext, task: &Task) -> OnArchiveOutcome {
    if task.worktree_path.is_none() {
        return OnArchiveOutcome::Nothing;
    }

    match remove_worktree(ctx, &task.id, RemovalAuthorization::default()).await {
        Ok(removed) => OnArchiveOutcome::WorktreeRemoved {
            bytes_freed: removed.bytes_freed,
        },
        Err(error) => OnArchiveOutcome::Failed {
            reason: error.to_string(),
        },
    }
}

/// Spawns the configured executable and waits for it, bounded by
/// [`SCRIPT_TIMEOUT`].
///
/// The contract is seam-contract D26.4's table, and every row of it is a
/// decision:
///
/// - **An argv of one.** ADR-0025 point 5 — the setting holds a path, never a
///   command line, because splitting a command line correctly is shell quoting
///   and CLAUDE.md forbids `sh -c` for a reason that starts with repository
///   paths containing spaces. A user who wants a pipeline writes it inside
///   their own script, behind their own `#!`.
/// - **The repository root as cwd, never the worktree.** The worktree may be
///   the thing the script is deleting, and a process whose working directory
///   has been removed is in a state nothing good comes of.
/// - **Context in the environment, not in arguments.** Extensible without
///   breaking a positional contract: a script that ignores a variable it does
///   not know about is the normal case, while a script that mis-reads `$4` is
///   silent corruption.
/// - **`CLAUDE_*` stripped**, through the runner's own helper. Those are
///   process identity rather than user configuration, and that rule was never
///   runner-specific.
/// - **Its own process group**, so a script that spawns children can be stopped
///   by one signal rather than leaking them.
async fn run_script(
    ctx: &ServiceContext,
    repository: &Repository,
    task: &Task,
    script: &str,
) -> OnArchiveOutcome {
    let mut command = Command::new(script);
    command
        .current_dir(&repository.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    strip_process_identity(&mut command);
    set_process_group(&mut command);

    command
        .env("RIMAIA_TASK_ID", &task.id)
        .env("RIMAIA_TASK_TITLE", &task.title)
        .env("RIMAIA_REPOSITORY_PATH", &repository.path)
        .env("RIMAIA_BRANCH", task.branch.clone().unwrap_or_default())
        .env(
            "RIMAIA_WORKTREE_PATH",
            task.worktree_path.clone().unwrap_or_default(),
        );

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return OnArchiveOutcome::Failed {
                reason: format!("could not run the on-archive script {script}: {error}"),
            }
        }
    };

    let group = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Drained concurrently rather than after the wait: a script chatty enough
    // to fill a pipe buffer would otherwise block on its own output and then be
    // killed at the timeout for a deadlock we created.
    let collector = tokio::spawn(collect(stdout, stderr));

    let deadline = ctx.clock.now() + chrono::Duration::from_std(SCRIPT_TIMEOUT).unwrap_or_default();
    let status = tokio::select! {
        status = child.wait() => status,
        // The injected clock, not `tokio::time::sleep`: CLAUDE.md forbids
        // `sleep` in tests, and "a script that never exits is killed" is one of
        // the behaviours that has to be tested.
        _ = ctx.clock.sleep_until(deadline) => {
            stop(&mut child, group, ctx).await
        }
    };

    let output = collector.await.unwrap_or_default();

    match status {
        Ok(status) => OnArchiveOutcome::ScriptRan {
            exit_code: status.code(),
            output,
        },
        Err(error) => OnArchiveOutcome::Failed {
            reason: format!("could not wait for the on-archive script {script}: {error}"),
        },
    }
}

/// `TERM` to the group, a grace period, then `KILL` — the runner's own
/// escalation, reached through the runner's own `kill` argv rather than a
/// second copy of it here.
async fn stop(
    child: &mut tokio::process::Child,
    group: Option<u32>,
    ctx: &ServiceContext,
) -> std::io::Result<std::process::ExitStatus> {
    tracing::warn!(
        timeout_secs = SCRIPT_TIMEOUT.as_secs(),
        "the on-archive script outlasted its timeout; stopping it",
    );
    signal_group(group, Signal::Term).await;

    let grace =
        ctx.clock.now() + chrono::Duration::from_std(SCRIPT_GRACE_PERIOD).unwrap_or_default();
    tokio::select! {
        status = child.wait() => status,
        _ = ctx.clock.sleep_until(grace) => {
            signal_group(group, Signal::Kill).await;
            child.wait().await
        }
    }
}

/// Both streams, stdout then stderr, capped to a tail.
///
/// **Not run through [`credentials::redact`](crate::credentials::redact), and
/// that is a decision rather than an omission.** The runner redacts because
/// *it* puts a token into the child's environment and then writes the child's
/// output to a transcript file. Neither half is true here: Rimaia hands an
/// on-archive script no credential at all, and the output goes to the same
/// person who wrote the script, on their own machine, without touching disk.
/// Redacting anyway would mean reading the keychain on an archive — a new
/// failure mode, and on macOS a possible prompt — to scrub a value we never
/// supplied.
async fn collect(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
) -> String {
    let mut collected = String::new();
    if let Some(stdout) = stdout {
        read_into(&mut collected, BufReader::new(stdout)).await;
    }
    if let Some(stderr) = stderr {
        read_into(&mut collected, BufReader::new(stderr)).await;
    }
    tail(&collected)
}

async fn read_into<R>(collected: &mut String, reader: BufReader<R>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        collected.push_str(&line);
        collected.push('\n');
    }
}

/// The last [`OUTPUT_TAIL_BYTES`] of `output`, cut at a line boundary.
///
/// Cut at a boundary because the alternative is slicing a UTF-8 sequence in
/// half, and a panic in a cleanup path is the worst possible place for one.
fn tail(output: &str) -> String {
    if output.len() <= OUTPUT_TAIL_BYTES {
        return output.to_string();
    }

    let start = output.len() - OUTPUT_TAIL_BYTES;
    let cut = output[start..]
        .find('\n')
        .map(|offset| start + offset + 1)
        .unwrap_or_else(|| {
            // No newline in the tail at all — one enormous line. Walk forward to
            // the next character boundary rather than slicing mid-codepoint.
            let mut index = start;
            while index < output.len() && !output.is_char_boundary(index) {
                index += 1;
            }
            index
        });

    format!("…\n{}", &output[cut..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_short_output_is_carried_whole() {
        assert_eq!(tail("removed 2 volumes\n"), "removed 2 volumes\n");
    }

    #[test]
    fn a_long_output_keeps_its_end_and_says_it_was_cut() {
        let long = "x".repeat(OUTPUT_TAIL_BYTES) + "\nthe last thing it said\n";
        let tailed = tail(&long);

        assert!(tailed.starts_with("…\n"), "{tailed}");
        assert!(tailed.ends_with("the last thing it said\n"), "{tailed}");
    }

    #[test]
    fn cutting_a_line_that_is_longer_than_the_cap_never_splits_a_codepoint() {
        // Multi-byte throughout and no newline anywhere, which is the case that
        // slices mid-sequence if the cut is taken on a byte offset.
        let long = "é".repeat(OUTPUT_TAIL_BYTES);

        let tailed = tail(&long);

        assert!(tailed.starts_with("…\n"), "{tailed}");
    }

    #[test]
    fn a_non_zero_exit_needs_attention_and_a_clean_one_does_not() {
        // The distinction between "a guard refused" and "your script said no"
        // matters to this module and not to the person reading the board.
        assert!(OnArchiveOutcome::ScriptRan {
            exit_code: Some(1),
            output: String::new(),
        }
        .needs_attention());
        assert!(!OnArchiveOutcome::ScriptRan {
            exit_code: Some(0),
            output: String::new(),
        }
        .needs_attention());
        assert!(OnArchiveOutcome::ScriptRan {
            exit_code: None,
            output: String::new(),
        }
        .needs_attention());
        assert!(!OnArchiveOutcome::Nothing.needs_attention());
        assert!(OnArchiveOutcome::Failed {
            reason: "refused".to_string(),
        }
        .needs_attention());
    }

    #[tokio::test]
    async fn a_relative_script_path_is_refused_when_it_is_written() {
        let error = validate_script_path("./cleanup.sh")
            .await
            .expect_err("a relative path must be refused");

        assert!(
            error.to_string().contains("absolute"),
            "the refusal must say what is wrong: {error}"
        );
    }

    #[tokio::test]
    async fn an_empty_script_path_is_refused() {
        let error = validate_script_path("   ")
            .await
            .expect_err("an empty path must be refused");

        assert!(error.to_string().contains("needs the path"), "{error}");
    }

    #[tokio::test]
    async fn a_directory_is_not_a_program() {
        let dir = tempfile::tempdir().expect("temp dir");

        let error = validate_script_path(&dir.path().to_string_lossy())
            .await
            .expect_err("a directory must be refused");

        assert!(error.to_string().contains("not a program"), "{error}");
    }

    // There is no executable bit to be missing on Windows, where
    // `is_executable` says so rather than pretending to have checked — so this
    // asserts a refusal that platform does not make.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_without_an_executable_bit_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let script = dir.path().join("cleanup.sh");
        tokio::fs::write(&script, "#!/bin/sh\nexit 0\n")
            .await
            .expect("write");

        let error = validate_script_path(&script.to_string_lossy())
            .await
            .expect_err("a non-executable file must be refused");

        assert!(error.to_string().contains("chmod +x"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_executable_file_is_accepted_and_comes_back_canonicalized() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let script = dir.path().join("cleanup.sh");
        tokio::fs::write(&script, "#!/bin/sh\nexit 0\n")
            .await
            .expect("write");
        tokio::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .await
            .expect("chmod");

        let accepted = validate_script_path(&script.to_string_lossy())
            .await
            .expect("an executable file is a usable script");

        assert!(accepted.ends_with("cleanup.sh"), "{accepted}");
        assert!(Path::new(&accepted).is_absolute(), "{accepted}");
    }
}
