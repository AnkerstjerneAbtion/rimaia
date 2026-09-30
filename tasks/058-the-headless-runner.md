---
id: "058"
title: The headless runner
milestone: v0.5
status: ready
depends_on: ["055", "056", "057"]
adrs: ["0027", "0030", "0031"]
size: M
---

# The headless runner

## Goal

Ship `rimaia-runner`, the third mode in
[ADR-0027](../docs/adr/0027-a-server-for-the-board-and-runners-for-the-work.md) point 4's
table: a runner with no window and no board, on a spare machine that only does work. It is
a binary in the existing `crates/runner` package, with a command line built on `clap`
(seam-contract D34), and these subcommands:

| Command | Does |
| --- | --- |
| `rimaia-runner pair <server> <code>` | Redeems a pairing code (ADR-0030 point 5), stores the `rmr_` token in the OS keychain and the runner's identity in `runner.db` |
| `rimaia-runner run` | Runs the runner loop against the server until it is told to stop |
| `rimaia-runner status` | Prints what this runner is, what it holds and whether it is running, from the local store alone |
| `rimaia-runner checkout add <path>` / `list` / `remove <repository>` | Maps a team repository to a local clone (ADR-0033 point 2) |
| `rimaia-runner consent <repository>` | Gives or withdraws this machine's unattended consent for one repository, behind ADR-0012's wording, at a terminal (ADR-0032 point 4) |
| `rimaia-runner service <systemd\|launchd>` | Prints a service definition for this binary. It installs nothing |

The binary is a thin adapter. Every rule it applies already lives in `rimaia-core` or in the
`rimaia-runner` library: pairing is 047's, the checkout mapping is 054's, consent is 041's
and 045's, the loop is 042's, leases are 043's and 053's, the run-scoped proxy is 055's, the
outbox is 056's and the push postcondition is 057's. This task composes them into one
process, adds the command line, and decides the handful of things a process with no window
has to decide on its own: where its data lives, where its token lives, what a signal means,
what its exit status says, and how a service manager should run it.

**Stopping the service stops that runner's queue** (seam-contract D15, per runner by
ADR-0031 point 6). "A headless runner's go signal is its service running": `run` turns the
switch on when it starts, and a clean shutdown turns it off and cancels its in-flight runs
through the same path a desktop quit uses.

## Why now

Tasks 052 to 057 built every part a runner needs over HTTP: the protocol and the HTTP board
adapter (052), the long poll, heartbeat and sleep recovery (053), checkouts by remote (054),
the run-scoped proxy (055), transcripts and the outbox (056), and the push postcondition
(057). So far only tests host them. The headless binary is the first production host that
has no board file and no window, which makes it the cleanest proof that the runner protocol
stands on its own. If a headless runner can pair, map a clone, take consent, claim, run,
push and report, then everything the connected desktop adds in 059 is interface.

It comes before 059 for a second reason. Both need the same composition: store, keychain
token, HTTP board, reconcile, proxy, outbox, heartbeat and loop, started in order and
stopped in reverse. Written here first, that composition has no Tauri in it, and 059
reuses it instead of growing a second one inside `src-tauri/src/lib.rs`. The plan's
end-of-M4 check needs "one headless runner and one connected desktop" against a local
server, and this task provides the first of them.

## Scope

**1. The binary.** `crates/runner/src/main.rs`, declared as
`[[bin]] name = "rimaia-runner"` in `crates/runner/Cargo.toml`. `clap` 4 with `derive` is
added exactly as D34 approves it: a `[workspace.dependencies]` line, referenced with
`{ workspace = true }` from `crates/runner` only. The `clap` types stay in `main.rs` and a
private `cli` module beside it. Everything a test needs to call is a library function in
`crates/runner/src/headless/` that takes its inputs as values and its terminal as a reader
and a writer, so the command line has nothing in it worth testing except parsing.

- `--version` prints `rimaia-runner <CARGO_PKG_VERSION>`. That string is what a person
  compares against the web UI's list of runners (ADR-0037 point 5, D34), and it is the
  `app_version` 053's heartbeat reports.
- The only production provider is `ProviderId::ClaudeCode`. There is no `--provider` flag
  until a second production provider exists. `pair` sends `ProviderId::as_str()`.
- **Exit status.** `0` after a clean shutdown. `2` for a usage error, which is `clap`'s own.
  `78` (`EX_CONFIG` in `sysexits.h`) for every refusal a person has to fix: not paired,
  already paired, the token missing from the keychain, the keychain unavailable, the data
  directory refused, the lock held, a revoked token, and `UpgradeRequired`. `1` for anything
  else. One function maps an `Error` and a `HostEnd` (point 6) to the status, and the
  service definitions (point 9) depend on it.

**2. Where a headless runner keeps its data.** Its own directory, never the desktop app's.
A desktop app and a headless runner on one machine are two runners, and two runners must
not share one `runner.db`: 040's adoption refuses a store that names another runner, and a
store with two loops behind it has two `InFlight` registries.

- `headless::paths::platform_fallback(os, env) -> Result<PathBuf>` is pure, and mirrors the
  directory Tauri resolves for the desktop, with the identifier `com.rimaia.runner` in
  place of `com.rimaia.app`:
  - macOS: `$HOME/Library/Application Support/com.rimaia.runner`;
  - Linux: `$XDG_DATA_HOME/com.rimaia.runner` when that is set and absolute, else
    `$HOME/.local/share/com.rimaia.runner`;
  - Windows: `%APPDATA%\com.rimaia.runner`.

  It reads the three variables through the `env` argument, never through `std::env`, so
  each platform's case is testable on every platform. A missing `HOME` or `APPDATA` is an
  `invalid` error that names `RIMAIA_DATA_DIR` as the way out. No `dirs` crate: it is in
  `Cargo.lock` through Tauri, but D34's list is closed, and three lines per platform do not
  argue for an exception.
