//! What a repository's credential puts in a run's child environment (task 022,
//! ADR-0020).
//!
//! This is a **third environment rule**, beside the two `runner::process`'s
//! header already states, and it is not the same shape as either. Rule 1 is the
//! operator's *choice* (`RunEnvironment`); rule 2 is unconditional (`CLAUDE_*`
//! is process identity); this one is conditional on the repository — a
//! repository without a credential keeps today's ambient behaviour **byte for
//! byte**, which is what makes adopting the feature safe one repository at a
//! time.
//!
//! # Everything is an environment variable, and nothing is an argument
//!
//! `ps` is world-readable on Unix and the Windows equivalents are no better, so
//! the token never becomes a command-line argument. Nothing is written to disk
//! either: no `.git-credentials`, no `gh` config directory, nothing left inside
//! a worktree the run could stage.
//!
//! # HTTPS auth through git's own environment, not a credential helper
//!
//! `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` is git's
//! documented way to add configuration to one process, and it is identical on
//! all three platforms. A `credential.helper` snippet is `sh -c` by another
//! name and does not exist on Windows at all — ADR-0020 names it as the thing
//! not to reach for, and this module is where that would otherwise happen.
//!
//! The operator's own `GIT_CONFIG_*` variables are **removed first**. Appending
//! to their count is an off-by-one that silently drops either their
//! configuration or ours, and which of the two it drops depends on numbering
//! nobody can see.

use std::collections::BTreeMap;

use base64::Engine;

use super::redact::Redactor;
use super::Secret;

/// The one git config entry a credential adds.
///
/// `github.com` specifically: ADR-0020 claims the *forge* credential and
/// nothing else, and a header on every HTTPS remote would send a GitHub token
/// to whatever else the run cloned.
pub const EXTRAHEADER_KEY: &str = "http.https://github.com/.extraheader";

/// The variables an inherited environment must not carry through when a
/// repository has its own credential.
///
/// The four `gh` reads, in its own precedence order, plus its config directory —
/// ADR-0020 point 5: the operator's ambient login must be *absent*, not merely
/// outranked, or a token Rimaia injected and a token `gh` found on disk become
/// indistinguishable in a transcript.
pub const AMBIENT_FORGE_VARS: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GH_CONFIG_DIR",
];

/// The prefixes of git's own per-process configuration channel.
const GIT_CONFIG_PREFIXES: [&str; 3] = ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"];

/// What one repository's credential contributes to a child.
///
/// Removals and additions kept apart, because they are two different rules with
/// two different reasons — and because a test that asserts "the operator's
/// ambient token is *absent*" needs to see the removal as its own fact rather
/// than infer it from an addition.
#[derive(Debug, Clone, Default)]
pub struct ChildEnvironment {
    /// Names to `env_remove`, in a stable order.
    pub remove: Vec<String>,
    /// Names and values to `env`, sorted so an assertion can compare the whole
    /// map rather than probe it key by key.
    pub set: BTreeMap<String, String>,
    /// Every form the token takes among the values above, for the transcript.
    pub redactor: Redactor,
}

impl ChildEnvironment {
    /// The state a repository with no credential is in: nothing added, nothing
    /// removed, nothing to redact.
    pub fn ambient() -> Self {
        Self::default()
    }

    pub fn is_ambient(&self) -> bool {
        self.remove.is_empty() && self.set.is_empty()
    }
}

/// `Basic base64("x-access-token:" + token)` — the value the `extraheader`
/// carries.
///
/// `x-access-token` is the username GitHub documents for a token used as a
/// password over HTTPS; the token is the password.
pub fn authorization_header(secret: &Secret) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("x-access-token:{}", secret.expose()))
    )
}

/// What this repository's credential does to the child's environment.
///
/// Pure, and takes the parent environment rather than reading it, for exactly
/// the reason [`inherited_identity_vars`](crate::runner::process::inherited_identity_vars)
/// does: the rule is then assertable without spawning anything, which is what
/// makes "an exact environment diff" a test rather than an aspiration.
pub fn child_environment<I, S>(secret: Option<&Secret>, parent: I) -> ChildEnvironment
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let Some(secret) = secret else {
        // Byte for byte what it was before this feature existed. Consuming
        // `parent` is deliberate rather than an oversight — the caller passing
        // an environment it will not be asked about is the honest signature.
        let _ = parent.into_iter().count();
        return ChildEnvironment::ambient();
    };

    let mut remove: Vec<String> = parent
        .into_iter()
        .map(Into::into)
        .filter(|name| is_git_config_var(name))
        .collect();
    remove.sort();
    remove.dedup();
    // The ambient forge variables are removed whether or not the parent has
    // them: `env_remove` on an absent name is a no-op, and listing them
    // unconditionally is what makes the rule readable as "these are gone"
    // rather than as "these are gone if they happened to be there".
    remove.extend(AMBIENT_FORGE_VARS.iter().map(|name| name.to_string()));

    let header = authorization_header(secret);
    let set = BTreeMap::from([
        ("GH_TOKEN".to_string(), secret.expose().to_string()),
        // Exactly one entry, because the operator's own count was removed
        // above. Numbering from zero is git's own convention.
        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        ("GIT_CONFIG_KEY_0".to_string(), EXTRAHEADER_KEY.to_string()),
        ("GIT_CONFIG_VALUE_0".to_string(), header.clone()),
        // A bad credential has to fail immediately rather than block on a
        // prompt no one will answer at 2am. Windows ships Git Credential
        // Manager by default and is where this bites; `GCM_INTERACTIVE` is its
        // own switch and is harmless where GCM is not installed.
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        ("GCM_INTERACTIVE".to_string(), "Never".to_string()),
    ]);

    ChildEnvironment {
        remove,
        set,
        // Both forms, because a run can print either: `env` shows the header,
        // `gh auth token` shows the token.
        redactor: Redactor::for_values([
            secret.expose().to_string(),
            header.clone(),
            // The base64 alone, without the `Basic ` prefix — which is what a
            // `git config --list` in the worktree would print.
            header.trim_start_matches("Basic ").to_string(),
        ]),
    }
}

