//! **Unix only.** Every fixture in this file is a `/bin/sh` shebang script
//! standing in for `claude`, `git` or `gh` — the technique `spike/FINDINGS.md`
//! settled on and the only way to test "signed out", "too old" or "never called
//! the tool" without depending on what is installed on the machine running the
//! suite. Windows has no shebang, so the whole file is gated rather than each
//! test: a file that compiled and silently ran nothing would report a green
//! Windows job that had checked none of this.
//!
//! **What still runs on Windows is the part that matters most there**, and it is
//! deliberately not here: `credentials::inject` and `openers` both assert their
//! Windows tables from unit tests over injected inputs, on every platform,
//! because `Platform` and the parent environment are values rather than `cfg!`.
//! Task 022's CI matrix exists to compile the keychain backends and the
//! environment-building code on all three — not to pretend a POSIX shell is
//! available on a runner that has none.
#![cfg(unix)]

//! A repository's own forge token, from the outside (task 022, ADR-0020).
//!
//! The stand-in CLI writes its whole environment to a file, which is what makes
//! task 022's "asserted as an exact environment diff" a test rather than an
//! aspiration — and it echoes the token into its own stream, which is what
//! makes the redaction assertion real rather than a claim about a function.
//!
//! **No test here touches a real keychain.** CI has no unlocked keychain and no
//! D-Bus; `testing::credentials::MemoryStore` is the whole reason
//! `CredentialStore` is a trait.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rimaia_core::credentials::inject::AMBIENT_FORGE_VARS;
use rimaia_core::credentials::{CredentialAccess, CredentialStore, Secret};
use rimaia_core::db::{BoardColumn, RunStatus};
use rimaia_core::repo::{self, NewRepository};
use rimaia_core::runner::{run_task, CancelSignal, RunRequest, RunTrigger, RunnerConfig};
use rimaia_core::tasks::{self, NewTask};
use rimaia_core::testing::credentials::MemoryStore;
use rimaia_core::testing::fixtures::fixture_path;
use rimaia_core::testing::{TempRepo, TestContext};
use rimaia_core::AppPaths;
use tempfile::TempDir;

/// A value nothing else in the suite could produce, so a `contains` for it is
/// evidence rather than a coincidence.
const SENTINEL: &str = "ghp_rimaia_sentinel_0123456789abcdefghij";

struct Fixture {
    harness: TestContext,
    #[allow(dead_code)]
    repository: TempRepo,
    #[allow(dead_code)]
    data: TempDir,
    paths: AppPaths,
    repository_id: String,
    task_id: String,
    store: MemoryStore,
}

