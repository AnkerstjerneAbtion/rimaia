//! A stand-in for the `claude` CLI: one shell script that replays recorded
//! fixture streams, and behaves differently per **task and attempt**.
//!
//! # Why this is not a mock
//!
//! ADR-0015 and CLAUDE.md both refuse a trait double for the CLI: what the
//! runner has to be right about is pipes, argv, stdin and exit codes, and a
//! mocked trait proves the mock works. So this is a real executable that a real
//! `Command` spawns, and the bytes it writes are the recorded ones under
//! [`fixtures`](super::fixtures). Two reasons it is not the real `claude`
//! either: a real child costs the operator money, and `cargo test` is routinely
//! run from inside a Claude Code session, so it would inherit exactly the
//! `CLAUDE_*` variables `runner::process` exists to strip.
//!
//! # Task **and attempt**, which is what task 014 needed
//!
//! `tests/scheduler.rs` grew this shape for task 009 and dispatched on the task
//! alone — a queue's interesting scenarios are the ones where the second task
//! does not behave like the first, and `RunnerConfig` carries one program for
//! the whole queue, so the difference has to live inside the script. The
//! worktree is `<worktree-root>/<task-id>` (ADR-0005), which makes the child's
//! own working directory the dispatch key.
//!
//! ADR-0011's retry loop needs one more axis. "Fail the first attempt, succeed
//! the second" is the whole of what a retry test asserts, and one task now
//! spawns several processes. The attempt number is derived rather than passed:
//! the script counts its own `start <task>` lines in the spawn log *before*
//! appending its own, which needs nothing threaded through the runner and
//! cannot disagree with what actually happened.
//!
//! # Why it lives here rather than in a test binary
//!
//! It was a private struct in `tests/scheduler.rs`, whose header argued against
//! sharing it: "each integration test is its own binary, and a `mod common`
//! shared between them would make either file's stand-in awkward to change for
//! the other's sake." That argument was about `mod common`, and it is still
//! right about `mod common`. A `testing`-feature module is not one — it is the
//! same door [`TempRepo`](super::TempRepo), [`TestClock`](super::TestClock) and
//! [`fixtures`](super::fixtures) already come through, versioned with the code
//! it stands in for. The alternative was a third copy of two hundred lines.
//!
//! # It records what it was asked to do
//!
//! [`argv`](FakeCli::argv) and [`stdin`](FakeCli::stdin) are per attempt, which
//! is what lets a test assert that the *second* attempt carried `--resume` and
//! the continuation prompt while the first carried `--session-id` and the
//! composed one. Asserting the exact vector is the same class of contract as
//! prompt composition — see `runner::process`'s own header on why.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::testing::fixtures::{fixture_lines, fixture_path};

/// One script, dispatching on the task whose worktree it was started in and on
/// which attempt of that task this is.
#[derive(Debug)]
pub struct FakeCli {
    /// Held for its `Drop`; every path below points inside it.
    dir: TempDir,
}

impl Default for FakeCli {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCli {
    /// A stand-in that replays `success.jsonl` for every task nothing else has
    /// been said about.
    pub fn new() -> Self {
        let cli = Self {
            dir: tempfile::Builder::new()
                .prefix("rimaia-fake-cli-")
                .tempdir()
                .expect("temp dir for the stand-in CLI"),
        };
        cli.write_plan("default", &Self::replay_plan("success", 0));
        cli.write_script();
        cli
    }

