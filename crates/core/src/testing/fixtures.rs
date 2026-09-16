//! Access to the recorded agent-CLI streams.
//!
//! The CLI is faked by replaying real recorded output, not by mocking a trait
//! (ADR-0015), so tasks 008 and 014 can exercise parsing and classification
//! without spawning a process or spending tokens.
//!
//! This module stops at the line level on purpose. It hands out paths and raw
//! JSONL lines and knows nothing about event shapes — the event enum, the parser
//! and the classifier belong to the runner (task 008), and a fixture helper that
//! also parsed would let a parser bug hide inside its own test harness.
//!
//! [`all_fixtures`] globs a directory rather than listing scenarios, which is
//! what makes adding a fixture a change to a fixtures directory and nowhere
//! else. **That property now holds per corpus** (seam-contract D27.6): each
//! provider's recordings live under their own directory, because seven tests
//! iterate the Claude corpus asserting properties every real recording has, and
//! a foreign file dropped in beside them turns each of those into an exclusion
//! list.
//!
//! The unqualified helpers — [`fixtures_dir`], [`fixture_path`],
//! [`fixture_lines`], [`all_fixtures`] — **are** the Claude corpus and are
//! behaviourally unchanged. That is deliberate: the tests that did not have to
//! change are the evidence that ADR-0026's refactor moved no behaviour.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::runner::provider::ProviderId;

/// Relative to the crate root, so the lookup is independent of the working
/// directory a test runner happens to use.
const FIXTURE_DIR: &str = "tests/fixtures";

const FIXTURE_EXTENSION: &str = "jsonl";

pub fn fixtures_dir() -> PathBuf {
    dir_for(ProviderId::ClaudeCode)
}

/// Where one provider's corpus lives. A directory per provider, never a prefix
/// inside one.
pub fn dir_for(provider: ProviderId) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE_DIR)
        .join(corpus(provider))
}

/// The directory name for a provider's corpus. `claude-code`'s is `cli` for the
/// reason nothing else in this file changed: it is the path six test files and a
/// README already name.
fn corpus(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::ClaudeCode => "cli",
        ProviderId::Ledger => "ledger",
    }
}

/// Where the `name` scenario is recorded, e.g. `fixture_path("interrupted-sigterm")`.
/// `name` is the file stem; the extension is implied.
pub fn fixture_path(name: &str) -> PathBuf {
    path_for(ProviderId::ClaudeCode, name)
}

pub fn path_for(provider: ProviderId, name: &str) -> PathBuf {
    dir_for(provider).join(format!("{name}.{FIXTURE_EXTENSION}"))
}

/// One scenario from `provider`'s corpus, one JSON document per item.
pub fn lines_for(provider: ProviderId, name: &str) -> impl Iterator<Item = String> {
    read_lines(&path_for(provider, name))
}

/// Every scenario in `provider`'s corpus, sorted, by stem.
pub fn all_for(provider: ProviderId) -> Vec<String> {
    scenario_names(&dir_for(provider))
}

/// The scenario's stream, one JSON document per item, blank lines skipped.
///
/// Lines are handed back unparsed and unvalidated — a malformed-line fixture is
/// a scenario, not an error.
pub fn fixture_lines(name: &str) -> impl Iterator<Item = String> {
    read_lines(&fixture_path(name))
}

/// Every recorded scenario, sorted, by stem — the names [`fixture_lines`] takes.
///
/// Returns empty rather than panicking when the directory does not exist yet, so
/// this module is usable before the fixtures land. A test that iterates these
/// should assert the list is non-empty first; otherwise it passes vacuously.
pub fn all_fixtures() -> Vec<String> {
    scenario_names(&fixtures_dir())
}

fn scenario_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut names: Vec<String> = entries
        .map(|entry| {
            entry
                .expect("the fixtures directory must stay readable")
                .path()
        })
        .filter(|path| path.extension() == Some(OsStr::new(FIXTURE_EXTENSION)))
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect();

    names.sort();
    names
}

fn read_lines(path: &Path) -> impl Iterator<Item = String> {
    let file = File::open(path)
        .unwrap_or_else(|error| panic!("fixture {} is missing: {error}", path.display()));
    let path = path.to_path_buf();

    BufReader::new(file)
        .lines()
        .map(move |line| {
            line.unwrap_or_else(|error| {
                panic!("fixture {} is not readable text: {error}", path.display())
            })
        })
        .filter(|line| !line.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_paths_resolve_against_the_crate_not_the_working_directory() {
        let path = fixture_path("success");

        assert!(path.is_absolute());
        assert!(
            path.ends_with("tests/fixtures/cli/success.jsonl"),
            "unexpected fixture layout: {}",
            path.display()
        );
    }

    #[test]
    fn each_provider_reads_its_own_corpus_and_only_its_own() {
        // Seam-contract D27.6. The two directories are siblings, so "adding a
        // fixture changes only the fixtures directory" holds per corpus rather
        // than being traded away for one shared list with carve-outs in it.
        let claude = all_for(ProviderId::ClaudeCode);
        let ledger = all_for(ProviderId::Ledger);

        assert!(!claude.is_empty());
        assert!(!ledger.is_empty());
        assert!(
            claude.iter().all(|name| !ledger.contains(name)),
            "the two corpora share a scenario name, so one of them is being read twice",
        );
        assert_eq!(fixtures_dir(), dir_for(ProviderId::ClaudeCode));
        assert!(path_for(ProviderId::Ledger, "finished").is_file());
    }

    #[test]
    fn blank_lines_are_skipped_but_whitespace_inside_a_line_is_kept() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("scenario.jsonl");
        std::fs::write(
            &path,
            "{\"type\": \"system\"}\n\n   \n{\"type\": \"result\"}\n\n",
        )
        .expect("write a stand-in fixture");

        assert_eq!(
            read_lines(&path).collect::<Vec<_>>(),
            vec!["{\"type\": \"system\"}", "{\"type\": \"result\"}"]
        );
    }

    #[test]
    fn every_discovered_name_reads_back_through_fixture_lines() {
        // The guarantee downstream leans on: whatever `all_fixtures` reports is
        // something `fixture_lines` can be handed unchanged.
        let names = all_fixtures();
        assert!(
            !names.is_empty(),
            "no recorded scenarios under {}",
            fixtures_dir().display()
        );

        for name in names {
            assert!(
                fixture_path(&name).is_file(),
                "{name} was discovered but does not resolve"
            );
            assert!(
                fixture_lines(&name).count() > 0,
                "{name} is empty; a scenario needs at least one event"
            );
        }
    }

    #[test]
    fn discovery_globs_jsonl_and_ignores_everything_else() {
        // The property that makes "adding a fixture changes only the fixtures
        // directory" true, asserted against a directory this test controls.
        let dir = tempfile::tempdir().expect("temp dir");
        for file in ["resume-success.jsonl", "max-turns.jsonl", "README.md"] {
            std::fs::write(dir.path().join(file), "{}\n").expect("write a stand-in fixture");
        }
        std::fs::create_dir(dir.path().join("nested")).expect("a directory to ignore");

        assert_eq!(scenario_names(dir.path()), ["max-turns", "resume-success"]);
    }

    #[test]
    fn a_missing_fixtures_directory_is_empty_rather_than_fatal() {
        // Downstream tasks call `all_fixtures` before their fixtures exist; that
        // must not be the thing that fails their build.
        let dir = tempfile::tempdir().expect("temp dir");

        assert!(scenario_names(&dir.path().join("never-created")).is_empty());
    }
}
