//! Measuring what a run's branch carries, at the run's finish (task 033).
//!
//! The git half of the review bundle. The types are the board's and live in
//! [`crate::runs::bundle`]; this module only produces them, from a worktree, so
//! that task 036 can move it to the runner side of the board port unchanged.
//!
//! # Two functions, so a bundle that cannot be built never takes `head_sha`
//!
//! [`build`] does the git work and returns its failures, so a test can see
//! them. [`capture`] never fails: it records `head_sha` whenever `HEAD`
//! resolves, and a bundle only when [`build`] produced one. Task 044 chains a
//! dependent off `head_sha` and D29 point 5 reads it for every kind of run, so
//! losing it because a diff could not be parsed would cost more than the bundle
//! is worth.
//!
//! # Anchored on two shas, never on branch names
//!
//! Every range below is `base_sha..head_sha` or `base_sha...head_sha`. A branch
//! name answers about the branch as it is when someone asks; the two recorded
//! commits answer about this run, which is the point of recording anything.

use std::path::Path;

use super::git;
use crate::error::{Error, Result};
use crate::runs::bundle::{BundleFile, PatchInclusion, ReviewBundle, RunCapture, PATCH_CAP_BYTES};
use crate::worktree::FileDiffStat;

/// What a section header looks like. Inside a patch a line starting with this
/// can only be a header: every content line starts with a space, `+`, `-`, `\`
/// or `@@`.
const SECTION_HEADER: &[u8] = b"diff --git ";

/// The review bundle for `base_sha..head_sha` in `worktree_path`, or `None`
/// when that range has no commits.
///
/// Three git calls, in an order that stops as early as it can: the commits
/// (none means no bundle), the numstat, then the patch, streamed. The numstat
/// and the patch run with the same [`git::DIFF_OPTIONS`] over the same range,
/// so they walk one diff queue in one order, and `files` pairs them by
/// position. If the two counts disagree this returns an error rather than a
/// mispaired list.
pub async fn build(
    worktree_path: &Path,
    base_sha: &str,
    head_sha: &str,
) -> Result<Option<ReviewBundle>> {
    let commits = git::commits(worktree_path, base_sha, head_sha).await?;
    if commits.is_empty() {
        return Ok(None);
    }

    let range = format!("{base_sha}...{head_sha}");
    let numstat = git::checked(worktree_path, &git::diff_args("--numstat", &range)).await?;
    let (diff, stats) = git::parse_numstat(&numstat);

    let mut patch = PatchAssembly::new(binary_sections(&stats));
    git::stream_lines(worktree_path, &git::diff_args("--patch", &range), |line| {
        patch.push_line(line)
    })
    .await?;
    let patch = patch.finish()?;

    if patch.inclusions.len() != stats.len() {
        return Err(Error::internal(format!(
            "git listed {} changed files for {range} but its patch has {} sections; \
             refusing to pair them",
            stats.len(),
            patch.inclusions.len(),
        )));
    }

    let files = stats
        .into_iter()
        .zip(patch.inclusions)
        .map(|(stat, inclusion)| BundleFile {
            path: stat.path,
            insertions: stat.insertions,
            deletions: stat.deletions,
            patch: inclusion,
        })
        .collect();

    Ok(Some(ReviewBundle {
        diff,
        files,
        commits,
        patch: patch.text,
        patch_bytes: patch.total_bytes,
        patch_truncated: patch.truncated,
    }))
}

/// What a finishing run records about `worktree_path`. Never fails.
///
/// - `head_sha` is `git rev-parse HEAD`, recorded whenever it resolves —
///   including when nothing was committed, because a review commits nothing
///   and its `head_sha` is still "the commit it cleared" (D29 point 5). When
///   it does not resolve (the worktree was deleted underneath the run), this
///   logs a warning and returns [`RunCapture::default`].
/// - A bundle is produced iff `base_sha` is known and `base_sha..head_sha`
///   has at least one commit: the *branch* ends with commits, whichever attempt
///   authored them. When [`build`] fails, the failure is logged and
///   `head_sha` is kept.
pub async fn capture(worktree_path: &Path, base_sha: Option<&str>) -> RunCapture {
    let head_sha = match git::head_sha(worktree_path).await {
        Ok(sha) => sha,
        Err(error) => {
            tracing::warn!(
                worktree = %worktree_path.display(),
                %error,
                "could not read the worktree's HEAD at the finish; this run records no head \
                 commit and no review bundle",
            );
            return RunCapture::default();
        }
    };

    let Some(base_sha) = base_sha else {
        return RunCapture {
            head_sha: Some(head_sha),
            bundle: None,
        };
    };

    let bundle = match build(worktree_path, base_sha, &head_sha).await {
        Ok(bundle) => bundle,
        Err(error) => {
            tracing::warn!(
                worktree = %worktree_path.display(),
                %base_sha,
                %head_sha,
                %error,
                "could not build the review bundle; recording the head commit without one",
            );
            None
        }
    };

    RunCapture {
        head_sha: Some(head_sha),
        bundle,
    }
}

/// Which sections, by position, are binary: their numstat row is `-` `-`.
fn binary_sections(stats: &[FileDiffStat]) -> Vec<bool> {
    stats
        .iter()
        .map(|stat| stat.insertions.is_none() && stat.deletions.is_none())
        .collect()
}

/// The capped patch, as [`PatchAssembly::finish`] hands it back.
struct AssembledPatch {
    text: String,
    inclusions: Vec<PatchInclusion>,
    total_bytes: i64,
    truncated: bool,
}

