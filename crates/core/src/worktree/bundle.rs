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
use crate::runs::bundle::{
    encoded_len, BundleFile, PatchInclusion, ReviewBundle, RunCapture, BUNDLE_CAP_BYTES,
    PATCH_CAP_BYTES,
};
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
///
/// The bundle is held to [`BUNDLE_CAP_BYTES`] last, by [`bound`]: past it the
/// patch is dropped, and past it again the bundle is, `head_sha` kept either
/// way (task 052).
pub async fn capture(worktree_path: &Path, base_sha: Option<&str>) -> RunCapture {
    capture_with_cap(worktree_path, base_sha, BUNDLE_CAP_BYTES).await
}

/// [`capture`], bounded by `cap` rather than [`BUNDLE_CAP_BYTES`], so a test
/// can reach the cap with a repository of a few files.
async fn capture_with_cap(worktree_path: &Path, base_sha: Option<&str>, cap: usize) -> RunCapture {
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
        bundle: bundle.and_then(|bundle| bound(bundle, cap)),
    }
}

/// `bundle`, held to `cap` bytes of [`encoded_len`], in two steps (task 052):
///
/// 1. Over the cap, the patch goes the way [`PATCH_CAP_BYTES`]' budget drops a
///    section: every `Included` file becomes `TooLarge`, `patch` is empty and
///    `patch_truncated` is true. `files`, `commits` and `diff` are kept. That
///    is a state task 033 already stores and renders: the cap cut something.
/// 2. Still over the cap, the lists alone are too large, and this is `None`:
///    task 033's own path for a bundle that could not be built, which
///    `get_run` reads as `NotRecorded`, the reading that is never false.
///
/// Neither step adds a column to say why (D28 has none, and D4 reserves no
/// migration); each logs the counts, so a missing patch or bundle has its
/// reason in the runner's log. Pure, so its cases build values rather than
/// repositories of twenty thousand files.
pub fn bound(bundle: ReviewBundle, cap: usize) -> Option<ReviewBundle> {
    let size = encoded_len(&bundle);
    if size <= cap {
        return Some(bundle);
    }
    tracing::warn!(
        files = bundle.files.len(),
        commits = bundle.commits.len(),
        encoded_bytes = size,
        cap,
        "the review bundle is over its cap; recording it without its patch",
    );

    let mut bundle = bundle;
    for file in &mut bundle.files {
        if file.patch == PatchInclusion::Included {
            file.patch = PatchInclusion::TooLarge;
        }
    }
    bundle.patch = String::new();
    bundle.patch_truncated = true;

    let size = encoded_len(&bundle);
    if size <= cap {
        return Some(bundle);
    }
    tracing::warn!(
        files = bundle.files.len(),
        commits = bundle.commits.len(),
        encoded_bytes = size,
        cap,
        "the review bundle's lists alone are over its cap; recording the head commit without a \
         bundle",
    );
    None
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

    fn file(path: &str, patch: PatchInclusion) -> BundleFile {
        BundleFile {
            path: path.to_string(),
            insertions: Some(1),
            deletions: Some(0),
            patch,
        }
    }

    fn commit(index: usize) -> crate::worktree::CommitSummary {
        crate::worktree::CommitSummary {
            sha: format!("{index:040x}"),
            short_sha: format!("{index:07x}"),
            subject: format!("Commit {index}"),
            author: "Rimaia Test".to_string(),
            committed_at: "2026-08-20T12:00:00Z".parse().expect("a literal timestamp"),
        }
    }

    /// Two included files, one binary, one commit, and a patch of
    /// `patch_bytes` bytes.
    fn a_bundle(patch_bytes: usize) -> ReviewBundle {
        ReviewBundle {
            diff: crate::worktree::DiffStat {
                files_changed: 3,
                insertions: 3,
                deletions: 0,
            },
            files: vec![
                file("src/a.rs", PatchInclusion::Included),
                file("logo.png", PatchInclusion::Binary),
                file("src/b.rs", PatchInclusion::Included),
            ],
            commits: vec![commit(1)],
            patch: "+".repeat(patch_bytes),
            patch_bytes: patch_bytes as i64,
            patch_truncated: false,
        }
    }

    #[test]
    fn a_bundle_under_the_cap_is_left_exactly_as_built() {
        let built = a_bundle(1_000);

        assert_eq!(bound(built.clone(), encoded_len(&built)), Some(built));
    }

    #[test]
    fn a_bundle_over_the_cap_drops_its_patch_and_keeps_its_lists() {
        let built = a_bundle(10_000);
        let cap = encoded_len(&built) - 1;

        let bounded = bound(built.clone(), cap).expect("the lists alone fit");

        assert_eq!(
            bounded,
            ReviewBundle {
                files: vec![
                    file("src/a.rs", PatchInclusion::TooLarge),
                    file("logo.png", PatchInclusion::Binary),
                    file("src/b.rs", PatchInclusion::TooLarge),
                ],
                patch: String::new(),
                patch_truncated: true,
                ..built.clone()
            }
        );
        assert_eq!(bounded.diff, built.diff);
        assert_eq!(bounded.commits, built.commits);
        assert!(encoded_len(&bounded) <= cap);
    }

    #[test]
    fn a_bundle_whose_lists_alone_exceed_the_cap_is_dropped() {
        let files = (0..20_000)
            .map(|index| {
                file(
                    &format!("generated/module_{index:05}/schema_definitions.json"),
                    PatchInclusion::TooLarge,
                )
            })
            .collect::<Vec<_>>();
        let built = ReviewBundle {
            files,
            commits: (0..100).map(commit).collect(),
            ..a_bundle(0)
        };
        assert!(encoded_len(&built) > BUNDLE_CAP_BYTES);

        assert_eq!(bound(built, BUNDLE_CAP_BYTES), None);
    }

    #[tokio::test]
    async fn an_oversized_bundle_is_dropped_and_head_sha_is_kept() {
        use crate::db::{BoardColumn, ExitClass, RunKind, RunState, RunStatus};
        use crate::runner::outcome::{finish_run, start_run, NewRun, RunOutcome};
        use crate::runs::{self, RunReview};
        use crate::testing::{TempRepo, TestContext};

        let source = TempRepo::init();
        let base_sha = source.head_sha();
        let source = source.commit("a.txt", &"a\n".repeat(200), "Add a").commit(
            "b.txt",
            &"b\n".repeat(200),
            "Add b",
        );
        let head_sha = source.head_sha();

        // A few hundred bytes: the two commits' list is over it on its own.
        let capture = capture_with_cap(source.path(), Some(&base_sha), 300).await;

        assert_eq!(capture.head_sha.as_deref(), Some(head_sha.as_str()));
        assert_eq!(capture.bundle, None);

        // Recorded as a finish would record it, and read back.
        let h = TestContext::new().await;
        let data = tempfile::TempDir::new().expect("a data directory");
        let paths = crate::AppPaths::new(data.path());
        let repository = crate::repo::register(
            &h.context,
            h.machine(),
            &paths.worktrees_dir(),
            crate::repo::NewRepository {
                path: source.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register the repository");
        let task = crate::tasks::create_task(
            &h.context,
            crate::tasks::NewTask {
                repository_id: repository.id,
                title: "Regenerate everything".to_string(),
                plan: Some("1. Regenerate".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a task");
        for state in [RunState::Queued, RunState::Running] {
            crate::tasks::set_run_state(&h.context, &task.id, state)
                .await
                .expect("walk the task into running");
        }
        let run = start_run(
            &h.context,
            &paths,
            NewRun {
                task_id: task.id.clone(),
                kind: RunKind::Implementation,
                session_id: "session".to_string(),
                prompt: "regenerate".to_string(),
                base_ref: Some("main".to_string()),
                base_sha: Some(base_sha.clone()),
            },
        )
        .await
        .expect("open the run");
        let outcome = RunOutcome {
            exit_class: ExitClass::Success,
            status: RunStatus::Succeeded,
            error_message: None,
            num_turns: Some(1),
            cost_usd: None,
            duration_ms: None,
            pr_url: None,
            usage_limit_resets_at: None,
            resume_after: None,
            spawned_as: Default::default(),
            usage: Default::default(),
        };
        finish_run(&h.context, &run.id, &outcome, &capture)
            .await
            .expect("close the run");

        let detail = runs::get_run(&h.context, &paths, &run.id)
            .await
            .expect("get_run");
        assert_eq!(detail.run.head_sha, Some(head_sha));
        assert_eq!(detail.review, RunReview::NotRecorded);
    }

    #[test]
    fn bytes_before_the_first_header_are_refused() {
        let mut assembly = PatchAssembly::new(vec![false]);
        assembly.push_line(b"warning: something\n");
        assembly.push_line(b"diff --git a/x b/x\n");

        assert!(assembly.finish().is_err());
    }
}
