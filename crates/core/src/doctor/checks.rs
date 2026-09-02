//! The eight checks, one function each, every input injected.
//!
//! Each function corresponds to one row of task 018's table and to one way an
//! overnight queue can waste a night. None of them reads the environment for
//! itself: a program arrives as a path, a directory as an [`AppPaths`], a
//! repository as a row, the MCP endpoint as an `Option<&str>` already read off
//! the live handle table. That is what lets a test point a check at a `TempDir`
//! and a renamed binary rather than at whatever the machine running the suite
//! happens to have installed.
//!
//! Every pass/warn/fail assignment below is argued in seam-contract D22, not
//! here — a reviewer needs one place to check them against, and a doc comment
//! in the module that made the choice is not a place another task can inherit.

use std::path::Path;

use tokio::process::Command;

use crate::db::Repository;
use crate::error::Result;
use crate::paths::AppPaths;
use crate::repo::{self, GhStatus};
use crate::runner::probe_cli;
use crate::runner::process::strip_process_identity;

use super::{Check, CheckResult};

/// The CLI the spike measured against (`spike/FINDINGS.md`, 2026-08-20), and
/// therefore the oldest `claude` any of this repository's parsing has been
/// observed to work with. ADR-0004 asks for a pinned minimum; this is the only
/// number there is evidence for.
///
/// Falling below it is a **warning**, never a refusal — see [`claude_cli`].
pub const MINIMUM_CLAUDE_VERSION: (u32, u32, u32) = (2, 1, 234);

/// `git worktree remove` landed in git 2.17.0, and
/// [`crate::worktree`] uses it on every cleanup. `git worktree add`
/// (2.5) and `git worktree list --porcelain` (2.7) are both older, so 2.17 is
/// the binding one of the three.
pub const MINIMUM_GIT_VERSION: (u32, u32, u32) = (2, 17, 0);

/// Below this, [`disk_space`] fails. One gigabyte is not "enough to work
/// comfortably"; it is roughly the floor beneath which cloning a worktree of an
/// ordinary repository and writing a night of JSONL transcripts alongside it
/// stops being possible at all.
pub const USABLE_DISK_BYTES: u64 = 1024 * 1024 * 1024;

/// Below this, [`disk_space`] warns. Five gigabytes is a night's headroom for
/// several worktrees and their transcripts, not a hard requirement.
pub const ROOMY_DISK_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// The name of the file [`data_directory`] writes and deletes to prove the
/// directory is writable. Fixed rather than random so a crash between the write
/// and the delete leaves one stale file that the next probe overwrites, instead
/// of accumulating one per launch.
const WRITE_PROBE_FILE: &str = ".rimaia-write-probe";

type Version = (u32, u32, u32);

/// The first `major.minor[.patch]` in `text`.
///
/// Hand-rolled rather than a `semver` dependency, which seam-contract D6 (as
/// extended to Cargo by D16.3) would need an entry for and which would buy
/// nothing: neither tool prints a semver string. `git` prints
/// `git version 2.39.5 (Apple Git-154)` on macOS and
/// `git version 2.50.0.windows.1` on Windows, and `claude` prints
/// `2.1.258 (Claude Code)`. Taking the *first* such run of digits and dots is
/// what makes all three read correctly — the Apple build number is second, and
/// the Windows suffix is separated by a non-digit so it never joins the patch.
///
/// A missing patch reads as `0`. `git version 2.50` is not something any git in
/// living memory prints, but treating it as unparseable would turn a version
/// that is obviously new enough into a warning.
fn parse_version(text: &str) -> Option<Version> {
    text.split(|character: char| !character.is_ascii_digit() && character != '.')
        .filter(|token| !token.is_empty())
        .find_map(|token| {
            let mut parts = token.split('.');
            let major = parts.next()?.parse::<u32>().ok()?;
            let minor = parts.next()?.parse::<u32>().ok()?;
            let patch = parts
                .next()
                .and_then(|part| part.parse::<u32>().ok())
                .unwrap_or(0);
            Some((major, minor, patch))
        })
}

fn format_version((major, minor, patch): Version) -> String {
    format!("{major}.{minor}.{patch}")
}

