# Test fixtures

- **`cli/`** — recorded and synthesized `claude` CLI `stream-json` streams. See
  [`cli/README.md`](cli/README.md) for what each scenario is and which ones must never be
  re-recorded. Reached from Rust through `rimaia_core::testing::fixtures`.
- **`make-test-repo.sh`** — builds the throwaway git repository the fixtures in `cli/` were
  recorded against: a small Rust crate with a `slugify` helper and a passing test. Run it
  by hand (`./make-test-repo.sh [destination]`); no test executes it. It is the ground
  truth for tasks 007, 008 and 009 — when a run's behaviour needs checking against a real
  repository rather than a replayed stream, this is the repository to point it at.

Do not confuse this with `rimaia_core::testing::TempRepo`, which builds a repository *in
process* for git and worktree assertions. `make-test-repo.sh` exists for the other case: a
repository with real work in it that a Claude Code run can be asked to do.
