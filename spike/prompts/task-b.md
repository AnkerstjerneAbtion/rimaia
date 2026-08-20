# Base instructions

Commit as you work, with focused commits and clear messages.
Run the project's tests before you finish.
Do not push and do not open a pull request — this is a local-only spike.

# Task

Repository: spike-testrepo
Branch: rimaia/spike-b

# Plan

Work through these steps in order, committing after each one:

1. Add `slug_words(input: &str) -> Vec<String>` to `src/lib.rs`, returning the
   slugified input split on hyphens with empty segments removed. Add tests. Commit.
2. Add `is_valid_slug(input: &str) -> bool` returning true when the input is already
   equal to its own slugified form and is non-empty. Add tests. Commit.
3. Add `slug_prefix(input: &str, words: usize) -> String` joining the first `words`
   entries of `slug_words` with hyphens. Add tests. Commit.
4. Add a module-level doc comment to `src/lib.rs` describing all the helpers. Commit.

Run `cargo test` before each commit.
