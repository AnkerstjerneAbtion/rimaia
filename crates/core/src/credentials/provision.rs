//! Saving a token, and the verification that happens first (task 022,
//! ADR-0020).
//!
//! # Verified before stored, and three outcomes rather than two
//!
//! `gh api user` and `gh api repos/{owner}/{repo}`, with **only that token** in
//! the child's environment, and the login it resolves to is what gets stored.
//! The three outcomes are genuinely different and each says a different thing:
//!
//! - **Verified.** The forge answered, and the login is recorded so the pane can
//!   say whose token it is without ever showing the token.
//! - **Rejected by the forge.** ADR-0020's "refused at paste time" — the save
//!   does not happen, because a token the forge will not accept is a run that
//!   fails at 2am with a push it cannot make.
//! - **Could not verify.** `gh` is not installed. The save *happens*, marked
//!   unverified: a missing local tool says nothing about the token, and
//!   refusing here would make the feature unusable on a machine that has git
//!   but not `gh` — which is a machine that can still clone and push.
//!
//! # `gh`, not an HTTP client
//!
//! It needs no new dependency, it is the same binary and the same auth
//! precedence the run will use, and task 018's doctor already lists it as a
//! per-repository prerequisite. Argument vectors, never `sh -c` — Windows has
//! no `sh`.

use std::path::Path;

use serde::Serialize;
use tokio::process::Command;

use crate::credentials::inject::AMBIENT_FORGE_VARS;
use crate::credentials::Secret;
use crate::repo::GH_CLI;
use crate::runner::process::strip_process_identity;

/// What asking the forge about a token came back with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Verification {
    /// The forge answered, and this is who the token is.
    Verified {
        login: String,
        /// Whether that login can also reach the repository the credential is
        /// for. `None` when the repository has no GitHub remote to check
        /// against — an SSH remote, or none at all.
        repository_access: Option<bool>,
    },
    /// The forge said no. The save is refused.
    Rejected { reason: String },
    /// `gh` is not installed, so nothing local could ask. The save is allowed
    /// and marked.
    Unverifiable { reason: String },
}

impl Verification {
    /// The login to store, or `None` for a save that could not be verified.
    pub fn login(&self) -> Option<&str> {
        match self {
            Self::Verified { login, .. } => Some(login),
            _ => None,
        }
    }
}

/// Asks the forge who this token is, and whether it can reach `owner/repo`.
///
/// `program` is injected for the reason every other external binary in this
/// crate is: the suite points it at a script and asserts all three outcomes
/// without a network, a token or an installed `gh`.
pub async fn verify(program: &Path, secret: &Secret, owner_repo: Option<&str>) -> Verification {
    let user = match run_gh(program, secret, &["api", "user"]).await {
        Ok(output) => output,
        Err(reason) => return Verification::Unverifiable { reason },
    };

    if !user.status.success() {
        return Verification::Rejected {
            reason: forge_message(&user.stderr, &user.stdout),
        };
    }

    let Some(login) = login_from(&String::from_utf8_lossy(&user.stdout)) else {
        return Verification::Rejected {
            reason: "the forge answered without naming a login, so this token could not be \
                     identified"
                .to_string(),
        };
    };

    // Asked second and reported separately: a token that is valid but cannot
    // see *this* repository is a real, savable state — the user may be about to
    // grant it — where a token the forge rejects outright is not.
    let repository_access = match owner_repo {
        Some(owner_repo) => {
            match run_gh(program, secret, &["api", &format!("repos/{owner_repo}")]).await {
                Ok(output) => Some(output.status.success()),
                // The first call worked, so `gh` is there; a spawn failure on
                // the second is a fluke worth reporting as "unknown" rather
                // than as "no access".
                Err(_) => None,
            }
        }
        None => None,
    };

    Verification::Verified {
        login,
        repository_access,
    }
}

/// `gh`, with **only this token** in its environment.
///
/// Every ambient forge variable is removed before the token is added, so the
/// answer is about the token the user just pasted and not about whatever `gh`
/// found on disk — which is the whole reason to verify at all. `GH_CONFIG_DIR`
/// too: `gh` reads a stored login from there in preference to nothing.
async fn run_gh(
    program: &Path,
    secret: &Secret,
    args: &[&str],
) -> std::result::Result<std::process::Output, String> {
    let mut command = Command::new(program);
    command.args(args);
    strip_process_identity(&mut command);
    for name in AMBIENT_FORGE_VARS {
        command.env_remove(name);
    }
    command.env("GH_TOKEN", secret.expose());
    // `gh` prompts for nothing when it has a token, but a `git` it shells out
    // to might; both are belt and braces on a path with no user watching.
    command.env("GIT_TERMINAL_PROMPT", "0");

    command.output().await.map_err(|error| {
        format!(
            "`{}` could not be run, so this token was saved without being checked against the \
             forge: {error}",
            program.display(),
        )
    })
}

/// The first `"login": "..."` in `gh api user`'s answer.
///
/// Hand-parsed off the JSON rather than deserialized into a struct, because the
/// only field that matters is one string and a struct would be a schema for
/// GitHub's user object that this code would then own.
fn login_from(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("login")?
        .as_str()
        .map(|login| login.to_string())
        .filter(|login| !login.is_empty())
}

/// What the forge said, trimmed to one line a pane can render.
fn forge_message(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let said = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("the forge rejected it without saying why");
    format!("the forge rejected this token: {said}")
}

/// `owner/repo` from a remote URL, or `None` when it is not a GitHub HTTPS or
/// SSH remote.
///
/// Both spellings, because the credential covers `gh` API calls whatever the
/// remote is — a repository with an SSH `origin` still opens pull requests
/// through the API, and its settings pane still wants to say whether the token
/// can see it.
pub fn owner_repo_from_remote(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.trim_matches('/').split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

/// Whether `gh` is the name this machine resolves on `PATH`.
pub fn default_gh() -> &'static Path {
    Path::new(GH_CLI)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_login_is_read_out_of_the_forges_own_answer() {
        assert_eq!(
            login_from(r#"{"login":"ea","id":1}"#),
            Some("ea".to_string())
        );
        assert_eq!(login_from(r#"{"id":1}"#), None);
        assert_eq!(login_from("not json"), None);
        assert_eq!(login_from(r#"{"login":""}"#), None);
    }

    #[test]
    fn owner_and_repo_are_read_from_either_spelling_of_a_github_remote() {
        for url in [
            "https://github.com/AnkerstjerneAbtion/rimaia.git",
            "https://github.com/AnkerstjerneAbtion/rimaia",
            "git@github.com:AnkerstjerneAbtion/rimaia.git",
            "ssh://git@github.com/AnkerstjerneAbtion/rimaia.git",
        ] {
            assert_eq!(
                owner_repo_from_remote(url),
                Some("AnkerstjerneAbtion/rimaia".to_string()),
                "{url}",
            );
        }

        // Not GitHub, or not a repository: nothing to ask about, which is a
        // `None` rather than a guess.
        assert_eq!(owner_repo_from_remote("https://gitlab.com/a/b.git"), None);
        assert_eq!(owner_repo_from_remote("/srv/git/bare.git"), None);
        assert_eq!(
            owner_repo_from_remote("https://github.com/only-owner"),
            None
        );
    }

    #[test]
    fn a_rejection_carries_what_the_forge_said_rather_than_a_generic_sentence() {
        assert_eq!(
            forge_message(b"gh: Bad credentials (HTTP 401)\n", b""),
            "the forge rejected this token: gh: Bad credentials (HTTP 401)",
        );
        assert!(forge_message(b"", b"").contains("without saying why"));
    }
}