/// Human bytes, for a disk-space row nobody should have to divide by 1024.
fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = (1024 * 1024 * 1024) as f64;
    const MIB: f64 = (1024 * 1024) as f64;
    if bytes as f64 >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MB", bytes as f64 / MIB)
    }
}

/// `claude` on `PATH`, and new enough (ADR-0004).
///
/// Reuses [`probe_cli`] rather than spawning `--version` a second way, so the
/// doctor and the queue's own per-step probe cannot disagree about whether the
/// binary runs — and so the message a missing binary produces is the one
/// `runner::process` already wrote, rather than a second wording of it.
///
/// **An old version warns, it does not fail.** The minimum is evidence about
/// what has been *tested*, not a claim about what breaks: a CLI one patch below
/// it will almost certainly work, the parser is tolerant of unknown events by
/// design (ADR-0004), and locking the user out of their own queue over a
/// version string is a worse outcome than the risk it avoids. A binary that
/// cannot be run at all is a different question and does fail.
pub async fn claude_cli(program: &Path) -> CheckResult {
    let version_output = match probe_cli(program).await {
        Ok(output) => output,
        Err(error) => {
            return CheckResult::fail(
                Check::ClaudeCli,
                error.to_string(),
                "Install Claude Code and check that `claude --version` runs in a terminal, \
                 then press Re-check. Rimaia drives your own installation and never bundles one.",
            );
        }
    };

    match parse_version(&version_output) {
        Some(version) if version >= MINIMUM_CLAUDE_VERSION => {
            CheckResult::pass(Check::ClaudeCli, format!("{version_output} on PATH."))
        }
        Some(version) => CheckResult::warn(
            Check::ClaudeCli,
            format!(
                "Claude Code {} is older than {}, the oldest version Rimaia's event parsing has \
                 been tested against.",
                format_version(version),
                format_version(MINIMUM_CLAUDE_VERSION),
            ),
            "Run `claude update`. Runs may well work as they are — this is the version Rimaia \
             was measured against, not a version it is known to break below.",
        ),
        None => CheckResult::warn(
            Check::ClaudeCli,
            format!("`claude --version` printed something unrecognisable: {version_output}"),
            "Check that `claude --version` prints a version number in a terminal. The CLI runs, \
             so runs will most likely still work.",
        ),
    }
}