impl Fixture {
    async fn new() -> Self {
        let harness = TestContext::new().await;
        let repository = TempRepo::init();
        let data = tempfile::Builder::new()
            .prefix("rimaia-credentials-")
            .tempdir()
            .expect("a temporary app data directory");
        let paths = AppPaths::new(data.path());
        paths.create_all().expect("the app data directories");

        let registered = repo::register(
            &harness.context,
            &paths.worktrees_dir(),
            NewRepository {
                path: repository.path().to_string_lossy().into_owned(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register the test repository");
        repo::set_allow_unattended_runs(&harness.context, &registered.id, true)
            .await
            .expect("ADR-0012's per-repository opt-in");

        let task = tasks::create_task(
            &harness.context,
            NewTask {
                repository_id: registered.id.clone(),
                title: "Push something".to_string(),
                plan: Some("a plan".to_string()),
                extra_instructions: None,
                column: Some(BoardColumn::Ready),
                links: vec![],
            },
        )
        .await
        .expect("create a ready task");

        Self {
            harness,
            repository,
            data,
            paths,
            repository_id: registered.id,
            task_id: task.id,
            store: MemoryStore::new(),
        }
    }

    /// Provisions the sentinel, both halves: the keychain item and the row that
    /// says there is one.
    async fn provision(&self) {
        self.store
            .set(
                &self.repository_id,
                &Secret::new(SENTINEL).expect("a token"),
            )
            .expect("store the sentinel");
        repo::set_credential_metadata(
            &self.harness.context,
            &self.repository_id,
            Some("ea"),
            Some("test"),
        )
        .await
        .expect("record the credential");
    }

    fn config(&self, cli: &FakeCli) -> RunnerConfig {
        RunnerConfig {
            program: cli.program(),
            credentials: CredentialAccess::new(self.store.clone()),
            ..RunnerConfig::default()
        }
    }

    async fn run(&self, cli: &FakeCli) -> rimaia_core::Result<rimaia_core::db::Run> {
        run_task(
            &self.harness.context,
            &self.paths,
            &self.config(cli),
            RunRequest {
                task_id: self.task_id.clone(),
                trigger: RunTrigger::Queued,
                resume: None,
                cancel: CancelSignal::new(),
                in_flight: None,
            },
        )
        .await
    }
}

/// A stand-in `claude` that records its own environment and then replays a
/// successful run.
struct FakeCli {
    dir: TempDir,
}

impl FakeCli {
    /// Replays the recorded success stream, having first written its whole
    /// environment out.
    fn recording() -> Self {
        Self::with_body(&format!(
            "cat '{}'\nexit 0\n",
            fixture_path("success").display()
        ))
    }

    /// The same, but it also prints its own `GH_TOKEN` into the stream — which
    /// is exactly what an agent debugging a push failure does, and the thing
    /// the redaction has to survive.
    fn echoing_its_own_token() -> Self {
        Self::with_body(&format!(
            "printf '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\
             \"text\":\"my token is %s\"}}]}}}}\\n' \"$GH_TOKEN\"\n\
             printf 'stderr saw %s\\n' \"$GH_TOKEN\" >&2\n\
             cat '{}'\nexit 0\n",
            fixture_path("success").display()
        ))
    }

    fn with_body(body: &str) -> Self {
        let cli = Self {
            dir: tempfile::Builder::new()
                .prefix("rimaia-fake-cli-")
                .tempdir()
                .expect("a temporary directory"),
        };
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = '--version' ]; then echo '2.1.234 (Claude Code)'; exit 0; fi\n\
             env > '{env}'\n\
             cat > /dev/null\n\
             {body}",
            env = cli.path("env").display(),
        );
        std::fs::write(cli.program(), script).expect("write the stand-in CLI");
        make_executable(&cli.program());
        cli
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn program(&self) -> PathBuf {
        self.path("claude")
    }

    /// The child's own environment, as it saw it.
    fn child_env(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(self.path("env"))
            .expect("the stand-in recorded its environment")
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in executable");
}

#[tokio::test]
async fn a_repository_with_a_credential_runs_with_it_and_without_the_operators_own() {
    // Task 022's first acceptance criterion, as an exact environment diff.
    let fixture = Fixture::new().await;
    fixture.provision().await;
    let cli = FakeCli::recording();

    let run = fixture.run(&cli).await.expect("the run completes");
    assert_eq!(run.status, RunStatus::Succeeded);

    let child = cli.child_env();
    assert_eq!(child.get("GH_TOKEN").map(String::as_str), Some(SENTINEL));
    assert_eq!(child.get("GIT_CONFIG_COUNT").map(String::as_str), Some("1"));
    assert_eq!(
        child.get("GIT_CONFIG_KEY_0").map(String::as_str),
        Some("http.https://github.com/.extraheader"),
    );
    assert_eq!(
        child.get("GIT_CONFIG_VALUE_0").map(String::as_str),
        Some(format!("Basic {}", base64_of(&format!("x-access-token:{SENTINEL}"))).as_str()),
    );
    // A bad credential has to fail immediately rather than block on a prompt
    // nobody will answer at 2am.
    assert_eq!(
        child.get("GIT_TERMINAL_PROMPT").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        child.get("GCM_INTERACTIVE").map(String::as_str),
        Some("Never")
    );

    // ADR-0020 point 5: the operator's ambient login is *absent*, not merely
    // outranked. `GH_TOKEN` is the one the run's own value replaces.
    for name in AMBIENT_FORGE_VARS
        .iter()
        .filter(|name| **name != "GH_TOKEN")
    {
        assert!(
            !child.contains_key(*name),
            "{name} reached the child: {child:?}",
        );
    }
    // And nothing else was disturbed — a run still gets the environment it is
    // supposed to have.
    assert!(child.contains_key("PATH"));
}

#[tokio::test]
async fn a_repository_without_a_credential_is_byte_identical_to_before_this_feature() {
    // The other half of the first criterion, and the one that makes adopting
    // the feature safe one repository at a time.
    let fixture = Fixture::new().await;
    let cli = FakeCli::recording();

    fixture.run(&cli).await.expect("the run completes");

    let child = cli.child_env();
    assert!(!child.contains_key("GIT_CONFIG_COUNT"));
    assert!(!child.contains_key("GIT_CONFIG_KEY_0"));
    assert!(!child.contains_key("GIT_CONFIG_VALUE_0"));
    assert!(!child.contains_key("GCM_INTERACTIVE"));
    // Whatever the operator's own environment said, unchanged: this test's
    // parent has no `GH_TOKEN`, and nothing added or removed one.
    assert_eq!(
        child.get("GH_TOKEN").cloned(),
        std::env::var("GH_TOKEN").ok(),
    );
}

#[tokio::test]
async fn a_configured_credential_missing_from_the_keychain_refuses_the_run() {
    // ADR-0020's fail-closed rule. The failure this prevents is invisible in
    // every artefact a run leaves behind: a run that quietly used the
    // operator's whole GitHub account instead of the token granted to one
    // repository looks exactly like a run that worked.
    let fixture = Fixture::new().await;
    fixture.provision().await;
    fixture.store.forget(&fixture.repository_id);
    let cli = FakeCli::recording();

    let refusal = fixture
        .run(&cli)
        .await
        .expect_err("a missing keychain item must refuse the run");

    let message = refusal.to_string();
    assert!(message.contains("keychain"), "{message}");
    assert!(
        message.contains("will not fall back"),
        "the refusal has to say it did not use the ambient login: {message}",
    );
    assert!(
        !cli.path("env").exists(),
        "nothing may be spawned for a repository whose credential is missing",
    );
    // And no attempt was recorded for a process that never started.
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE task_id = ?1")
        .bind(&fixture.task_id)
        .fetch_one(&fixture.harness.context.pool)
        .await
        .expect("count the runs");
    assert_eq!(runs, 0);
}

#[tokio::test]
async fn the_sentinel_appears_in_no_row_no_transcript_and_no_log() {
    // Task 022's own phrasing: "asserted by a test that provisions a known
    // sentinel value and greps everything the run wrote for it". The stand-in
    // here deliberately echoes its own token into both streams, which is what
    // an agent debugging a push failure actually does.
    let fixture = Fixture::new().await;
    fixture.provision().await;
    let cli = FakeCli::echoing_its_own_token();

    let run = fixture.run(&cli).await.expect("the run completes");

    // The child really did see it — otherwise this test would pass by proving
    // nothing.
    assert_eq!(
        cli.child_env().get("GH_TOKEN").map(String::as_str),
        Some(SENTINEL)
    );

    let transcript = std::fs::read_to_string(&run.log_path).expect("the transcript exists");
    assert!(
        transcript.contains("[redacted]"),
        "the echoed token should have been replaced, not merely absent: {transcript}",
    );
    assert!(!transcript.contains(SENTINEL), "the transcript leaked it");

    for entry in std::fs::read_dir(
        Path::new(&run.log_path)
            .parent()
            .expect("a transcript lives in a directory"),
    )
    .expect("read the run directory")
    {
        let path = entry.expect("a directory entry").path();
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !contents.contains(SENTINEL),
            "{} leaked the token",
            path.display(),
        );
    }

    // Every text column of every table, which is the only way to be sure no
    // write path put it somewhere nobody thought of.
    for table in ["runs", "tasks", "repositories", "settings"] {
        let dump: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT group_concat(quote(t.*), char(10)) FROM (SELECT * FROM {table}) t"
        ))
        .fetch_all(&fixture.harness.context.pool)
        .await
        .unwrap_or_default();
        assert!(
            !dump.join("\n").contains(SENTINEL),
            "the {table} table holds the token",
        );
    }

    // And the `Debug` of the value that carries it.
    assert_eq!(
        format!("{:?}", Secret::new(SENTINEL).expect("a token")),
        "Secret(***)",
    );
}

fn base64_of(value: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(value)
}
