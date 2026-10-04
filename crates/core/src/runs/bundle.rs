//! The review bundle: what a run's branch carried when the run ended, kept as a
//! record on the board (ADR-0033 point 7, ADR-0036, task 033).
//!
//! These are the board's types. Computing one is git, and lives in
//! [`crate::worktree::bundle`]; storing one is [`crate::runner::outcome`]'s,
//! the only writer of `runs` and of `review_bundles` (ADR-0006); reading one
//! back is [`super::get_run`]'s.
//!
//! # The cap: [`PATCH_CAP_BYTES`], 512 KiB
//!
//! Only the patch is capped. The reasons, which task 033's Notes argue at
//! length:
//!
//! - **It holds a whole task's diff.** A task is sized for one agent session,
//!   about 3–4k lines of diff, which at git's typical 60–80 bytes a line is
//!   roughly 250–300 KB. That fits whole, with about twice that as headroom.
//!   Past that nobody reads a patch line by line, and the forge holds the rest
//!   one link away (ADR-0033 point 7).
//! - **The transport limit.** Task 052's `finish_run` route keeps axum's 2 MB
//!   default body limit (D34). A 512 KiB patch roughly doubles in the worst
//!   realistic JSON escaping, so the patch alone never forces a special limit.
//! - **Storage.** Every board write is replicated (ADR-0037), and every row
//!   whose branch carries commits stores its own bundle, so a night costs at
//!   most runs × 512 KiB of patch.
//!
//! Changing the cap needs no migration. Raising it is bounded by the body limit.
//!
//! **Whole files, never a byte prefix.** git orders a diff by path, so a
//! regenerated `package-lock.json` comes before `src/`, and a prefix cut would
//! spend the whole budget on it. A file that does not fit is skipped and the
//! next one is still tried; its [`PatchInclusion`] says why it is missing. The
//! stored patch is therefore always whole, byte-exact sections of text files,
//! and applies onto `base_sha` as a whole.
//!
//! # The stored JSON's field names are a storage format
//!
//! `review_bundles.files` and `.commits` are JSON arrays produced by these serde
//! types and read only through them (seam-contract D28). Their keys are the
//! wire's camelCase ones (`shortSha`, `committedAt`), deliberately: one casing
//! for disk and wire is one format to keep in step rather than two. **Renaming
//! a field of [`BundleFile`], [`DiffStat`], [`FileDiffStat`](crate::worktree::FileDiffStat) or
//! [`CommitSummary`] is therefore a data migration**, because every row already
//! written still spells the old name.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::worktree::{CommitSummary, DiffStat};

/// The most bytes of patch a bundle stores. See this module's header for why
/// it is 512 KiB.
pub const PATCH_CAP_BYTES: usize = 512 * 1024;

/// What a run's branch carried at its finish: the value a runner produces and
/// D31's `FinishRun::bundle` carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewBundle {
    /// The totals over [`files`](Self::files).
    pub diff: DiffStat,
    /// Every changed file, in git's order, whether or not its section made it
    /// into the patch.
    pub files: Vec<BundleFile>,
    /// Newest first.
    pub commits: Vec<CommitSummary>,
    /// The included sections, concatenated whole. At most [`PATCH_CAP_BYTES`].
    pub patch: String,
    /// The size of the whole diff before the cap, every section counted.
    /// `i64` like every count in [`DiffStat`]: SQLite's `INTEGER` is signed.
    pub patch_bytes: i64,
    /// True iff the cap cut something — some file is
    /// [`TooLarge`](PatchInclusion::TooLarge). A binary or non-UTF-8 file would
    /// be absent at any cap, so it does not set this.
    pub patch_truncated: bool,
}

/// One changed file: [`FileDiffStat`](crate::worktree::FileDiffStat)'s three
/// fields, plus whether its section is in the stored patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleFile {
    pub path: String,
    /// `None` for a binary file, as in
    /// [`FileDiffStat`](crate::worktree::FileDiffStat).
    pub insertions: Option<i64>,
    pub deletions: Option<i64>,
    pub patch: PatchInclusion,
}