    /// What to put in [`RunnerConfig::program`](crate::runner::RunnerConfig).
    pub fn program(&self) -> PathBuf {
        self.path("claude")
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Replays `fixture` for every attempt of `task_id`, exiting with `code`.
    pub fn replays(&self, task_id: &str, fixture: &str, code: i32) {
        self.write_plan(task_id, &Self::replay_plan(fixture, code));
    }

    /// Replays `fixture` for one specific attempt, leaving the others to
    /// whatever the task-wide or default directive says.
    ///
    /// Attempts are 1-based, matching `runs.attempt`. This is the whole point
    /// of the second dispatch axis: "the first attempt hits a wall and the
    /// second, resumed, succeeds" is one call to this per attempt.
    pub fn replays_on_attempt(&self, task_id: &str, attempt: usize, fixture: &str, code: i32) {
        self.write_plan(
            &format!("{task_id}-{attempt}"),
            &Self::replay_plan(fixture, code),
        );
    }

    fn replay_plan(fixture: &str, code: i32) -> [String; 4] {
        [
            "replay".to_string(),
            fixture_path(fixture).display().to_string(),
            code.to_string(),
            String::new(),
        ]
    }

    /// Replays the first `head` lines of `fixture` for `task_id`, then waits
    /// for the returned gate file to appear before replaying the rest.
    ///
    /// Not a new fixture: the same recorded bytes handed over in two pieces so
    /// a test can act between them, exactly as `tests/runner_process.rs` splits
    /// a recording around a signal.
    pub fn gates(&self, task_id: &str, fixture: &str, head: usize) -> PathBuf {
        let (head_file, rest_file) = self.split(task_id, fixture, head);
        let gate = self.path(&format!("gate-{task_id}"));
        self.write_plan(
            task_id,
            &[
                "gate".to_string(),
                head_file.display().to_string(),
                rest_file.display().to_string(),
                gate.display().to_string(),
            ],
        );
        gate
    }

    /// Replays the first `head` lines of `fixture` for `task_id` and then waits
    /// to be stopped — a run that is going nowhere until somebody cancels it.
    pub fn hangs(&self, task_id: &str, fixture: &str, head: usize) {
        let (head_file, _) = self.split(task_id, fixture, head);
        self.write_plan(
            task_id,
            &[
                "hang".to_string(),
                head_file.display().to_string(),
                String::new(),
                String::new(),
            ],
        );
    }

    /// The task ids the stand-in was started in, in the order it was started —
    /// one entry per *process*, so a task retried three times appears three
    /// times.
    pub fn started(&self) -> Vec<String> {
        self.spawns()
            .into_iter()
            .filter_map(|line| line.strip_prefix("start ").map(str::to_string))
            .collect()
    }

    /// How many processes this task has had. The process-level answer beside
    /// the row-level one `runs.attempt` gives.
    pub fn attempts(&self, task_id: &str) -> usize {
        self.started().iter().filter(|id| *id == task_id).count()
    }

    /// The exact argument vector attempt `attempt` of `task_id` was spawned
    /// with, one element per line as the script received it.
    ///
    /// Panics for an attempt that never happened — a test asserting about a
    /// resume that did not occur should fail on the resume, not on an empty
    /// vector that quietly matches nothing.
    pub fn argv(&self, task_id: &str, attempt: usize) -> Vec<String> {
        self.read(&format!("argv-{task_id}-{attempt}"))
            .unwrap_or_else(|| {
                panic!("attempt {attempt} of {task_id} was never spawned: {:?}", self.spawns())
            })
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// What was delivered on that attempt's stdin — the prompt, verbatim.
    pub fn stdin(&self, task_id: &str, attempt: usize) -> String {
        self.read(&format!("stdin-{task_id}-{attempt}"))
            .unwrap_or_else(|| {
                panic!("attempt {attempt} of {task_id} was never spawned: {:?}", self.spawns())
            })
    }

    /// The most stand-ins that were alive at the same moment, walked off their
    /// own start/end log.
    ///
    /// The witness both assertions below rest on, and the only one available:
    /// two `runs` rows are both `running` for a while whether or not the
    /// processes ever coexisted, and `started_at` comes from a fake clock. A
    /// stand-in that is killed or hangs never writes its `end`, which this
    /// reads as still open — correct, because it is.
    pub fn peak_overlap(&self) -> usize {
        let mut open = 0usize;
        let mut peak = 0usize;
        for line in self.spawns() {
            match line.split_once(' ') {
                Some(("start", _)) => {
                    open += 1;
                    peak = peak.max(open);
                }
                Some(("end", _)) => open = open.saturating_sub(1),
                _ => panic!("unreadable spawn log line: {line}"),
            }
        }
        peak
    }

    /// Asserts no two runs overlapped — the sequential half of ADR-0010's
    /// sequential mode, read off the processes themselves rather than off the
    /// rows they wrote.
    pub fn assert_never_two_at_once(&self) {
        assert_eq!(
            self.peak_overlap(),
            1,
            "two runs overlapped where exactly one was allowed: {:?}",
            self.spawns(),
        );
    }

    /// The inverse, for task 012: at least `at_least` stand-ins were alive at
    /// the same instant.
    ///
    /// `at_least` rather than exactly, because the assertion is about
    /// parallelism happening and not about the scheduler having reached its
    /// limit on the exact pass a test looked.
    pub fn assert_overlapped(&self, at_least: usize) {
        let peak = self.peak_overlap();
        assert!(
            peak >= at_least,
            "at most {peak} runs were ever alive at once; expected at least {at_least}: {:?}",
            self.spawns(),
        );
    }

    /// The raw `start`/`end` log.
    pub fn spawns(&self) -> Vec<String> {
        match self.read("spawns") {
            Some(log) => log.lines().map(str::to_string).collect(),
            // Nothing has been spawned yet, which is a result and not a
            // failure — several tests assert exactly that.
            None => Vec::new(),
        }
    }

    /// Makes every future `--version` probe block until
    /// [`release_version_probe`](Self::release_version_probe) is called —
    /// widening, for a test, the window `try_step` leaves open between
    /// registering its `CancelSignal` and actually claiming a task. The probe
    /// otherwise answers instantly, which is correct for every other test and
    /// exactly why this is opt-in rather than the default.
    pub fn hold_version_probe(&self) {
        std::fs::write(self.path("version-hold"), "").expect("arm the version-probe gate");
    }

    /// Releases a probe blocked by [`hold_version_probe`](Self::hold_version_probe).
    pub fn release_version_probe(&self) {
        std::fs::write(self.path("version-go"), "").expect("release the version-probe gate");
    }

    fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.path(name)).ok()
    }

