//! Keeping a token out of everything a run writes down (task 022, ADR-0020).
//!
//! A `bypassPermissions` run can read its own environment, and echoing it while
//! debugging a push failure is a realistic thing for an agent to do — not a
//! hypothetical. So the token is scrubbed from ADR-0013's JSONL transcript and
//! from the D14 live tail **before write**, and from `tracing` output.
//!
//! # Before write, not on read
//!
//! The transcript is a file on disk that outlives the process and that task 015
//! offers to open in an editor. Redacting on read would leave the secret in the
//! file, which is the only copy that matters.
//!
//! # What this is not
//!
//! Not a secret scanner. It replaces exactly the values it was given, and it
//! makes no attempt to recognise a token by shape — a regex over `ghp_[A-Za-z]+`
//! would miss a fine-grained token, miss an enterprise one, and would sooner or
//! later redact a legitimate string that happened to match. The set of things
//! worth hiding is known exactly: it is what Rimaia itself put in the child's
//! environment.

/// What a redacted value is replaced with.
///
/// A fixed marker rather than a same-length mask: an equal-length run of
/// asterisks leaks the token's length, and a transcript reader is better served
/// by a word that says what happened.
pub const REDACTED: &str = "[redacted]";

/// The values one run's output must not contain.
///
/// Built at spawn from what was actually injected, so it is empty for a
/// repository with no credential — which is the common case and costs a
/// `is_empty` check per line.
#[derive(Clone, Default)]
pub struct Redactor {
    /// Longest first, so a value that contains another is replaced whole rather
    /// than leaving its tail behind. `Basic <base64>` contains the base64, and
    /// the base64 does not contain the raw token — but a future third value
    /// might, and the ordering makes that safe without anyone having to notice.
    values: Vec<String>,
}

impl Redactor {
    /// Nothing to hide. The state a repository without a credential is in.
    pub fn none() -> Self {
        Self::default()
    }

    /// Every form the token takes on the way into the child: the token itself,
    /// and the base64 the `extraheader` carries it in.
    ///
    /// Both, because they are different strings and a run can print either —
    /// `env` shows the header value, `gh auth status` shows the token.
    pub fn for_values(values: impl IntoIterator<Item = String>) -> Self {
        let mut values: Vec<String> = values
            .into_iter()
            // A short value would match everywhere and redact the transcript
            // into uselessness. Nothing legitimate is this short; a token is
            // ~40 bytes at the smallest.
            .filter(|value| value.len() >= 8)
            .collect();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        Self { values }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// `line` with every known value replaced.
    ///
    /// Borrowed back unchanged when there is nothing to do, so the common case —
    /// a repository with no credential, every line of every transcript — costs
    /// no allocation.
    pub fn apply<'a>(&self, line: &'a str) -> std::borrow::Cow<'a, str> {
        if self.values.is_empty() {
            return std::borrow::Cow::Borrowed(line);
        }

        let mut redacted = std::borrow::Cow::Borrowed(line);
        for value in &self.values {
            if redacted.contains(value.as_str()) {
                redacted = std::borrow::Cow::Owned(redacted.replace(value.as_str(), REDACTED));
            }
        }
        redacted
    }
}

/// Hand-written so a `?` on anything holding one cannot print the set.
impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("values", &self.values.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn nothing_to_redact_costs_no_allocation() {
        let redactor = Redactor::none();

        assert!(redactor.is_empty());
        assert!(matches!(
            redactor.apply("a line about nothing"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn every_form_the_token_takes_is_replaced() {
        // A run that echoes its own environment prints both: `GH_TOKEN` carries
        // the raw value and the extraheader carries the base64.
        let redactor = Redactor::for_values([
            "ghp_sentinelvalue0123456789".to_string(),
            "eC1hY2Nlc3MtdG9rZW46Z2hwX3NlbnRpbmVs".to_string(),
        ]);

        assert_eq!(
            redactor.apply("GH_TOKEN=ghp_sentinelvalue0123456789"),
            "GH_TOKEN=[redacted]"
        );
        assert_eq!(
            redactor.apply("Authorization: Basic eC1hY2Nlc3MtdG9rZW46Z2hwX3NlbnRpbmVs"),
            "Authorization: Basic [redacted]"
        );
    }

    #[test]
    fn a_value_appearing_twice_on_one_line_is_replaced_twice() {
        let redactor = Redactor::for_values(["ghp_sentinel_value".to_string()]);

        assert_eq!(
            redactor.apply("ghp_sentinel_value and again ghp_sentinel_value"),
            "[redacted] and again [redacted]"
        );
    }

    #[test]
    fn a_longer_value_containing_a_shorter_one_is_replaced_whole() {
        // Ordering, asserted rather than assumed: replacing the short one first
        // would leave the long one's tail in the transcript.
        let redactor =
            Redactor::for_values(["ghp_secret".to_string(), "ghp_secret_and_more".to_string()]);

        assert_eq!(redactor.apply("x ghp_secret_and_more y"), "x [redacted] y");
    }

    #[test]
    fn a_value_too_short_to_be_a_token_is_not_used_at_all() {
        // Otherwise it would match everywhere and redact the transcript into
        // uselessness. Nothing legitimate is this short.
        let redactor = Redactor::for_values(["abc".to_string()]);

        assert!(redactor.is_empty());
        assert_eq!(redactor.apply("abcdef"), "abcdef");
    }

    #[test]
    fn the_debug_output_names_a_count_rather_than_the_values() {
        let redactor = Redactor::for_values(["ghp_sentinelvalue".to_string()]);

        let printed = format!("{redactor:?}");
        assert!(!printed.contains("ghp_"), "{printed}");
        assert_eq!(printed, "Redactor { values: 1 }");
    }
}
