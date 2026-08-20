# Base instructions

Commit as you work, with focused commits and clear messages.
Run the project's tests before you finish.
If you cannot complete the task, stop, commit what you have, and explain what is blocking you.
Do not push and do not open a pull request — this is a local-only spike.

# Task

Repository: spike-testrepo
Branch: rimaia/spike-a

# Plan

Add a `truncate_slug(input: &str, max_len: usize) -> String` function to `src/lib.rs`.

It should slugify the input using the existing `slugify` function, then truncate the
result to at most `max_len` characters without leaving a trailing hyphen.

Add unit tests covering: a short input that is unchanged, a long input that gets
truncated, and an input whose truncation would land on a hyphen.

Run `cargo test` and make sure everything passes before committing.