    fn split(&self, task_id: &str, fixture: &str, head: usize) -> (PathBuf, PathBuf) {
        let lines: Vec<String> = fixture_lines(fixture).collect();
        assert!(head < lines.len(), "{fixture} is shorter than {head} lines");

        let head_file = self.path(&format!("head-{task_id}.jsonl"));
        let rest_file = self.path(&format!("rest-{task_id}.jsonl"));
        std::fs::write(&head_file, lines[..head].join("\n") + "\n").expect("write the head");
        std::fs::write(&rest_file, lines[head..].join("\n") + "\n").expect("write the rest");
        (head_file, rest_file)
    }

    /// One directive, one line per field, so a path containing a space survives
    /// `read` — the same reason the production code builds argument vectors and
    /// never `sh -c`.
    fn write_plan(&self, key: &str, fields: &[String; 4]) {
        std::fs::write(self.path(&format!("plan-{key}")), fields.join("\n") + "\n")
            .expect("write a stand-in directive");
    }

    /// A shebang script, executed directly rather than through `sh -c`.
    ///
    /// `--version` short-circuits before anything else: the runner probes for
    /// the prerequisite before it starts a run, and the probe runs in Rimaia's
    /// own working directory, where the dispatch below would find no task.
    /// It waits on `version-hold`/`version-go` first, so a test can hold it
    /// open — see [`hold_version_probe`](Self::hold_version_probe).
    ///
    /// `pwd -P` rather than the `pwd` builtin's default, because `PWD` is
    /// inherited from the parent and `Command::current_dir` does not update it.
    ///
    /// The attempt number is counted off the spawn log **before** this process
    /// appends its own line, so the first run of a task is attempt 1. `grep -c`
    /// on a missing file is an error rather than zero, hence the guard.
    fn write_script(&self) {
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = '--version' ]; then\n\
             if [ -f '{dir}/version-hold' ]; then\n\
             while [ ! -f '{dir}/version-go' ]; do sleep 0.02; done\n\
             fi\n\
             echo '2.1.234 (Claude Code)'; exit 0\n\
             fi\n\
             dir='{dir}'\n\
             task=\"$(basename \"$(pwd -P)\")\"\n\
             if [ -f \"$dir/spawns\" ]; then\n\
             attempt=$(( $(grep -c \"^start $task$\" \"$dir/spawns\") + 1 ))\n\
             else\n\
             attempt=1\n\
             fi\n\
             printf '%s\\n' \"$@\" > \"$dir/argv-$task-$attempt\"\n\
             cat > \"$dir/stdin-$task-$attempt\"\n\
             printf 'start %s\\n' \"$task\" >> \"$dir/spawns\"\n\
             plan=\"$dir/plan-$task-$attempt\"\n\
             if [ ! -f \"$plan\" ]; then plan=\"$dir/plan-$task\"; fi\n\
             if [ ! -f \"$plan\" ]; then plan=\"$dir/plan-default\"; fi\n\
             {{ read -r mode; read -r one; read -r two; read -r three; }} < \"$plan\"\n\
             case \"$mode\" in\n\
             replay)\n\
               cat \"$one\"\n\
               printf 'end %s\\n' \"$task\" >> \"$dir/spawns\"\n\
               exit \"$two\"\n\
               ;;\n\
             gate)\n\
               cat \"$one\"\n\
               while [ ! -f \"$three\" ]; do sleep 0.02; done\n\
               cat \"$two\"\n\
               printf 'end %s\\n' \"$task\" >> \"$dir/spawns\"\n\
               exit 0\n\
               ;;\n\
             hang)\n\
               cat \"$one\"\n\
               sleep 300\n\
               ;;\n\
             esac\n",
            dir = self.dir.path().display(),
        );

        let program = self.program();
        std::fs::write(&program, script).expect("write the stand-in CLI");
        make_executable(&program);
    }
}

/// Releases a stand-in gated by [`FakeCli::gates`].
pub fn open_gate(gate: &Path) {
    std::fs::write(gate, "go\n").expect("open the gate");
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in executable");
}

/// The stand-in is a POSIX shell script, so there is no honest port here — the
/// same gap `runner::process::set_process_group` names rather than stubs.
#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