/// Whether the CLI is signed in.
///
/// `claude auth status --json` answers `{"loggedIn": true, "authMethod": …}` and
/// exits zero (verified against Claude Code 2.1.258). That is the mechanism, and
/// it is a real one rather than a guess: it is non-interactive, it costs no
/// tokens, and it does not depend on parsing an error message the way ADR-0004
/// forbids for usage limits.
///
/// **Only an explicit `loggedIn: false` fails.** A non-zero exit, a missing
/// subcommand or unparseable output means the check could not be *performed* —
/// most likely a CLI old enough not to have `auth status` — and reporting "not
/// signed in" on that evidence would be the doctor lying, which is worse than
/// the doctor admitting a gap. Seam-contract D22 records the version dependency.
pub async fn claude_authenticated(program: &Path) -> CheckResult {
    let mut command = Command::new(program);
    command.args(["auth", "status", "--json"]);
    // The same rule every other child of Rimaia's follows — see
    // `strip_process_identity`'s own doc.
    strip_process_identity(&mut command);

    let undetermined = |detail: String| {
        CheckResult::warn(
            Check::ClaudeAuthenticated,
            detail,
            "Run `claude auth status` in a terminal. If it is not a known command, this CLI is \
             older than the check and the sign-in cannot be verified from here — `claude` itself \
             will still tell you at the start of a run.",
        )
    };

    let output = match command.output().await {
        Ok(output) => output,
        Err(error) => {
            return undetermined(format!(
                "the sign-in could not be checked because `{}` could not be run: {error}",
                program.display()
            ));
        }
    };
    if !output.status.success() {
        return undetermined(format!(
            "`claude auth status --json` exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stdout) else {
        return undetermined(
            "`claude auth status --json` did not answer with JSON, so the sign-in could not be \
             verified."
                .to_string(),
        );
    };
    let Some(logged_in) = parsed.get("loggedIn").and_then(serde_json::Value::as_bool) else {
        return undetermined(
            "`claude auth status --json` answered without a `loggedIn` field, so the sign-in \
             could not be verified."
                .to_string(),
        );
    };

    if !logged_in {
        return CheckResult::fail(
            Check::ClaudeAuthenticated,
            "Claude Code is installed but not signed in, so every run would fail with an \
             authentication error."
                .to_string(),
            "Run `claude auth login` in a terminal, then press Re-check. Rimaia never handles \
             your credentials — it uses the sign-in the CLI already has.",
        );
    }

    let method = parsed
        .get("authMethod")
        .and_then(serde_json::Value::as_str)
        .map(|method| format!(" via {method}"))
        .unwrap_or_default();
    CheckResult::pass(Check::ClaudeAuthenticated, format!("Signed in{method}."))
}

/// `git`, new enough for worktrees.
///
/// **Fails rather than warns below the minimum**, which is the opposite of
/// [`claude_cli`] and deliberately so: every task Rimaia runs begins by creating
/// a worktree (ADR-0005), so a `git` that cannot do it is not a risk to weigh,
/// it is every run failing at the same line. There is nothing for a user to
/// discover by being allowed to try.
pub async fn git(program: &Path) -> CheckResult {
    let mut command = Command::new(program);
    command.arg("--version");

    let output = match command.output().await {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        Ok(output) => {
            return CheckResult::fail(
                Check::Git,
                format!(
                    "`{} --version` exited {}: {}",
                    program.display(),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim(),
                ),
                "Install git, or repair the installation, then press Re-check. Every run starts \
                 by creating a git worktree.",
            );
        }
        Err(error) => {
            return CheckResult::fail(
                Check::Git,
                format!("git could not be run ({}): {error}", program.display()),
                "Install git and check that `git --version` runs in a terminal, then press \
                 Re-check. Every run starts by creating a git worktree.",
            );
        }
    };

    match parse_version(&output) {
        Some(version) if version >= MINIMUM_GIT_VERSION => {
            CheckResult::pass(Check::Git, format!("{output}."))
        }
        Some(version) => CheckResult::fail(
            Check::Git,
            format!(
                "git {} is older than {}, which is where `git worktree remove` was added.",
                format_version(version),
                format_version(MINIMUM_GIT_VERSION),
            ),
            "Upgrade git to 2.17 or newer, then press Re-check. Rimaia creates and removes a \
             worktree for every task and cannot do either with this version.",
        ),
        // The binary ran, so worktrees will most likely work; refusing to start
        // the queue over output this module failed to read would be blaming the
        // user for the parser.
        None => CheckResult::warn(
            Check::Git,
            format!(
                "`{} --version` printed no recognisable version: {output}",
                program.display()
            ),
            "Check that `git --version` prints a version number in a terminal. git runs, so \
             worktree creation will most likely still work.",
        ),
    }
}

/// The app data directory exists and can be written to.
///
/// Proved by writing a file, not by reading permission bits: on macOS a
/// directory can be `drwxr-xr-x` and owned by the user and still be unwritable
/// because of a sandbox or a full-disk-access refusal, and the permission bits
/// would have said yes.
pub fn data_directory(paths: &AppPaths) -> CheckResult {
    let data_dir = paths.data_dir();

    if let Err(error) = paths.create_all() {
        return CheckResult::fail(
            Check::DataDirectory,
            format!("{} could not be created: {error}", data_dir.display()),
            "Check the directory's permissions, and that its parent exists and Rimaia is allowed \
             to write there, then press Re-check. Nothing — not the database, not a single run \
             log — persists without it.",
        );
    }

    let probe = data_dir.join(WRITE_PROBE_FILE);
    if let Err(error) = std::fs::write(&probe, b"rimaia") {
        return CheckResult::fail(
            Check::DataDirectory,
            format!("{} is not writable: {error}", data_dir.display()),
            "Check the directory's permissions, and on macOS that Rimaia has access to the folder \
             it is in, then press Re-check. Nothing — not the database, not a single run log — \
             persists without it.",
        );
    }
    // A failure to clean up is not a failure of the check: the write succeeded,
    // which is the question. The next probe overwrites the file.
    let _ = std::fs::remove_file(&probe);

    CheckResult::pass(
        Check::DataDirectory,
        format!("{} is writable.", data_dir.display()),
    )
}

/// Room for tonight's worktrees and transcripts.
///
/// `fs4` because there is no std API for free space and ADR-0002 keeps Windows
/// and Linux viable, so a `statvfs` call written by hand here would be three
/// implementations. The import is confined to this one function — seam-contract
/// D22 records the dependency and this constraint.
pub fn disk_space(paths: &AppPaths) -> CheckResult {
    let data_dir = paths.data_dir();
    let available = match fs4::available_space(data_dir) {
        Ok(available) => available,
        Err(error) => {
            // Warn, not fail: this is the doctor failing to measure, not the
            // disk being full, and the two must not read the same on screen.
            return CheckResult::warn(
                Check::DiskSpace,
                format!(
                    "free space on {} could not be measured: {error}",
                    data_dir.display()
                ),
                "Check that the app data directory exists and is readable. Runs will still \
                 start; they will fail mid-run if the disk is in fact full.",
            );
        }
    };

    let where_it_is = format!("{} free on {}", format_bytes(available), data_dir.display());
    if available < USABLE_DISK_BYTES {
        CheckResult::fail(
            Check::DiskSpace,
            format!(
                "{where_it_is} — below the {} a worktree and a night of transcripts need.",
                format_bytes(USABLE_DISK_BYTES)
            ),
            "Free up disk space, then press Re-check. A run that fills the disk halfway through \
             leaves a half-written worktree and no branch to review.",
        )
    } else if available < ROOMY_DISK_BYTES {
        CheckResult::warn(
            Check::DiskSpace,
            format!(
                "{where_it_is} — enough to start, less than the {} a full night is comfortable \
                 with.",
                format_bytes(ROOMY_DISK_BYTES)
            ),
            "Free up disk space, or prune old run logs in Settings → Storage, before queueing a \
             long night.",
        )
    } else {
        CheckResult::pass(Check::DiskSpace, format!("{where_it_is}."))
    }
}

/// A registered repository is still where it was registered.
///
/// **Fails.** A row pointing at a directory that has been renamed or deleted is
/// not a state anyone chose, and its consequence is every task in that
/// repository dying at worktree creation. The named cost, so nobody meets it as
/// a surprise: a repository with no queued work still blocks the start.
/// Narrowing this to "only repositories with a `ready` task" would need the
/// selection plan the doctor deliberately does not read, and is left to whoever
/// finds the over-blocking worse than the wasted night (seam-contract D22).
pub async fn repository_path(repository: &Repository) -> Result<CheckResult> {
    Ok(match repo::path_problem(repository).await? {
        None => CheckResult::pass(
            Check::RepositoryPath,
            format!("{} is at {}.", repository.name, repository.path),
        )
        .about(&repository.name),
        Some(problem) => CheckResult::fail(
            Check::RepositoryPath,
            format!(
                "{} is registered at {}, but {problem}.",
                repository.name, repository.path
            ),
            format!(
                "Remove \"{}\" in Settings → Repositories and register it at its new path, then \
                 press Re-check. Every run in it would otherwise fail while creating its worktree.",
                repository.name
            ),
        )
        .about(&repository.name),
    })
}

/// `gh`, per repository.
///
/// **Warns, never fails**, and the reason is the one the task file gives
/// implicitly: a repository may not need a pull request at all. Base
/// instructions that ask for one already treat an unready `gh` as a reason to
/// skip that step rather than to fail the run (see [`repo::RemoteInfo`]), so a
/// blocking failure here would refuse a night's work over a step the run was
/// going to skip anyway. The warning names the repository because that is the
/// acceptance criterion, and because an installation-wide "gh is not
/// authenticated" tells the user nothing about which of five repositories is
/// affected.
pub async fn github_cli(repository: &Repository, program: &Path) -> Result<CheckResult> {
    let status = repo::gh_status(repository, program).await?;
    Ok(match status {
        GhStatus::Ready => CheckResult::pass(
            Check::GitHubCli,
            format!("gh is authenticated for {}'s remote.", repository.name),
        ),
        GhStatus::NoRemote => CheckResult::pass(
            Check::GitHubCli,
            format!(
                "{} has no remote host, so there is no pull request to open.",
                repository.name
            ),
        ),
        GhStatus::NotInstalled => CheckResult::warn(
            Check::GitHubCli,
            format!(
                "the GitHub CLI is not installed, so a run in {} cannot open a pull request.",
                repository.name
            ),
            "Install the GitHub CLI (`gh`) and run `gh auth login`, or leave it — a run whose \
             instructions ask for a pull request will skip that step and still leave its branch \
             to review.",
        ),
        GhStatus::NotAuthenticated => CheckResult::warn(
            Check::GitHubCli,
            format!(
                "gh is installed but not authenticated for {}'s remote, so a run in it cannot \
                 open a pull request.",
                repository.name
            ),
            "Run `gh auth login`, or leave it — a run whose instructions ask for a pull request \
             will skip that step and still leave its branch to review.",
        ),
    }
    .about(&repository.name))
}

/// Whether the MCP server actually bound (ADR-0006, seam-contract D16.7).
///
/// **Reads the bound endpoint; never attempts a bind of its own.** A doctor that
/// proved the port was free by taking it would race the server already holding
/// it — on the happy path it would find its own server and report the
/// installation broken, and on an unlucky one it would take the port for the
/// microsecond between bind and drop. `bound` is
/// [`RunHandles::endpoint`](crate::mcp::RunHandles::endpoint), which
/// `mcp::build` writes on every bind including the runtime rebind.
///
/// **Warns.** D16.7 already decided a busy port is surfaced rather than fatal to
/// startup, and the same argument holds one level up: nothing about running a
/// queued task needs the MCP server. What is lost is the handoff — writing plans
/// in from another session — and, for a `planned` task, the planner. Blocking
/// the whole night over that would refuse work that would have succeeded.
pub fn mcp_port(configured_port: u16, bound: Option<&str>) -> CheckResult {
    match bound {
        Some(endpoint) => CheckResult::pass(Check::McpPort, format!("Listening on {endpoint}.")),
        None => CheckResult::warn(
            Check::McpPort,
            format!(
                "nothing is listening on port {configured_port}, so plans cannot be handed to \
                 Rimaia from another Claude Code session and tasks set to `planned` cannot be \
                 planned."
            ),
            "Another program is most likely holding the port. Pick a free one in Settings → MCP, \
             or quit whatever is using it. Runs that are already queued are unaffected.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    use crate::doctor::{Check, CheckStatus};

    #[test]
    fn a_claude_version_line_parses_past_its_trailing_product_name() {
        assert_eq!(parse_version("2.1.258 (Claude Code)"), Some((2, 1, 258)));
    }

    #[test]
    fn an_apple_git_version_takes_the_git_version_not_the_apple_build_number() {
        assert_eq!(
            parse_version("git version 2.39.5 (Apple Git-154)"),
            Some((2, 39, 5))
        );
    }

    #[test]
    fn a_windows_git_version_stops_before_its_platform_suffix() {
        assert_eq!(
            parse_version("git version 2.50.0.windows.1"),
            Some((2, 50, 0))
        );
    }

    #[test]
    fn a_two_part_version_reads_its_missing_patch_as_zero() {
        assert_eq!(parse_version("git version 2.50"), Some((2, 50, 0)));
    }

    #[test]
    fn output_with_no_version_in_it_is_not_invented() {
        assert_eq!(parse_version("command not found"), None);
    }

    #[test]
    fn a_bound_mcp_endpoint_passes_and_an_unbound_one_only_warns() {
        // D16.7's argument one level up: a busy port costs the handoff, not the
        // night's work, so it must never be what refuses to start a queue.
        assert_eq!(
            mcp_port(4517, Some("http://127.0.0.1:4517")).status,
            CheckStatus::Pass
        );
        let unbound = mcp_port(4517, None);
        assert_eq!(unbound.status, CheckStatus::Warn);
        assert!(unbound.detail.contains("4517"));
        assert_eq!(unbound.check, Check::McpPort);
    }
}