/// Splits a streamed patch into per-file sections and applies the cap, one
/// line at a time.
///
/// Each section gets exactly one [`PatchInclusion`], decided in this order:
///
/// 1. `Binary` when its numstat row says so. It is never buffered.
/// 2. `TooLarge` as soon as it outgrows what is left of the budget. Its bytes
///    are counted from then on and no longer kept, which is what makes a
///    500 MB vendored diff cost the cap plus a read buffer.
/// 3. `NotUtf8` when it was buffered whole and is not valid UTF-8.
/// 4. `Included` otherwise, appended whole, in git's order.
struct PatchAssembly {
    /// By section position, from the numstat. A section past the end of this
    /// list is still split and counted, so the count mismatch surfaces in
    /// [`build`] rather than here.
    binary: Vec<bool>,
    text: Vec<u8>,
    inclusions: Vec<PatchInclusion>,
    total_bytes: i64,
    current: Option<Section>,
    /// Bytes seen before the first section header. git never writes any with
    /// [`git::DIFF_OPTIONS`]; if it ever does, the patch is not one this module
    /// understands, and [`finish`](Self::finish) says so.
    preamble_bytes: usize,
}

struct Section {
    binary: bool,
    /// `None` once the section has outgrown the budget, or for a binary one.
    buffer: Option<Vec<u8>>,
    too_large: bool,
}

impl PatchAssembly {
    fn new(binary: Vec<bool>) -> Self {
        Self {
            binary,
            text: Vec::new(),
            inclusions: Vec::new(),
            total_bytes: 0,
            current: None,
            preamble_bytes: 0,
        }
    }

    fn remaining(&self) -> usize {
        PATCH_CAP_BYTES.saturating_sub(self.text.len())
    }

    fn push_line(&mut self, line: &[u8]) {
        self.total_bytes += line.len() as i64;

        if line.starts_with(SECTION_HEADER) {
            self.close_section();
            let binary = self
                .binary
                .get(self.inclusions.len())
                .copied()
                .unwrap_or(false);
            self.current = Some(Section {
                binary,
                buffer: (!binary).then(Vec::new),
                too_large: false,
            });
        }

        let remaining = self.remaining();
        let Some(section) = self.current.as_mut() else {
            self.preamble_bytes += line.len();
            return;
        };
        if let Some(buffer) = section.buffer.as_mut() {
            if buffer.len() + line.len() > remaining {
                section.buffer = None;
                section.too_large = true;
            } else {
                buffer.extend_from_slice(line);
            }
        }
    }

    fn close_section(&mut self) {
        let Some(section) = self.current.take() else {
            return;
        };
        let inclusion = if section.binary {
            PatchInclusion::Binary
        } else if section.too_large {
            PatchInclusion::TooLarge
        } else {
            let buffer = section.buffer.unwrap_or_default();
            if std::str::from_utf8(&buffer).is_ok() {
                self.text.extend_from_slice(&buffer);
                PatchInclusion::Included
            } else {
                PatchInclusion::NotUtf8
            }
        };
        self.inclusions.push(inclusion);
    }

    fn finish(mut self) -> Result<AssembledPatch> {
        self.close_section();
        if self.preamble_bytes > 0 {
            return Err(Error::internal(format!(
                "git's patch began with {} bytes before its first `diff --git` header",
                self.preamble_bytes,
            )));
        }
        let truncated = self.inclusions.contains(&PatchInclusion::TooLarge);
        // Every included section was checked as UTF-8 on its own, and a
        // concatenation of valid UTF-8 is valid UTF-8; this cannot fail, but a
        // lossy conversion would hide it if it ever did.
        let text = String::from_utf8(self.text).map_err(|error| {
            Error::internal(format!("the assembled patch is not UTF-8: {error}"))
        })?;
        Ok(AssembledPatch {
            text,
            inclusions: self.inclusions,
            total_bytes: self.total_bytes,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn assemble(binary: Vec<bool>, patch: &[u8]) -> AssembledPatch {
        let mut assembly = PatchAssembly::new(binary);
        for line in patch.split_inclusive(|byte| *byte == b'\n') {
            assembly.push_line(line);
        }
        assembly.finish().expect("a well-formed patch")
    }

    #[test]
    fn a_header_lookalike_inside_a_hunk_does_not_start_a_section() {
        // The content line is `+diff --git …`: prefixed, so it is not a header.
        let patch = b"diff --git a/x b/x\n@@ -0,0 +1 @@\n+diff --git a/y b/y\n";
        let assembled = assemble(vec![false], patch);

        assert_eq!(assembled.inclusions, vec![PatchInclusion::Included]);
        assert_eq!(assembled.text.as_bytes(), patch);
    }

    #[test]
    fn a_binary_section_is_never_buffered_and_never_sets_truncated() {
        let patch =
            b"diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n";
        let assembled = assemble(vec![true], patch);

        assert_eq!(assembled.inclusions, vec![PatchInclusion::Binary]);
        assert_eq!(assembled.text, "");
        assert!(!assembled.truncated);
        assert_eq!(assembled.total_bytes, patch.len() as i64);
    }

    #[test]
    fn bytes_before_the_first_header_are_refused() {
        let mut assembly = PatchAssembly::new(vec![false]);
        assembly.push_line(b"warning: something\n");
        assembly.push_line(b"diff --git a/x b/x\n");

        assert!(assembly.finish().is_err());
    }
}
