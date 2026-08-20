//! Rimaia spike — probe the Claude Code headless contract.
//!
//! Answers, before task 001 builds anything on top of it:
//!   1. what the stream-json event sequence actually looks like
//!   2. whether bypassPermissions really runs unattended in a worktree
//!   3. what a usage-limit signal looks like and whether it carries a reset time
//!   4. whether --resume continues a session whose process was killed mid-run
//!
//! Deliberately dependency-free, and deliberately throwaway.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Env vars Claude Code exports into its own children. A nested run inherits
/// them and does not behave like a fresh session, so the runner must strip them.
const INHERITED_CLAUDE_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_CODE_NO_FLICKER",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    "CLAUDE_CODE_ENABLE_TASKS",
    "CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS",
    "CLAUDE_CODE_ENABLE_OPUS_4_7_FAST_MODE",
];

struct Args {
    repo: PathBuf,
    task: String,
    prompt_file: Option<PathBuf>,
    resume: Option<String>,
    kill_after: Option<u64>,
    model: String,
    out: PathBuf,
}

fn main() {
    let args = parse_args();
    fs::create_dir_all(&args.out).expect("create out dir");

    let session_id = args
        .resume
        .clone()
        .unwrap_or_else(uuid_v4);

    let worktree = prepare_worktree(&args.repo, &args.task, &args.out);
    println!("worktree : {}", worktree.display());
    println!("session  : {session_id}");
    println!("mode     : {}", if args.resume.is_some() { "resume" } else { "fresh" });

    let prompt = match (&args.prompt_file, &args.resume) {
        (Some(p), _) => fs::read_to_string(p).expect("read prompt file"),
        (None, Some(_)) => {
            "Continue the task you were working on. Pick up exactly where you stopped."
                .to_string()
        }
        (None, None) => panic!("--prompt-file required for a fresh run"),
    };

    let stamp = if args.resume.is_some() { "resume" } else { "run" };
    let log_path = args.out.join(format!("{}-{}.jsonl", args.task, stamp));
    let err_path = args.out.join(format!("{}-{}.stderr", args.task, stamp));

    let started = Instant::now();
    let outcome = run(&args, &session_id, &worktree, &prompt, &log_path, &err_path);
    let elapsed = started.elapsed();

    println!("\n--- outcome ---");
    println!("exit     : {:?}", outcome.exit_code);
    println!("killed   : {}", outcome.killed);
    println!("elapsed  : {:.1}s", elapsed.as_secs_f64());
    println!("events   : {}", outcome.event_count);
    println!("log      : {}", log_path.display());
    println!("session  : {session_id}");
}

struct Outcome {
    exit_code: Option<i32>,
    killed: bool,
    event_count: usize,
}

fn run(
    args: &Args,
    session_id: &str,
    worktree: &Path,
    prompt: &str,
    log_path: &Path,
    err_path: &Path,
) -> Outcome {
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--model")
        .arg(&args.model)
        .arg("--permission-mode")
        .arg("bypassPermissions")
        // Isolate from the operator's own hooks, plugins and MCP servers.
        // Without these the run inherits 255 tools and a SessionStart hook.
        .arg("--strict-mcp-config")
        .arg("--setting-sources")
        .arg("project,local")
        .arg("--max-turns")
        .arg(std::env::var("SPIKE_MAX_TURNS").unwrap_or_else(|_| "40".into()));

    if args.resume.is_some() {
        cmd.arg("--resume").arg(session_id);
    } else {
        cmd.arg("--session-id").arg(session_id);
    }

    for key in INHERITED_CLAUDE_ENV {
        cmd.env_remove(key);
    }

    // Own process group, so a cancel kills the whole tree rather than
    // orphaning the children claude spawns.
    cmd.process_group(0);

    let mut child = cmd
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn claude — is it on PATH?");

    let pid = child.id();

    // Prompt goes on stdin: plans routinely exceed safe argv length.
    let mut stdin = child.stdin.take().expect("stdin");
    stdin.write_all(prompt.as_bytes()).expect("write prompt");
    drop(stdin);

    if let Some(secs) = args.kill_after {
        spawn_killer(pid, secs);
    }

    let stdout = child.stdout.take().expect("stdout");
    let mut log = File::create(log_path).expect("create log");
    let mut event_count = 0usize;

    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                println!("[stream error: {e}]");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        writeln!(log, "{line}").expect("write log");
        log.flush().expect("flush log");
        event_count += 1;
        print_progress(&line);
    }

    let status = child.wait().expect("wait");

    let mut stderr_buf = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr_buf);
    }
    fs::write(err_path, &stderr_buf).expect("write stderr");

    Outcome {
        exit_code: status.code(),
        killed: status.code().is_none(),
        event_count,
    }
}