/// Whether one file's section is in the stored patch, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchInclusion {
    Included,
    /// It did not fit in what was left of [`PATCH_CAP_BYTES`].
    TooLarge,
    /// Its section is not valid UTF-8 text, and the column is `TEXT`.
    NotUtf8,
    /// git's section for it is one "Binary files … differ" line, which
    /// `git apply` rejects, so it is never included.
    Binary,
}

/// A stored bundle as a read returns it.
///
/// Its own struct rather than a flattened [`ReviewBundle`], because one field
/// changes type: D28 lets `patch` be NULL once it has been pruned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredBundle {
    pub diff: DiffStat,
    pub files: Vec<BundleFile>,
    pub commits: Vec<CommitSummary>,
    /// `None` once pruned (ADR-0036 point 6). Nothing in solo mode prunes it.
    pub patch: Option<String>,
    pub patch_bytes: i64,
    pub patch_truncated: bool,
    pub patch_pruned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// What a run's finish measured about its worktree: the two fields D31's
/// `FinishRun` carries, in the same shape, so task 036 can move them onto the
/// port without reshaping them.
///
/// [`Default`] is "nothing was measured", which is what a crash-closed row and
/// a worktree that could not be read both record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunCapture {
    pub head_sha: Option<String>,
    pub bundle: Option<ReviewBundle>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    /// The keys pinned here are a storage format (see the module header): a
    /// rename that this test does not catch would leave every stored row
    /// unreadable.
    #[test]
    fn a_bundle_file_is_stored_with_the_wires_camel_case_keys() {
        let file = BundleFile {
            path: "src/lib.rs".to_string(),
            insertions: Some(3),
            deletions: None,
            patch: PatchInclusion::NotUtf8,
        };

        assert_eq!(
            serde_json::to_value(&file).expect("serialize"),
            json!({ "path": "src/lib.rs", "insertions": 3, "deletions": null, "patch": "not_utf8" })
        );
    }

    #[test]
    fn every_patch_inclusion_spells_itself_in_snake_case() {
        for (inclusion, spelling) in [
            (PatchInclusion::Included, "included"),
            (PatchInclusion::TooLarge, "too_large"),
            (PatchInclusion::NotUtf8, "not_utf8"),
            (PatchInclusion::Binary, "binary"),
        ] {
            assert_eq!(
                serde_json::to_value(inclusion).expect("serialize"),
                json!(spelling)
            );
            assert_eq!(
                serde_json::from_value::<PatchInclusion>(json!(spelling)).expect("deserialize"),
                inclusion
            );
        }
    }

    #[test]
    fn a_review_bundle_round_trips_through_its_own_json() {
        let bundle = ReviewBundle {
            diff: DiffStat {
                files_changed: 1,
                insertions: 2,
                deletions: 0,
            },
            files: vec![BundleFile {
                path: "a.txt".to_string(),
                insertions: Some(2),
                deletions: Some(0),
                patch: PatchInclusion::Included,
            }],
            commits: vec![CommitSummary {
                sha: "1111111111111111111111111111111111111111".to_string(),
                short_sha: "1111111".to_string(),
                subject: "Add a".to_string(),
                author: "Rimaia Test".to_string(),
                committed_at: "2026-08-20T12:00:00Z".parse().expect("a literal timestamp"),
            }],
            patch: "diff --git a/a.txt b/a.txt\n".to_string(),
            patch_bytes: 26,
            patch_truncated: false,
        };

        let wire = serde_json::to_value(&bundle).expect("serialize");
        assert_eq!(wire["commits"][0]["shortSha"], json!("1111111"));
        assert_eq!(wire["patchBytes"], json!(26));
        assert_eq!(
            serde_json::from_value::<ReviewBundle>(wire).expect("deserialize"),
            bundle
        );
    }
}