- `RIMAIA_DATA_DIR` relocates it through the existing `AppPaths::resolve`, with the same
  refusals of a relative path, a `~` and a rooted-but-not-absolute path (ADR-0023). There is
  no `--data-dir` flag, so there is one way to say it.
- `runner.db` is `AppPaths::runner_db_file()` (040), opened with `RunnerStore::open`. A
  headless store runs no adoption (040: "a store opened without a board has no adoption to
  run"). There is no `rimaia.db` in this directory, ever.

**3. One `run` per data directory.** `run` takes an exclusive lock on
`<data>/runner.lock` with `std::fs::File::try_lock` (stable in the pinned 1.98) and holds it
until the process exits. A second `run` on the same directory exits `78` naming the lock
file. The OS releases the lock when the process dies, so a crash leaves nothing to clean up.
`status` opens the same file and tries the lock without keeping it, which is how it tells
whether a `run` is active. The other subcommands do not take the lock: they write
`runner.db` through the store, which is safe beside a running loop because the store is
SQLite in WAL mode (ADR-0003) and the loop re-reads checkouts and consent on every claim
pass (042).

**4. Pairing, and the token in the keychain.** `headless::pair(PairRequest, &dyn
CredentialStore, &RunnerStore, &dyn Clock, out) -> Result<Paired>`.

- **The server argument is an origin.** It is parsed with `reqwest::Url`, already a
  dependency, and normalised to `scheme://host[:port]` with no trailing slash. A path, a
  query, a fragment or credentials in the URL are refused. `https` is required, except for
  a loopback host (`127.0.0.1`, `::1`, `localhost`), where `http` is accepted for a local
  server and for tests. Everything the runner sends carries its token, and a token sent in
  clear to a remote host is a token given away. The web shell prints the exact line to type,
  `rimaia-runner pair <window.location.origin> <code>` (task 050), so the normal input is
  already an origin.
- **The label** is `--label`, or else the machine's hostname, read by running `hostname`
  as an argument vector, never through `sh -c`. The command exists on macOS, Linux and
  Windows. Its output is trimmed. An empty output or a failed spawn is an `invalid` error
  asking for `--label`. ADR-0030 point 5 makes the hostname the default.
- **The order is chosen so that a refusal never spends the code.** A code is useless once
  redeemed, whether or not the redemption that used it succeeded (047 Scope 7).
  1. Refuse if `runner_identity` already exists, naming the runner id and server it holds.
  2. Refuse if the keychain is `StoreStatus::Unavailable`, with its reason.
  3. `POST /api/v1/auth/pair` with `{ code, label, provider }` and 046's `Rimaia-Protocol`
     header. The answer is `{ runnerId, token }` (047).
  4. Save the token to the keychain.
  5. Write `runner_identity (1, runnerId, origin, now)`.

  If step 4 fails, nothing is written to `runner.db`, and the error names the runner id,
  which the person unpairs in the web UI. Step 5 comes last, so a store that says it is
  paired always has a token behind it, unless someone removed the token by hand (point 6
  covers that).
- **Where the token lives.** Through the existing `CredentialStore` trait
  (`crates/core/src/credentials/mod.rs`), under `KEYCHAIN_SERVICE`, with a key that names
  the runner's token and cannot collide with any repository key. If 054 gave the trait a
  typed key, add a `RunnerToken { runner_id }` variant. If the trait still takes a bare
  string, the account is `runner-token:<runner_id>`, which no UUIDv4 repository id (D10) can
  equal. The token is held as `Secret`, whose `Debug` already redacts it. It never reaches
  `runner.db`, stdout, a log line or an error message (D28: "The runner token is in the
  keychain, never here"). Task 059 stores the desktop's `rmd_` token the same way.
- **After pairing**, `pair` prints the runner id and the server, and the next command to
  type (`rimaia-runner checkout add <path to a clone>`). It then offers the run
  environment, because ADR-0032 point 6 says "pairing a runner recommends" `strict_local`
  for team use. `--run-environment <inherit|strict-local>` sets it without asking. At a
  terminal with no flag, `pair` asks, with `strict_local` as the recommended answer. With
  no terminal and no flag it writes nothing, so the default stays `inherit`, and it prints
  the recommendation. The value is written through 041's
  `db::settings::set_run_environment(&MachineContext, ..)`, so its rules stay in one place.
  Pairing does not change the `inherit` default for anyone who does not answer (CLAUDE.md's
  Gotchas). It asks the one person that ADR-0032 wanted asked.

**5. Checkouts and consent at a terminal.**

- **`checkout add <path> [--repository <id>]`** maps an existing clone through the same
  `rimaia-core` function 054 gave the connected desktop. That function verifies that the
  clone's `origin` normalises to the team repository's remote before it saves anything
  (ADR-0033 point 2). The runner finds candidate team repositories by the normalised
  remote through 054's lookup. If more than one team repository matches (the same remote
  registered in two of the owner's teams), `add` lists them and requires `--repository`.
  If none matches, it says which remote it looked for. The new checkout gets the default
  `worktree_root` that registration uses today, `max_concurrency` 1 and no consent. Then
  `add` reports the checkout set to the board through 054's `report_runner`, the same call
  `run` makes at startup. If the server cannot be reached, the checkout is still saved,
  with a warning that the board learns of it when `run` next starts.
- **`checkout list`** prints one line per checkout: the normalised remote (or the
  repository id when a solo-style row has none), the path, and `unattended: allowed` or
  `unattended: not allowed`.
- **`checkout remove <repository>`** accepts a repository id or a normalised remote, and
  removes the checkout. It is refused while worktrees remain for it (041's store contract,
  `a_checkout_with_worktrees_cannot_be_removed`), with the number of worktrees in the
  message. There is no `--force`. Removing worktrees has its own rules (D20), and this task
  does not give them a second door.
- **`consent <repository>`** gives this machine's unattended consent for one mapped
  checkout. It is the only way to give it on a headless runner, and it is given only at a
  terminal:
  - with no terminal on stdin (`std::io::IsTerminal`), it is refused with `78` and nothing
    printed but the refusal. There is no `--yes`. A consent that a script can give is not
    ADR-0012's "explicit, informed" opt-in, and ADR-0032 point 4 says the dialog's
    "wording does not soften";
  - at a terminal it prints, exactly (the remote is the example's):

    ```
    Unattended runs for github.com/acme/api on this machine

    Enabling unattended runs for "github.com/acme/api" means the agent can run any command in this repository's worktree, including network access and package installation, without asking.

    This consent is stored on this machine only. Your team's owners can forbid unattended runs in this repository, but nobody can grant them here except you.

    Type yes to allow unattended runs, or anything else to cancel:
    ```

    and only the exact answer `yes` (surrounding whitespace ignored) grants it. It then
    prints `Unattended runs allowed for github.com/acme/api on this machine.`. Any other
    answer prints `Nothing changed.` and writes nothing;
  - `consent <repository> --withdraw` asks nothing and takes the consent away at once.
    Taking a permission away needs no warning;
  - the second paragraph is composed from one constant,
    `rimaia_core::repo::UNATTENDED_RUNS_GRANT`, which this task adds by moving the sentence
    out of the doc comment on `repo::set_allow_unattended_runs`. The desktop's copy in
    `src/views/settings/RepositoriesSection.tsx` stays where it is. A Rust test holds the
    two together (Acceptance criteria);
  - the write goes through the function 041 gave `set_repository_unattended_runs`, so the
    change event and the store rule are the desktop's. Then `report_runner` tells the board,
    as for `checkout add`.

**6. `run`, and the host it builds.** `rimaia_runner::host` owns the composition, and the
binary calls it. If 053 to 057 already left a composition function for their tests, extend
that one and do not write a second.

```rust
pub struct HostConfig {
    pub paths: AppPaths,
    pub store: RunnerStore,
    pub credentials: Arc<dyn CredentialStore>,
    pub provider: Arc<dyn AgentProvider>,
    pub program: Option<PathBuf>, // the agent CLI; tests point it at FakeCli
    pub clock: Arc<dyn Clock>,
    pub presence: Presence,       // Absent for headless; 059 passes what the desktop knows
}
pub enum HostEnd { Shutdown, Unauthenticated(String), UpgradeRequired(String) }

impl RunnerHost {
    pub async fn start(config: HostConfig) -> Result<RunnerHost>;
    pub async fn ended(&self) -> HostEnd;   // resolves when the server refuses the runner
    pub async fn shutdown(self) -> Result<()>;
}
```

The names are indicative. What is fixed is the order of `start`, the reverse order of
`shutdown`, and what a `HostEnd` means.

- **`start`, in this order, each step failing like its neighbours with a step name and the
  file or URL involved (D11's shape):**
  1. read `runner_identity`. Refuse with `78` if it is absent ("This runner is not paired.
     Run `rimaia-runner pair <server> <code>` first.");
  2. read the token from the keychain. Refuse with `78` if it is absent, naming the runner
     id and saying to pair again;
  3. build 052's `HttpBoard` with the origin, the token and the provider, and one
     `LocalEvents` (048) for every machine-state writer;
  4. reconcile the leases this runner held, from `held_leases`, through 043's per-runner
     reconcile and 053's pin (ADR-0031 point 5). It never touches another runner's runs;
  5. `report_runner` (054): the checkout set, the consent per repository and the doctor
     result;
  6. start 056's outbox sender, then 055's run-scoped proxy, then 053's heartbeat;
  7. write `queue_state = running` through 041's `set_queue_state` and start 042's loop
     with the port, the store, one `InFlight` and the runner config.
- **The run-scoped proxy, and nothing else, on loopback.** The headless runner serves
  `/mcp/run/{token}` (ADR-0035 point 5) on `127.0.0.1`, on the runner's `mcp_port` setting
  if one is set and on an ephemeral port otherwise, because only the runner's own children
  need to find it and they are told the URL. It serves **no operator endpoint**.
  Reconfiguring a machine over MCP is a desktop's loopback endpoint (ADR-0035 point 6), and
  a headless runner's operator surface is the hosted `/mcp` (060). If 055 mounts the run
  routes only together with the operator routes, split the router so the run routes can be
  mounted alone. Do not serve the operator routes with a refusal body.
- **Every run on a headless runner is unattended.** `Presence::Absent` means nobody is at
  the machine, so a `Claim` whose trigger is a manual Run now, pressed in a browser for
  this runner (052), runs with ADR-0012's unattended posture behind the consent, never
  `acceptEdits` (ADR-0031 point 7: "Permission posture follows whether the owner is at the
  machine, not which button was pressed"). The desktop passes whatever presence it knows
  in 059.
- **`shutdown`, which is what a signal does.** On `SIGTERM` or `SIGINT` on Unix, and on
  Ctrl-C or Ctrl-Break on Windows (`tokio::signal`, already in the workspace's `tokio`
  features):
  1. stop the loop from claiming more, as `queue.shutdown()` does on the desktop;
  2. cancel every in-flight run through the normal cancel path (SIGTERM to the process
     group, then SIGKILL after `DEFAULT_GRACE_PERIOD`), and write `queue_state = paused`.
     This is `AppState::cancel_everything`'s behaviour (`src-tauri/src/state.rs`) without
     the Tauri state around it. A cancelled run is reported as the desktop's quit reports
     it, through `finish_run` and 056's outbox;
  3. wait, bounded as `src-tauri/src/lib.rs`'s `shut_down` bounds it, until nothing is in
     flight. The wait awaits the runs themselves, not a polling loop with a sleep;
  4. stop the heartbeat, the proxy and the outbox sender, in that order. Reports still in
     the outbox stay in `runner.db` and are sent at the next start (056). Shutdown does not
     wait for the server;
  5. release the lock by exiting `0`.
  A second signal during shutdown exits at once with `1`, after the children have already
  been signalled, as the desktop does when its deadline passes.
- **When the server refuses the runner itself.** An `Unauthenticated` answer to any runner
  request (the token was revoked, or the runner was unpaired in the web UI) and an
  `UpgradeRequired` answer (ADR-0037 point 4) each resolve `ended()`. The binary then runs
  the same `shutdown` and exits `78`, printing 047's or 046's sentence. For
  `UpgradeRequired`, that sentence includes the minimum version. A too-old runner stops
  taking work rather than taking it and misreporting it, which is ADR-0037's rule. A
  `Conflict` is not a `HostEnd`. It fences one lease, and 053's single reaction handles it
  (D31 point 11).
- **Logs.** The same shape as `src-tauri/src/logging.rs`: stderr and a daily file,
  `<data>/logs/rimaia-runner.log`, with `RIMAIA_LOG` as the filter and a default of
  `rimaia_runner=debug,rimaia_core=debug,warn`. See Notes for the dependency question this
  raises. `run` logs one line at start naming the version, the runner id, the server
  origin and the data directory, and one at exit naming why.

**7. `status`.** `headless::status(&RunnerStore, &dyn CredentialStore, lock_path, out)`
reads only the local store, the keychain's `status` (never the secret) and the lock. It
makes no request, so it works while the server is down, which is exactly when someone runs
it. It prints, in this order and in this shape:

```
Runner       8d1e2c1a-0f4b-4a5e-9d0c-3b7f2a9e6c11
Server       https://rimaia.example
Token        in the keychain
Service      running
Queue        running
Usage limit  not paused
Environment  strict_local
Checkouts    2
  github.com/acme/api  /Users/a/src/api  unattended: allowed
  github.com/acme/web  /Users/a/src/web  unattended: not allowed
Leases       1
  3f2c…  implementation  since 2026-10-04T01:12:00Z
Outbox       0 reports waiting
```

`Service` is `running` when the lock is held by another process and `stopped` otherwise.
`Token` is `in the keychain`, `missing`, or `keychain unavailable: <reason>`. An unpaired
store prints `Not paired.` and the `pair` line, and exits `78`. The label is not shown,
because it lives on the server's `runners` row and `runner_identity` does not carry it
(D28).

**8. What `run` does not decide.** Capacity, the per-repository cap, the usage-limit pause,
windows and the strategy ceiling are the runner settings 041, 042 and 045 already read from
`runner.db`, and the loop obeys them unchanged. The go signal is the service running. An
enabled schedule in `runner.db` still fires through 042's `tick_schedules`, but a headless
runner has no command to create one (Out of scope), so in practice there is none.

**9. Service definitions.** `rimaia-runner service systemd` and
`rimaia-runner service launchd` print a definition to stdout, filled in from this process.
They write no file and run no `systemctl` or `launchctl`. Installing is two documented
steps, and a command that writes into `~/Library/LaunchAgents` would be the one part of this
binary nobody reviews.

- **Inputs.** The absolute path of the running binary (`std::env::current_exe`), `PATH` as
  it is in the shell that prints the definition, and `RIMAIA_DATA_DIR` only when it was
  set. `PATH` is carried because a service manager's `PATH` is minimal (launchd's is
  `/usr/bin:/bin:/usr/sbin:/sbin`), and the agent CLI, `git` and `gh` are usually
  installed elsewhere. Without it the doctor's CLI check fails on every claim pass, and the
  runner does nothing all night.
- **systemd** (a user unit, `~/.config/systemd/user/rimaia-runner.service`): `Type=simple`;
  `ExecStart=<binary> run`, with the path quoted when it contains a space;
  `Environment=` lines for `PATH` and, if set, `RIMAIA_DATA_DIR`; `Restart=on-failure`;
  `RestartSec=60`; `KillMode=mixed`; `TimeoutStopSec=60`; `WantedBy=default.target`.
  `KillMode=mixed` sends the first `SIGTERM` to the runner alone. Under the default
  `control-group`, systemd would signal the agent processes at the same instant as the
  runner, and a run would end from a signal the runner did not send, before the runner
  had marked it cancelled. `TimeoutStopSec=60` leaves room for the grace period and the
  runner's bounded wait before systemd's `SIGKILL`.
- **launchd** (a user agent, `~/Library/LaunchAgents/com.rimaia.runner.plist`, label
  `com.rimaia.runner`): `ProgramArguments` of the binary and `run`; `RunAtLoad` true;
  `KeepAlive` with `SuccessfulExit` false; `ThrottleInterval` 60; `ExitTimeOut` 60;
  `EnvironmentVariables` for `PATH` and, if set, `RIMAIA_DATA_DIR`; and `StandardErrorPath`
  `<data>/logs/rimaia-runner.stderr.log`, so a panic before the subscriber is up still lands
  somewhere. Every value is XML-escaped.
- **Why both restart on failure, including `78`.** launchd cannot exclude one exit status
  from `KeepAlive`, and treating the two managers differently would give one runner two
  behaviours. A refused runner therefore starts again once a minute, logs its reason, and
  exits `78` again. That is one log line a minute, not a tight loop, and it means a runner
  refused for its version starts working by itself once its binary is replaced. A clean
  stop exits `0`, and neither manager restarts it.
- **Windows.** `service` has no Windows form. It says so and exits `78`. The binary builds,
  tests and runs in the foreground on Windows, as ADR-0002 requires. A Windows service
  wrapper is out of scope.

**10. Documentation.**

- `docs/headless-runner.md`, new. It covers: installing the binary (built with
  `cargo build --release -p rimaia-runner` until a release pipeline exists); the agent CLI
  signed in as the same user the service runs as; pairing from the web UI's line;
  `checkout add` and `consent`; running under launchd as a **LaunchAgent**, not a
  LaunchDaemon, because both the login keychain holding the token and the agent CLI's own
  credentials belong to a logged-in user, with automatic login on a spare Mac; running
  under systemd as a **user** unit with `loginctl enable-linger`, and what the Secret
  Service needs on a machine with no desktop session (see Notes); reading logs; updating
  the binary; and unpairing (from the web UI, then removing the data directory). It says
  plainly that stopping the service cancels in-flight runs (D15), and that a reboot
  therefore leaves them cancelled for a person to retry.
- `CLAUDE.md`, one Gotchas bullet: the headless runner keeps its data in its own directory
  (`com.rimaia.runner`), not the desktop's, and `RIMAIA_DATA_DIR` relocates it as it does
  the app; its token is in the OS keychain; stopping its service stops its queue. The
  commands block does not change, because CI gains no step (Notes).
- Seam contract: a new entry under the next free `D` number, "Task 058's cross-cutting
  choices", in the four-part shape. It records: the data directory and its identifier; the
  lock file; the keychain key for runner tokens; exit status `78` and what maps to it; the
  `https` rule for the server origin; consent only at a terminal, with no `--yes`; the
  go-signal and shutdown order; loopback run routes only, on an ephemeral port by default;
  `Presence::Absent`; and the service settings with their reasons. Add 058's row to
  "How to use this": D10 · D11 · D15 · D19 · D28 · D31 · D33 · D34 and the new entry.

## Out of scope

- **Re-pairing in place, and an `unpair` command.** Unpairing is the web UI's (047, 050;
  057 releases the pins). A store that is already paired refuses `pair`. Pairing the same
  machine again means a fresh data directory, because the old runner's worktrees, held
  leases and outbox belong to a runner id the server may still pin tasks to. Moving them to
  a new id needs a decision, not a flag.
- **A per-repository forge credential from the command line** (ADR-0020's credential, keyed
  per runner by 054). Without one, `git` and `gh` use the machine's ambient credentials, as
  a desktop does for a repository with no credential set. A `credential set` subcommand is
  the first candidate follow-up.
- **Changing runner settings from the command line** after pairing: run environment,
  concurrency, schedules, `max_turns`, `disallowed_tools`, and the strategy ceiling. Task
  042 set the precedent of the sqlite3 CLI for runner values with no control yet
  (ADR-0003), and 061 adds the controls.
- **Run windows and schedules on a headless runner.** Its go signal is its service. A
  person who wants a night-only machine uses the service manager's own timers.
- **Cloning on demand** (ADR-0033 point 2 leaves it out deliberately).
- **Packaging, signing, installers and self-update of the binary.** 063 updates the
  desktop. ADR-0037 point 5 has headless runners report their version, which 053's
  heartbeat already sends and 050's list already shows.
- **A file-backed token store** for a Linux machine with no Secret Service. See Notes.
- **An operator MCP endpoint** on the headless runner (ADR-0035 point 6).
- **A Windows service definition.**
- No board migration, no runner migration, no new `BoardMethod`, no board route, no
  change under `src/`, and no Tauri change. `check-command-wiring.sh` is unaffected.

## Acceptance criteria

**The binary and its dependency.**

- `crates/runner` builds a binary named `rimaia-runner`. `rimaia-runner --version` prints
  `rimaia-runner <CARGO_PKG_VERSION>`, and `--help` lists exactly the subcommands in Goal.
- `clap` is a `[workspace.dependencies]` entry at `4` with feature `derive`, referenced
  from `crates/runner/Cargo.toml` only. `Cargo.lock` gains no package outside `clap` 4's
  own tree (plus whatever Notes' logging question settles). `rimaia-server` still does not
  depend on `rimaia-runner`, and 046's check still passes.
- `the_command_line_parses_every_subcommand`: `Cli::command().debug_assert()` passes, and
  `Cli::try_parse_from` accepts each subcommand in Goal with its arguments and refuses
  `pair` with one argument, `service` with an unknown manager, and `consent` with no
  repository.
- `every_refusal_exits_78`: the mapping function returns `78` for each refusal listed in
  Scope 1, `0` for `HostEnd::Shutdown`, and `1` for an `internal` error.

**The data directory and the lock.**

- `the_headless_data_directory_is_not_the_desktops`: `platform_fallback` returns each
  platform's path from Scope 2 for a given environment, prefers an absolute
  `XDG_DATA_HOME` on Linux and ignores a relative one, and never returns a path containing
  `com.rimaia.app`.
- `a_missing_home_names_the_override`: with no `HOME` (or no `APPDATA` on Windows), the
  error names `RIMAIA_DATA_DIR`.
- `the_override_relocates_the_headless_runner_and_is_refused_the_same_way`: an absolute
  `RIMAIA_DATA_DIR` puts `runner.db`, `runner.lock` and `logs/` under it, and a relative or
  `~` value is refused with `AppPaths::resolve`'s own sentences, before any file is
  created.
- `a_second_run_on_one_data_directory_is_refused`: with the lock held on a real file in a
  `TempDir`, a second acquisition fails with an `invalid` error naming the lock path. Once
  the first is dropped, the lock can be taken again.

**Pairing.** These run against the real `rimaia-server` router served on `127.0.0.1:0`
over a temporary board and a `TestClock`, as 052's `board_port_http.rs` does, with
`MemoryStore` as the keychain.

- `pairing_stores_the_token_in_the_keychain_and_the_identity_in_the_store`: after `pair`
  with a code minted through 047's `create_pairing_code`, `runner_identity` is
  `(1, <runnerId>, <origin>, <clock now>)`; the keychain holds an `rmr_` secret under the
  runner-token key; that token authenticates a runner request as this runner; and the
  captured output does not contain the secret.
- `pairing_refuses_before_spending_the_code_when_the_keychain_is_unavailable`: with
  `MemoryStore::unavailable`, `pair` exits `78` with the store's reason, `runner_identity`
  is empty, and the same code then redeems successfully.
- `an_already_paired_store_refuses_a_second_pairing`: the refusal names the stored runner
  id and server. The code is not spent, and the store and keychain are unchanged.
- `a_rejected_code_writes_nothing`: an unknown code gets 047's one `invalid` sentence, and
  neither the store nor the keychain changes.
- `the_server_must_be_an_https_origin_unless_it_is_loopback`: pure cases.
  `https://rimaia.example/` normalises to `https://rimaia.example`, and
  `http://127.0.0.1:4000` and `http://localhost:4000` are accepted. `http://rimaia.example`,
  `https://rimaia.example/app`, `https://user:pw@rimaia.example` and
  `https://rimaia.example/?x=1` are refused, each with a sentence that says why.
- `the_label_defaults_to_the_hostname`: pure over the command's output. `"mini.local\n"`
  gives `mini.local`, and empty output or a failed spawn is an error that asks for
  `--label`.
- `pairing_asks_for_the_run_environment_only_at_a_terminal`: with a terminal and the answer
  empty, `run_environment` is `strict_local`. With no terminal and no flag it is absent
  and the recommendation is printed. With `--run-environment inherit`, it is `inherit` and
  nothing is asked.

**Checkouts and consent.**

- `checkout_add_maps_a_clone_whose_origin_matches_a_team_repository`: a `TempRepo` with a
  remote whose URL normalises to a team repository's gives one `checkouts` row with that
  repository id, the normalised remote, the default `worktree_root`, `max_concurrency` 1 and
  no consent. The board's `runner_repositories` then lists this runner for it (054).
- `checkout_add_asks_which_when_two_team_repositories_match`: two teams of the owner hold
  the same remote. `add` without `--repository` lists both ids and writes nothing, and with
  `--repository` it maps the named one.
- `checkout_add_refuses_a_clone_whose_origin_matches_nothing`: the message names the
  normalised remote it looked for.
- `checkout_add_saves_while_the_server_is_down`: with the server stopped, the checkout is
  saved and the warning is printed.
- `checkout_remove_refuses_while_worktrees_remain`: the message names the count, and the row
  stays.
- `consent_states_adr_0012s_sentence_and_needs_the_word_yes`: with a terminal, the output up
  to the input is byte-for-byte the text in Scope 5 for the checkout's remote. `"yes\n"`
  and `"  yes  \n"` grant consent. `"y\n"`, `"YES please\n"` and end of input print
  `Nothing changed.` and leave consent false.
- `consent_is_refused_without_a_terminal`: exit `78`, no prompt printed, consent unchanged.
- `withdrawing_consent_asks_nothing`: `--withdraw` reads no input and leaves consent false.
- `consent_names_only_a_mapped_checkout`: an unknown repository is `NotFound`.
- `the_terminal_and_the_desktop_state_the_same_grant`: a test in `crates/core` reads
  `src/views/settings/RepositoriesSection.tsx` with `include_str!` and asserts that it
  contains `repo::UNATTENDED_RUNS_GRANT` verbatim, between double quotes. Changing either
  wording then fails this test, not a reviewer's memory.

**`run`.** These are `#![cfg(unix)]`, like `tests/scheduler.rs`, because `FakeCli` is a
shell script. Each one pairs against the real server router, maps a real `TempRepo` with a
bare remote, uses `FakeCli` replaying fixture streams and a `TestClock`, and never sleeps.
The host is stopped through `shutdown()`, not a real signal.

- `a_headless_runner_claims_runs_pushes_and_reports_a_task`: a ready task assigned to the
  runner's owner, in a consented checkout, is claimed, run and finished. The branch is on
  the bare remote at the recorded `head_sha` (057). The task is `in_review`, the run's
  `runner_id` is this runner, and its transcript is complete on the server (056).
- `run_turns_the_go_signal_on_and_shutdown_turns_it_off`: `queue_state` is `running` after
  `start`, even when the store said `paused`, and `paused` after `shutdown`.
- `shutdown_cancels_in_flight_runs_through_the_cancel_path`: with a `FakeCli` that hangs,
  `shutdown` sends its process group the SIGTERM the cancel path sends. The run is
  recorded exactly as a desktop quit records it, the lease is released, and nothing is left
  in flight.
- `a_report_the_server_did_not_take_is_sent_at_the_next_start`: the server is stopped
  before `shutdown`, the cancelled run's `finish_run` stays in the outbox, and a new host
  started against a restarted server delivers it.
- `an_unconsented_checkout_is_never_claimed_for`: a ready task in a mapped checkout without
  consent stays `idle`, and no agent process is started.
- `run_now_from_the_browser_runs_unattended_on_a_headless_runner`: a Run now for this runner
  (052), requested over a browser session, spawns with the unattended permission mode in
  `FakeCli`'s recorded argv, and never with `acceptEdits`.
- `a_revoked_token_ends_the_host_as_unauthenticated`: revoking the runner's token through
  047's `revoke_api_token` resolves `ended()` to `Unauthenticated`. After `shutdown`, the
  in-flight run was cancelled, `queue_state` is `paused`, and the exit status is `78`.
- `an_unsupported_protocol_ends_the_host_as_upgrade_required`: a server built to refuse the
  runner's protocol version (by whatever seam 046's tests already use for this) resolves
  `ended()` to `UpgradeRequired` with the minimum version in its message, and nothing is
  claimed.
- `the_headless_runner_serves_no_operator_endpoint`: on the runner's loopback port, `POST
  /mcp` is `404`, and a live run's `/mcp/run/{token}` answers `tools/list` with the
  run-scoped surface (055).
- `run_refuses_an_unpaired_store` and `run_refuses_when_the_keychain_has_no_token`: each is
  exit `78` with Scope 6's sentences, and neither makes a request.

**`status` and the service definitions.**

- `status_reads_only_the_local_store`: with the server stopped, `status` over a store with
  two checkouts, one held lease, a usage-limit pause and one outbox row prints exactly the
  Scope 7 shape for those values. The `Service` line is `running` while another handle holds
  the lock and `stopped` otherwise. An unpaired store prints `Not paired.` and exits `78`.
- `the_systemd_unit_is_exactly_this` and `the_launchd_plist_is_exactly_this`: for a fixed
  binary path, `PATH` and data directory, each definition is byte-for-byte the expected
  text, containing every setting in Scope 9.
- `a_binary_path_with_a_space_is_quoted_in_the_unit_and_escaped_in_the_plist`, and a `PATH`
  containing `&` is XML-escaped in the plist.
- `a_data_directory_override_is_carried_into_both_definitions`, and is absent from both
  when it was not set.

**Everything else.**

- `docs/headless-runner.md` exists with every topic in Scope 10. `CLAUDE.md` carries the
  one Gotchas bullet. The seam contract carries the new entry and the "How to use this"
  row.
- No file is added to `src-tauri/migrations/` or `crates/runner/migrations/`. If a query
  was added (for example, counting the outbox for `status`), both offline caches are
  regenerated with D33 point 3's recipe and committed.
- Every CI command passes with `SQLX_OFFLINE=true` exported: `npm run typecheck`,
  `npm run test`, `npm run build`, `cargo test -p rimaia-core`,
  `cargo test -p rimaia-runner`, `cargo fmt --all --check`,
  `cargo clippy -p rimaia-core --all-targets -- -D warnings`,
  `cargo clippy -p rimaia-runner --all-targets -- -D warnings`,
  `cargo check --workspace --all-targets`, `./scripts/check-command-wiring.sh`, and
  whatever server steps 046 added.
- **Checked by hand, and listed in the PR body:** against a local `rimaia-server`, pair a
  headless runner on a Mac under launchd and on a Linux machine under a systemd user unit.
  On each: map a clone, give consent, let one assigned task run to `in_review`, stop the
  service mid-run and confirm the run reads as cancelled, and reboot and confirm the
  runner comes back and claims again. On the Mac, replace the binary with a rebuilt one
  and confirm that the keychain does not show an access prompt nobody can answer (Notes).

## Notes

**Read first.** ADR-0027 points 4 to 6 (the three modes, the port, the crate layout).
ADR-0030 points 3 and 5 (one token shape; pairing). ADR-0031 points 5 to 7 (per-runner
recovery, queue control per runner, Run now and presence). ADR-0012 in full, for the
consent wording. Also ADR-0032 point 4 and point 6's last bullet, ADR-0033 point 2,
ADR-0035 points 5 and 6, and ADR-0037 points 4 and 5. Seam entries: **D15** and its
amendment (what quitting does), **D31** points 8, 10, 11 and 13 (where the binary builds its
port, the HTTP adapter, the one reaction to `Conflict`, the suite's server harness),
**D33**'s line for 058 (the binary uses the runner's cache and adds no third one),
**D34**'s `clap` row, D28's runner set and the note that the token lives in the keychain,
D10, D11 and D19. Read D4 and D6 as prohibitions.

**A dependency this task needs and D34 does not list. Ask before writing `main.rs`'s
logging.** A headless runner nobody watches is diagnosed from its logs. The shell gets
them from `tracing-subscriber` (`env-filter`) and `tracing-appender`, declared in
`src-tauri/Cargo.toml` only. `rimaia-runner` needs the same two. Both are already in
`Cargo.lock` at the versions the shell uses, so this is the no-new-tree argument D6 made for
`base64` and D34 made for `sha2`: promote the two to `[workspace.dependencies]` at the
shell's versions, and reference them from `src-tauri` and `crates/runner`. It is still a
D6 question, and D34 records the same kind of gap for 048. The rest of this task does not
depend on the answer, so build it first and keep the subscriber setup as the last commit.
Hand-rolling a subscriber is not the fallback. `eprintln!` loses every `tracing` line the
loop and `rimaia-core` emit.

**A gap for Linux, recorded rather than papered over.** `keyring`'s Secret Service backend
needs a D-Bus session and an unlocked collection. A Linux server with no desktop session,
running a lingering systemd user unit, usually has neither. `KeyringStore::status` already
reports that as "no keychain on this machine", and Scope 4 makes `pair` refuse on it before
spending the code. So the failure is loud, and the docs say what a machine needs (a
keyring daemon started and unlocked in the user's session). The likely real answer, a
token file readable only by its owner, as an explicit opt-in, weakens ADR-0030's "stores
in the OS keychain" and so needs an amendment to that ADR. It is not an implementation
choice. If the manual Linux check shows the keyring route is impractical, say so in the PR
and propose the amendment. Do not add a file store here.

**A macOS behaviour to confirm by hand.** Keychain items carry an access list naming the
program that created them. Replacing an unsigned binary can make macOS ask whether the new
one may read the item, and a prompt in a LaunchAgent at 2am is a prompt nobody answers.
The manual check in Acceptance criteria covers it. `rimaia-runner status` does not help
here, because it never reads the secret. If the check shows a prompt, the documented step
after an update is to run `rimaia-runner run` once in a terminal, answer "Always Allow",
stop it, and restart the service. Write the instruction the check proves, not this guess.

**What the earlier tasks provide, and what this task assumes.**

- 040: `RunnerStore::open`, `AppPaths::runner_db_file()` and `runner_identity`, whose
  `server_url` has been NULL until now. This task is its first writer on a headless runner.
- 041: `MachineStore`, `MachineContext`, the checkout contract, and the accessors
  `set_queue_state`, `set_run_environment` and the consent setter.
- 042: `rimaia_runner::queue::build` and its shutdown, and `ClaimTarget::Next` listing only
  consented checkouts.
- 043 and 053: `held_leases`, the per-runner reconcile, `Conflict`, the heartbeat, the long
  poll, and `app_version` on the heartbeat.
- 046: the `Rimaia-Protocol` header and `UpgradeRequired`. 047: `POST /api/v1/auth/pair`
  answering `{ runnerId, token }`, `create_pairing_code` and `revoke_api_token`.
- 048: `LocalEvents`. 052: `HttpBoard`, and a Run now for a specific runner.
- 054: the checkout mapping function, `normalized_remote`, `report_runner`, and a lookup of
  team repositories by normalised remote that a runner token may call. **If 054 shipped no
  such lookup, that is a missing D31 method. Stop and ask for a D31 amendment. Do not
  widen a board route to `rmr_` tokens:** 047's
  `only_browser_and_desktop_may_call_a_board_route` exists to prevent exactly that.
- 055: the run-scoped proxy. 056: the outbox and its sender. 057: the push postcondition.

Where one of these landed under a different name, use that name. Where one is missing
entirely, stop: this task composes, and it does not build a missing layer on the way.

**What 059 expects.** `rimaia_runner::host` with `HostConfig`, `Presence` and `HostEnd`,
which it starts in connected mode with the desktop's presence, and with `Unauthenticated`
turned into "sign in again" rather than an exit. The keychain key convention for tokens,
which it uses for the `rmd_` token. The `runner.lock` convention: the connected desktop
should take the same lock on its own data directory, so that `RIMAIA_DATA_DIR` pointed at
both cannot put two loops on one `runner.db`. 061 reads this task's seam entry for the
settings that still have no control.

**Files to start from.**

- `crates/runner/Cargo.toml`, `crates/runner/src/lib.rs`, `crates/runner/src/queue/` (042),
  `crates/runner/src/board/http.rs` (052), and `crates/runner/tests/board_port_http.rs`
  (052) for the server-in-process test harness.
- `crates/core/src/paths.rs` (`AppPaths::resolve`, `DATA_DIR_ENV`, `logs_dir`).
- `crates/core/src/credentials/mod.rs` (`CredentialStore`, `KeyringStore`, `StoreStatus`,
  `KEYCHAIN_SERVICE`, `Secret`) and `crates/core/src/testing/credentials.rs`
  (`MemoryStore`, `MemoryStore::unavailable`).
- `crates/core/src/repo/mod.rs` (`set_allow_unattended_runs`'s doc, where the grant
  sentence lives today) and `src/views/settings/RepositoriesSection.tsx`
  (`UNATTENDED_RUNS_GRANT`).
- `src-tauri/src/lib.rs` (`shut_down` and the exit handling around it),
  `src-tauri/src/state.rs` (`cancel_everything`) and `src-tauri/src/logging.rs`.
- `crates/core/src/runner/process.rs` (`DEFAULT_GRACE_PERIOD`, the cancel path) and
  `crates/core/src/runner/provider/mod.rs` (`ProviderId::as_str`).
- `crates/core/src/testing/cli.rs` (`FakeCli`), `crates/core/src/testing/repo.rs`
  (`TempRepo::with_remote`) and `crates/core/src/testing/clock.rs`.
- `crates/core/src/identity/pairing.rs` (047) and `crates/server/src/runner_api.rs` (052).

**No migration.** This task adds no file to either migration set (D4 and D28's amendment:
"that is the whole list"). `runner_identity`, `checkouts`, `held_leases` and `outbox`
already hold everything `pair`, `checkout`, `consent` and `status` read or write.

**CI.** `cargo test -p rimaia-runner` runs the new tests on all three operating systems.
The `run` tests are Unix-only, like the loop's. `cargo check --workspace --all-targets`
compiles the binary. No step is added, so `ci.yml` and CLAUDE.md's commands block stay
identical.

**Size.** M, at the top of it. Roughly: the binary and CLI parsing 200 lines; paths, lock
and exit status 200; pair 250; checkout and consent 300; the host 350; status 150; the
service definitions 200; docs 250; and about 1,200 lines of tests. That is about 3,000
lines, inside one session. If it runs over, cut in this order, each into a follow-up with
the next free number placed directly after 058 in `tasks/README.md`: first `service` and
its tests (the docs can show a hand-written unit instead), then `status`. `pair`,
`checkout`, `consent` and `run` are the task. Without any one of them a headless runner
cannot do a night's work.