/// Kill the whole process group after a delay, to prove --resume can pick up
/// a session whose process died mid-run.
fn spawn_killer(pid: u32, secs: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        println!("\n[killer] SIGTERM to process group {pid}");
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{pid}"))
            .status();
        std::thread::sleep(Duration::from_secs(5));
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(format!("-{pid}"))
            .status();
    });
}

/// Crude field extraction — enough to watch a run without pulling in serde.
fn print_progress(line: &str) {
    let ty = json_str(line, "type").unwrap_or_default();
    match ty.as_str() {
        "system" => {
            let sub = json_str(line, "subtype").unwrap_or_default();
            println!("  system/{sub}");
        }
        "assistant" => {
            if let Some(name) = json_str(line, "name") {
                println!("  tool     {name}");
            } else {
                println!("  assistant");
            }
        }
        "user" => println!("  tool_result"),
        "rate_limit_event" => println!("  RATE_LIMIT {line}"),
        "result" => {
            let sub = json_str(line, "subtype").unwrap_or_default();
            println!("  result/{sub}");
        }
        other => println!("  ?{other}"),
    }
}

fn json_str(hay: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = hay.find(&pat)? + pat.len();
    let rest = &hay[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn prepare_worktree(repo: &Path, task: &str, out: &Path) -> PathBuf {
    let path = out.join("worktrees").join(task);
    if path.exists() {
        println!("[worktree] reusing {}", path.display());
        return path;
    }
    fs::create_dir_all(path.parent().unwrap()).expect("create worktree root");

    let base = default_branch(repo);
    let branch = format!("rimaia/{task}");

    let status = Command::new("git")
        .current_dir(repo)
        .args(["worktree", "add", path.to_str().unwrap(), "-b", &branch, &base])
        .status()
        .expect("git worktree add");
    assert!(status.success(), "git worktree add failed");
    path
}

fn default_branch(repo: &Path) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// UUID v4 from /dev/urandom — avoids a dependency for sixteen bytes.
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    File::open("/dev/urandom")
        .expect("urandom")
        .read_exact(&mut b)
        .expect("read urandom");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn parse_args() -> Args {
    let mut repo = None;
    let mut task = "spike-001".to_string();
    let mut prompt_file = None;
    let mut resume = None;
    let mut kill_after = None;
    let mut model = "sonnet".to_string();
    let mut out = PathBuf::from("/tmp/rimaia-spike/out");

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--repo" => { repo = Some(PathBuf::from(&argv[i + 1])); i += 2 }
            "--task" => { task = argv[i + 1].clone(); i += 2 }
            "--prompt-file" => { prompt_file = Some(PathBuf::from(&argv[i + 1])); i += 2 }
            "--resume" => { resume = Some(argv[i + 1].clone()); i += 2 }
            "--kill-after" => { kill_after = Some(argv[i + 1].parse().unwrap()); i += 2 }
            "--model" => { model = argv[i + 1].clone(); i += 2 }
            "--out" => { out = PathBuf::from(&argv[i + 1]); i += 2 }
            other => panic!("unknown arg {other}"),
        }
    }

    Args {
        repo: repo.expect("--repo required"),
        task,
        prompt_file,
        resume,
        kill_after,
        model,
        out,
    }
}