fn is_git_config_var(name: &str) -> bool {
    GIT_CONFIG_PREFIXES
        .iter()
        .any(|prefix| name.eq_ignore_ascii_case(prefix) || name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn secret() -> Secret {
        Secret::new("ghp_sentinelvalue0123456789").expect("a token")
    }

    #[test]
    fn a_repository_without_a_credential_changes_nothing_at_all() {
        // The acceptance criterion, as a unit: "a repository without a
        // credential produces a child environment byte-identical to today's".
        let environment = child_environment(None, ["GH_TOKEN", "GIT_CONFIG_COUNT", "PATH"]);

        assert!(environment.is_ambient());
        assert!(environment.remove.is_empty());
        assert!(environment.set.is_empty());
        assert!(environment.redactor.is_empty());
    }

    #[test]
    fn a_credential_sets_gh_token_and_gits_own_per_process_configuration() {
        let environment = child_environment(Some(&secret()), Vec::<String>::new());

        assert_eq!(
            environment.set,
            BTreeMap::from([
                ("GCM_INTERACTIVE".to_string(), "Never".to_string()),
                (
                    "GH_TOKEN".to_string(),
                    "ghp_sentinelvalue0123456789".to_string()
                ),
                ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                (
                    "GIT_CONFIG_KEY_0".to_string(),
                    "http.https://github.com/.extraheader".to_string()
                ),
                (
                    "GIT_CONFIG_VALUE_0".to_string(),
                    "Basic eC1hY2Nlc3MtdG9rZW46Z2hwX3NlbnRpbmVsdmFsdWUwMTIzNDU2Nzg5".to_string()
                ),
                ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
            ]),
        );
    }

    #[test]
    fn the_operators_own_ambient_login_is_removed_rather_than_outranked() {
        // ADR-0020 point 5. Outranking would leave a token Rimaia injected and
        // one `gh` found on disk indistinguishable in a transcript.
        let environment = child_environment(Some(&secret()), Vec::<String>::new());

        for name in AMBIENT_FORGE_VARS {
            assert!(
                environment.remove.iter().any(|held| held == name),
                "{name} must be removed even when the parent does not have it",
            );
        }
    }

    #[test]
    fn every_inherited_git_config_variable_is_removed_before_ours_is_added() {
        // Appending to the operator's count is an off-by-one that silently
        // drops one side or the other, and which side depends on numbering
        // nobody can see.
        let environment = child_environment(
            Some(&secret()),
            [
                "GIT_CONFIG_COUNT",
                "GIT_CONFIG_KEY_0",
                "GIT_CONFIG_VALUE_0",
                "GIT_CONFIG_KEY_1",
                "GIT_CONFIG_VALUE_1",
                "PATH",
                "HOME",
            ],
        );

        assert_eq!(
            environment
                .remove
                .iter()
                .filter(|name| name.starts_with("GIT_CONFIG"))
                .collect::<Vec<_>>(),
            vec![
                "GIT_CONFIG_COUNT",
                "GIT_CONFIG_KEY_0",
                "GIT_CONFIG_KEY_1",
                "GIT_CONFIG_VALUE_0",
                "GIT_CONFIG_VALUE_1",
            ],
        );
        assert!(
            !environment.remove.iter().any(|name| name == "PATH"),
            "everything else is exactly what a run is supposed to have",
        );
    }

    #[test]
    fn the_header_is_the_base64_github_documents_for_a_token_over_https() {
        assert_eq!(
            authorization_header(&Secret::new("t").expect("a token")),
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode("x-access-token:t")
            ),
        );
    }

    #[test]
    fn both_forms_of_the_token_are_redactable() {
        let environment = child_environment(Some(&secret()), Vec::<String>::new());

        assert_eq!(
            environment
                .redactor
                .apply("GH_TOKEN=ghp_sentinelvalue0123456789"),
            "GH_TOKEN=[redacted]",
        );
        assert_eq!(
            environment
                .redactor
                .apply(&format!("value={}", environment.set["GIT_CONFIG_VALUE_0"])),
            // The whole header, not just its base64 tail: the longest value
            // wins, which is what the ordering in `Redactor::for_values` is for.
            "value=[redacted]",
        );
        // And the bare base64, which is what `git config --list` inside the
        // worktree would print without the scheme in front of it.
        assert_eq!(
            environment
                .redactor
                .apply("eC1hY2Nlc3MtdG9rZW46Z2hwX3NlbnRpbmVsdmFsdWUwMTIzNDU2Nzg5"),
            "[redacted]",
        );
    }
}
