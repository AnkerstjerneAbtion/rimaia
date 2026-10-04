---
id: "058"
title: The headless runner
milestone: v0.5
status: ready
depends_on: ["055", "056", "057"]
adrs: ["0027", "0030", "0031"]
size: L
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
`rimaia-runner` library: pairing is 047's, the checkout mapping is 054's, consent is 066's
and 045's, the loop and its doctor gate are 042's, leases are 043's and 053's, the
run-scoped proxy is 055's, the outbox is 056's and the push postcondition is 057's. This
task composes them into one process, adds the command line, and decides the handful of
things a process with no window has to decide on its own: where its data lives, where its
token lives, what a signal means, what its exit status says, and how a service manager
should run it.

**Stopping the service stops that runner's queue** (seam-contract D15, per runner by
ADR-0031 point 6). "A headless runner's go signal is its service running": `run` turns the
switch on through 042's doctor-gated `QueueHandle::start`, and a clean shutdown turns it off
through `QueueHandle::stop` and cancels its in-flight runs through the same path a desktop
quit uses.

## Why now

Tasks 052 to 057 built every part a runner needs over HTTP: the protocol and the HTTP board
adapter (052), the long poll, heartbeat and sleep recovery (053), checkouts by remote (054),
the run-scoped proxy (055), transcripts and the outbox (056), and the push postcondition
(057). So far only tests host them. The headless binary is the first production host that
has no board file and no window, which makes it the cleanest proof that the runner protocol
stands on its own. If a headless runner can pair, map a clone, take consent, claim, run,
push and report, then everything the connected desktop adds in 059 is interface.

It comes before 059 for a second reason. Both need the same composition: store, keychain
token, HTTP board, outbox, reconcile, proxy, heartbeat and loop, started in order and
stopped in reverse, over one `RunnerConfig`. Written here first, that composition has no
Tauri in it, and 059 reuses it instead of growing a second one inside
`src-tauri/src/lib.rs`. The plan's end-of-M4 check needs "one headless runner and one
connected desktop" against a local server, and this task provides the first of them.

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
- **Exit status, by D8's code, with no new variant.** `0` for `HostEnd::Shutdown` (point 6).
  `2` for a usage error, which is `clap`'s own. `78` (`EX_CONFIG` in `sysexits.h`) for
  `invalid`, `not_found`, `unauthenticated` and `upgrade_required`: a refusal a person has
  to fix, and every refusal this task names is raised as one of them. `1` for `internal`,
  `io`, `database` and `conflict`. One function maps an `Error` or a `HostEnd` to the
  status, and the service definitions (point 9) depend on it. `KeyringStore` reports its
  failures as `internal`, so the `headless` call sites turn `StoreStatus::Unavailable`, and
  an `Err` from reading the token, into `Error::invalid` with the keychain's reason. On this
  path it always means a keychain the person has to unlock.

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
until the process exits. A second `run` on the same directory is refused as `invalid`,
naming the lock file. The OS releases the lock when the process dies, so a crash leaves
nothing to clean up. `status` opens the same file and tries the lock without keeping it,
which is how it tells whether a `run` is active. The other subcommands do not take the
lock: they write `runner.db` through the store, which is safe beside a running loop because
the store is SQLite in WAL mode (ADR-0003) and the loop re-reads checkouts and consent on
every claim pass (042).

**4. Pairing, and the token in the keychain.** `headless::pair(PairRequest, &dyn
CredentialStore, &RunnerStore, &dyn Clock, terminal) -> Result<Paired>`.

- **The server argument is an origin.** It is parsed with `reqwest::Url`, already a
  dependency, and normalised to `scheme://host[:port]` with no trailing slash. A path, a
  query, a fragment or credentials in the URL are refused. `https` is required, except for
  a loopback host (`127.0.0.1`, `::1`, `localhost`), where `http` is accepted for a local
  server and for tests. Everything the runner sends carries its token, and a token sent in
  clear to a remote host is a token given away. The web shell prints the exact line to type,
  `rimaia-runner pair <window.location.origin> <code>` (task 050), so the normal input is
  already an origin.
- **The label** is `--label`, or else `headless::default_label()`: the machine's hostname,
  read by running `hostname` as an argument vector, never through `sh -c`. The command
  exists on macOS, Linux and Windows. Its output is trimmed. An empty output or a failed
  spawn is an `invalid` error asking for `--label`. ADR-0030 point 5 makes the hostname the
  default, and 059 calls the same function.
- **The order is chosen so that a refusal never spends the code.** A code is useless once
  redeemed, whether or not the redemption that used it succeeded (047 Scope 7).
  1. Refuse if `runner_identity` already exists, naming the runner id and server it holds.
  2. Refuse if the keychain is unavailable. No runner id exists yet, so the probe is
     `status(&CredentialKey::RunnerToken { runner_id: "pending" })`. That account is never
     written, because no D10 id is `pending`, so a working keychain answers `Absent` and a
     broken one `Unavailable`, with its reason.
  3. `POST /api/v1/auth/pair` with `{ code, label, provider }` and 046's `Rimaia-Protocol`
     header. The answer is `{ runnerId, token }` (047).
  4. Save the token through `credentials::save_runner_token`.
  5. Write `runner_identity (1, runnerId, origin, now)`.

  If step 4 fails, nothing is written to `runner.db`, and the error names the runner id,
  which the person unpairs in the web UI. Step 5 comes last, so a store that says it is
  paired always has a token behind it, unless someone removed the token by hand (point 6
  covers that).
- **Where the token lives.** In the OS keychain under `KEYCHAIN_SERVICE`, never in
  `runner.db`, stdout, a log line or an error message (D25; D28: "The runner token is in the
  keychain, never here"). The key is a new variant on 054's `CredentialKey` enum,
  `RunnerToken { runner_id }`, whose account is exactly `runner-token:<runner_id>`. It
  contains no `/`, so it cannot equal 054's `<repository_id>/<runner_id>`, and it is built
  in `CredentialKey::account()` like every other account. 059 adds `DesktopToken` and
  `LoopbackMcpToken` beside it. Every host reads and writes it through two accessors,
  `credentials::runner_token(store, runner_id) -> Result<Option<Secret>>` and
  `credentials::save_runner_token(store, runner_id, &Secret)`, and 059 calls the same two.
  The token is held as `Secret`, whose `Debug` already redacts it.
- **After pairing**, `pair` prints the runner id and the server, and the next command to
  type (`rimaia-runner checkout add <path to a clone>`). It then asks two questions, each
  under one rule: a flag sets the value without asking; at a terminal with no flag, `pair`
  asks; with no terminal and no flag it writes nothing, so the default stands, and it prints
  what it would have asked.
  - **The run environment**, because ADR-0032 point 6 says "pairing a runner recommends"
    `strict_local` for team use. `--run-environment <inherit|strict-local>`. At a terminal
    an empty answer is `strict_local`, the recommendation. The value is written through
    041's `db::settings::set_run_environment(&MachineContext, ..)`. Nobody who does not
    answer loses the `inherit` default (CLAUDE.md's Gotchas).
  - **Transcript upload**, because ADR-0036 point 5 says pairing shows this setting and what
    it means. `pair` prints `transcripts::UPLOAD_DISCLOSURE` (056) verbatim, terminal or
    not, then applies the rule with `--transcripts <full|summaries-only>`. At a terminal an
    empty answer is `full`, the default. The value is written through 056's
    `upload_transcripts` accessor.

**5. Checkouts and consent at a terminal.**

- **`checkout add <path> [--repository <id>]`** needs the server, because a mapping is
  verified against the board's repository (ADR-0033 point 2). It reads the board only
  through the port, because 047's `only_browser_and_desktop_may_call_a_board_route` keeps
  an `rmr_` token off every board route:
  1. without `--repository`, it normalises the clone's `origin` with 054's
     `normalized_remote` and asks `find_repositories(RepositoryLookup::ByRemote(..))` (D31's
     2026-10-04 amendment). More than one row (the same remote registered in two of the
     owner's teams) lists each team name, repository name and id, and requires
     `--repository`. No row says which remote it looked for, and that the repository must
     first be registered in one of the owner's teams. No `origin` is 054's sentence;
  2. it calls 054's `machine::checkouts::map(machine, &board, repository_id, path)`, the
     function the desktop's `map_repository_checkout` calls. Every rule is there: 003's
     validations, the remote match, the remote-less rule, the shared-path rule, a re-map
     keeping the machine's choices, and the report after the write. A new checkout gets the
     default `worktree_root` that registration uses today, `max_concurrency` 1 and no
     consent.

  An unreachable server is an `io` error naming the origin, and nothing is written: a
  mapping cannot be verified against a repository nobody can read.
- **`checkout list`** prints one line per checkout: the normalised remote, the path, and
  `unattended: allowed` or `unattended: not allowed`.
- **`checkout remove <repository>`** accepts a repository id or a normalised remote, and
  calls 054's `unmap_repository_checkout` core function. That function refuses while
  worktrees remain, with its own sentence, deletes the checkout's keychain item, and
  reports. There is no `--force`. Removing worktrees has its own rules (D20), and this task
  does not give them a second door.
- **`consent <repository>`** gives this machine's unattended consent for one mapped
  checkout. It is the only way to give it on a headless runner, and it is given only at a
  terminal:
  - with no terminal on stdin (`std::io::IsTerminal`), it is refused as `invalid` with
    nothing printed but the refusal. There is no `--yes`. A consent that a script can give
    is not ADR-0012's "explicit, informed" opt-in, and ADR-0032 point 4 says the dialog's
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
  - the write goes through the function 066 gave `set_repository_unattended_runs`, so the
    change event and the store rule are the desktop's.
- **Reports from a short-lived command.** `checkout add`, `checkout remove` and `consent`
  build the same port as `run` (point 6, step 3) for 054's functions to report through. The
  report comes from those functions, never from a second call in the binary. When it cannot
  be sent, the local write stands, and the command warns that the board learns of it when
  `run` next starts. 054's dirty-report resend rides a heartbeat, and a command that exits
  has none, so `run`'s startup report is the resend.

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
}
pub enum HostEnd { Shutdown, Unauthenticated(String), UpgradeRequired(String) }

impl RunnerHost {
    pub async fn start(config: HostConfig) -> Result<RunnerHost>;
    pub async fn ended(&self) -> HostEnd;   // resolves when the server refuses the runner
    pub async fn shutdown(self) -> Result<()>;
}
```

The names are indicative. What is fixed is the order of `start`, the reverse order of
`shutdown`, the `RunnerConfig` below, and what a `HostEnd` means.

- **The `RunnerConfig` `start` builds**, so that 059 inherits it by calling the same
  function:
  - `program` and the provider, from `HostConfig`;
  - `branch_postcondition: BranchPostcondition::OnOrigin` (057). The default, `Recorded`,
    checks and pushes nothing;
  - `host_secrets`: a `Redactor` over the `rmr_` token (056), so a transcript, stderr log
    or tail that contains it is redacted. 059 merges its own tokens in;
  - every other field from the runner settings, as solo builds them.

  The loop is built with `claim_wait = CLAIM_WAIT_MAX` (053). That is also what makes it
  claim at zero free capacity, which is how a busy runner hears a browser's Run now (052).
  It sends 057's `worktrees` list on every `claim(Next)`, and this task adds no second
  builder of that list.
- **No board context for the loop.** 042 point 5 leaves `SoloBoard`'s four reads for 058 to
  replace. Make them a small trait in `crates/runner/src/queue/`, implemented by `SoloBoard`
  and by a runner-only type over `MachineContext`: no `next_deadline` (053's long poll wakes
  board-side at `earliest_due`), no plan half in `status_with_plan`, the doctor over
  `runner.db`'s checkouts, and no fire-time preflight log (point 8). The loop does not branch
  on mode.
- **`start`, in this order, each step failing like its neighbours with a step name and the
  file or URL involved (D11's shape):**
  1. read `runner_identity`. Refuse if it is absent ("This runner is not paired. Run
     `rimaia-runner pair <server> <code>` first.");
  2. read the token through `credentials::runner_token`. Refuse if it is absent, naming the
     runner id and saying to pair again;
  3. build 052's `HttpBoard` with the origin, the token and the provider, and wrap it in
     056's `OutboxBoard`. The wrapped port is the only one the reconcile, the loop, the
     heartbeat and the proxy receive, so every lease-bound report goes through the outbox.
     Build one `LocalEvents` (048) for every machine-state writer;
  4. reconcile the leases this runner held, from `held_leases`, through 043's per-runner
     reconcile and 053's pin (ADR-0031 point 5). It never touches another runner's runs. An
     unreachable server here is an `io` error, so `run` exits `1` having started nothing,
     and the service manager starts it again in 60 seconds. That is how a runner that boots
     before its network starts once the network is up. Only the server's answer fences
     (053), so there is nothing to reconcile against without it;
  5. run 054's startup verification of every checkout, then the doctor, then send
     `report_runner` (054) with the checkout set, the consent per repository and the doctor
     result. A failed send marks the report dirty, and 053's heartbeat tick resends it;
  6. start 056's outbox sender, then the proxy, then 053's heartbeat;
  7. build 042's loop with the port, the store, one `InFlight`, the config and the
     runner-only reads, and turn the switch on with `QueueHandle::start`. That is D22 point
     1's one gate, which runs the doctor again: a blocking report refuses the start and
     writes no `queue_state`. The host then runs `shutdown`, and `run` exits `78` printing
     `blocking_summary()`. Step 5 has already told the board, so the web UI's doctor view
     shows why, and the service manager tries again once a minute.
- **When the server refuses the runner itself.** There is one detection point. `HttpBoard`
  publishes the first `unauthenticated` or `upgrade_required` answer to any request on a
  `tokio::sync::watch` channel, and `ended()` awaits it. Every runner request passes through
  the adapter: the loop's claim, the heartbeat, the outbox sender, `report_runner`,
  `find_repositories` and the proxy's `run_tool`. 053's hand-off of these two codes from
  the loop to the host is this signal, not a second path, and no caller retries either
  code. An `Unauthenticated` answer means the token was revoked or the runner was unpaired
  in the web UI. An `UpgradeRequired` answer (ADR-0037 point 4) carries the minimum version.
  Either way the binary runs `shutdown` and exits `78`, printing 047's or 046's sentence. A
  too-old runner stops taking work rather than taking it and misreporting it, which is
  ADR-0037's rule. A `Conflict` is not a `HostEnd`. It fences one lease, and 053's single
  reaction handles it (D31 point 11).
- **The run-scoped proxy, and nothing else, on loopback.** The host calls 055's
  `run_proxy::bind(handles, board, on_fenced, 0)` unchanged: `/mcp/run/{token}` (ADR-0035
  point 5) on `127.0.0.1` at an OS-chosen port, because only the runner's own children need
  to find it and they are told the URL. `mcp_port` is the operator listener's setting (041),
  and a headless runner has no operator listener. It serves **no operator endpoint**.
  Reconfiguring a machine over MCP is a desktop's loopback endpoint (ADR-0035 point 6), and
  a headless runner's operator surface is the hosted `/mcp` (060). The doctor's `McpPort`
  check therefore does not apply: `doctor::Environment` says whether an operator endpoint
  is expected, and when it is not, the report leaves the row out, rather than warning about
  "Settings → MCP" on a machine with no Settings.
- **Run now from a browser runs unattended.** The runner obeys `Claim::trigger`. 052's
  relay claims a browser's Run now as `RunTrigger::Queued`, which is ADR-0012's unattended
  posture behind consent (ADR-0031 point 7, decided by 043's `authorize_start` at the
  door). The host has no presence setting of its own.
- **`shutdown`, which is what a signal does.** On `SIGTERM` or `SIGINT` on Unix, and on
  Ctrl-C or Ctrl-Break on Windows (`tokio::signal`, already in the workspace's `tokio`
  features):
  1. stop the loop from claiming more, as `queue.shutdown()` does on the desktop;
  2. cancel every in-flight run through the normal cancel path (SIGTERM to the process
     group, then SIGKILL after `DEFAULT_GRACE_PERIOD`), and call `QueueHandle::stop`, which
     writes `paused` and clears `active_run_window` (D15's amendment). This is
     `AppState::cancel_everything` (`src-tauri/src/state.rs`) without the Tauri state
     around it. A cancelled run is reported as the desktop's quit reports it, through
     `finish_run` and the outbox;
  3. wait, bounded as `src-tauri/src/lib.rs`'s `shut_down` bounds it, until nothing is in
     flight. The wait awaits the runs themselves, not a polling loop with a sleep;
  4. stop the heartbeat, the proxy and the outbox sender, in that order. Reports still in
     the outbox stay in `runner.db` and are sent at the next start (056). Shutdown does not
     wait for the server;
  5. release the lock by exiting `0`.
  A second signal during shutdown exits at once with `1`, after the children have already
  been signalled, as the desktop does when its deadline passes.
- **Logs.** The same shape as `src-tauri/src/logging.rs`: stderr and a daily file,
  `<data>/logs/rimaia-runner.log`, with `RIMAIA_LOG` as the filter and a default of
  `rimaia_runner=debug,rimaia_core=debug,warn`. The two crates are D34's rows for this task
  (Notes). `run` logs one line at start naming the version, the runner id, the
  server origin and the data directory, and one at exit naming why.

**7. `status`.** `headless::status(&RunnerStore, &dyn CredentialStore, lock_path, out)`
reads the local store, the keychain's `status` for the runner token and the lock, and
makes no request, so it works while the server is down, which is exactly when someone runs
it. `KeyringStore`'s status is `get` matched to a variant: it reads the secret and drops it,
so on macOS it can raise the same access prompt `run` would (Notes). It prints, in this
order and in this shape:

```
Runner       8d1e2c1a-0f4b-4a5e-9d0c-3b7f2a9e6c11
Server       https://rimaia.example
Token        in the keychain
Service      running
Queue        running
Usage limit  not paused
Environment  strict_local
Transcripts  summaries_only
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
  installed elsewhere. Without it the doctor's CLI check fails, `QueueHandle::start`
  refuses (D22), and the runner exits `78` once a minute all night.
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
  refused for its version or its doctor starts working by itself once the cause is fixed. A
  clean stop exits `0`, and neither manager restarts it.
- **Windows.** `service` has no Windows form. It says so as `invalid`. The binary builds,
  tests and runs in the foreground on Windows, as ADR-0002 requires. A Windows service
  wrapper is out of scope.

**10. Documentation.**

- `docs/headless-runner.md`, new. It covers: installing the binary (built with
  `cargo build --release -p rimaia-runner` until a release pipeline exists); the agent CLI
  signed in as the same user the service runs as; pairing from the web UI's line, and the
  two questions it asks; `checkout add` and `consent`; running under launchd as a
  **LaunchAgent**, not a LaunchDaemon, because both the login keychain holding the token and
  the agent CLI's own credentials belong to a logged-in user, with automatic login on a
  spare Mac; running under systemd as a **user** unit with `loginctl enable-linger`, and
  what the Secret Service needs on a machine with no desktop session (see Notes); reading
  logs; updating the binary; and unpairing (from the web UI, then removing the data
  directory). It says plainly that stopping the service cancels in-flight runs (D15), and
  that a reboot therefore leaves them cancelled for a person to retry.
- `CLAUDE.md`, one Gotchas bullet: the headless runner keeps its data in its own directory
  (`com.rimaia.runner`), not the desktop's, and `RIMAIA_DATA_DIR` relocates it as it does
  the app; its token is in the OS keychain; stopping its service stops its queue. The
  commands block does not change, because CI gains no step (Notes).
- Seam contract: a new entry under the next free `D` number, "Task 058's cross-cutting
  choices", in the four-part shape. It records: the data directory and its identifier; the
  lock file; `CredentialKey::RunnerToken` and its account; the exit status by error code;
  the `https` rule for the server origin; the flag-terminal-nothing rule for pairing's two
  questions; consent only at a terminal, with no `--yes`; the host's `RunnerConfig`; the
  start order, the doctor gate's headless answer and exit `1` for an unreachable server at
  start; the refusal channel; loopback run routes only, on an OS-chosen port, with no
  `McpPort` row; and the service settings with their reasons. Add 058's row to "How to use
  this": D3 · D8 · D9 · D10 · D11 · D15 · D19 · D20 · D22 · D25 · D28 · D30 · D31 · D32 ·
  D33 · D34 and the new entry.

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
  transcript upload, concurrency, schedules, `max_turns`, `disallowed_tools`, and the
  strategy ceiling. Task 042 set the precedent of the sqlite3 CLI for runner values with no
  control (ADR-0003), and a headless runner keeps it. 069 adds controls only on a machine
  with a window: the board holds no runner settings (050), so a browser control would need
  a D28 amendment first.
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

**The binary and its dependencies.**

- `crates/runner` builds a binary named `rimaia-runner`. `rimaia-runner --version` prints
  `rimaia-runner <CARGO_PKG_VERSION>`, and `--help` lists exactly the subcommands in Goal.
- `clap` is a `[workspace.dependencies]` entry at `4` with feature `derive`, referenced
  from `crates/runner/Cargo.toml` only. `tracing-subscriber` and `tracing-appender` are
  workspace lines, as D34's rows say, referenced from `src-tauri` and `crates/runner`, and
  `cargo tree -d` shows one of each. `Cargo.lock` gains no
  package outside `clap` 4's own tree. `rimaia-server` still does not depend on
  `rimaia-runner`, and 046's check still passes.
- `the_command_line_parses_every_subcommand`: `Cli::command().debug_assert()` passes, and
  `Cli::try_parse_from` accepts each subcommand in Goal with its arguments and refuses
  `pair` with one argument, `service` with an unknown manager, and `consent` with no
  repository.
- `every_refusal_exits_78`: one row per refusal this task names (not paired, already
  paired, token missing, keychain unavailable on `pair` and on `run`, data directory
  refused, lock held, rejected pairing code, refused origin, no label, no matching
  repository, ambiguous repository without `--repository`, unknown repository, consent
  without a terminal, `checkout remove` with worktrees, a blocking doctor report, revoked
  token, `UpgradeRequired`, `service` on Windows), each raised by the function that raises
  it, maps to `78`. `HostEnd::Shutdown` maps to `0`, and `internal` and `io` map to `1`.

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

**Pairing and the token.** These run against the real `rimaia-server` router served on
`127.0.0.1:0` over a temporary board and a `TestClock`, as 052's `board_port_http.rs` does,
with `MemoryStore` as the keychain.

- `the_runner_token_account_is_runner_token_and_the_runner_id`: in `crates/core`,
  `CredentialKey::RunnerToken { runner_id }.account()` equals `runner-token:<runner_id>`
  and contains no `/`.
- `pairing_stores_the_token_in_the_keychain_and_the_identity_in_the_store`: after `pair`
  with a code minted through 047's `create_pairing_code`, `runner_identity` is
  `(1, <runnerId>, <origin>, <clock now>)`; `credentials::runner_token` returns an `rmr_`
  secret; that token authenticates a runner request as this runner; and the captured output
  does not contain the secret.
- `pairing_refuses_before_spending_the_code_when_the_keychain_is_unavailable`: with
  `MemoryStore::unavailable`, `pair` fails as `invalid` with the store's reason,
  `runner_identity` is empty, and the same code then redeems successfully.
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
- `pairing_asks_its_two_questions_only_at_a_terminal`: with a terminal and both answers
  empty, `run_environment` is `strict_local` and `upload_transcripts` is `full`. With no
  terminal and no flags both keys are absent and both recommendations are printed. With
  `--run-environment inherit --transcripts summaries-only`, those are written and nothing
  is asked.
- `pairing_prints_the_upload_disclosure`: with and without a terminal, the output contains
  `transcripts::UPLOAD_DISCLOSURE` byte-for-byte.

**Checkouts and consent.**

- `checkout_add_maps_a_clone_whose_origin_matches_a_team_repository`: a `TempRepo` with a
  remote whose URL normalises to a team repository's gives one `checkouts` row with that
  repository id, the normalised remote, the default `worktree_root`, `max_concurrency` 1 and
  no consent. The board's `runner_repositories` then lists this runner for it (054).
- `checkout_add_asks_which_when_two_team_repositories_match`: two teams of the owner hold
  the same remote, and a third team the owner is not in holds it too. `add` without
  `--repository` lists exactly the owner's two and writes nothing, and with
  `--repository` it maps the named one.
- `checkout_add_refuses_a_clone_whose_origin_matches_nothing`: the message names the
  normalised remote it looked for.
- `checkout_add_writes_nothing_while_the_server_is_down`: an `io` error naming the origin,
  and no `checkouts` row.
- `checkout_remove_goes_through_054s_unmap`: with worktrees remaining it is refused with
  054's sentence and the row stays; without, the row and the checkout's keychain item are
  gone.
- `consent_states_adr_0012s_sentence_and_needs_the_word_yes`: with a terminal, the output up
  to the input is byte-for-byte the text in Scope 5 for the checkout's remote. `"yes\n"`
  and `"  yes  \n"` grant consent. `"y\n"`, `"YES please\n"` and end of input print
  `Nothing changed.` and leave consent false.
- `consent_is_refused_without_a_terminal`: `invalid`, no prompt printed, consent unchanged.
- `withdrawing_consent_asks_nothing`: `--withdraw` reads no input and leaves consent false.
- `consent_names_only_a_mapped_checkout`: an unknown repository is `NotFound`.
- `consent_is_saved_while_the_server_is_down`: the consent is written, the warning is
  printed, and a host started against a restarted server reports it.
- `the_terminal_and_the_desktop_state_the_same_grant`: a test in `crates/core` reads
  `src/views/settings/RepositoriesSection.tsx` with `include_str!` and asserts that it
  contains `repo::UNATTENDED_RUNS_GRANT` verbatim, between double quotes. Changing either
  wording then fails this test, not a reviewer's memory.

**`run`.** These are `#![cfg(unix)]`, like `tests/scheduler.rs`, because `FakeCli` is a
shell script. Each one pairs against the real server router, maps a real `TempRepo` with a
bare remote, uses `FakeCli` replaying fixture streams and a `TestClock`, satisfies D22's
gate with a stand-in CLI rather than disabling it, and never sleeps. The host is stopped
through `shutdown()`, not a real signal.

- `a_headless_runner_claims_runs_pushes_and_reports_a_task`: a ready task assigned to the
  runner's owner, in a consented checkout, is claimed, run and finished. The branch is on
  the bare remote at the recorded `head_sha` (057). The task is `in_review`, the run's
  `runner_id` is this runner, and its transcript is complete on the server (056).
- `the_host_pushes_to_origin_and_redacts_its_token`: the `RunnerConfig` the host builds
  has `branch_postcondition: OnOrigin` and `host_secrets` covering the token. A fixture
  whose output contains the token yields a transcript on the server with the token
  redacted.
- `run_turns_the_go_signal_on_and_shutdown_turns_it_off`: `queue_state` is `running` after
  `start`, even when the store said `paused`, and after `shutdown` it is `paused` with no
  `active_run_window`.
- `a_blocking_doctor_report_refuses_the_start_and_exits_78`: with the stand-in CLI absent,
  `start` fails with `blocking_summary()`, `queue_state` is not written, nothing is
  claimed, and the board's doctor report for this runner names the failing check.
- `an_unreachable_server_at_start_exits_1`: with the server stopped, `start` fails as `io`
  at the reconcile step, maps to `1`, and starts no loop.
- `shutdown_cancels_in_flight_runs_through_the_cancel_path`: with a `FakeCli` that hangs,
  `shutdown` sends its process group the SIGTERM the cancel path sends. The run is
  recorded exactly as a desktop quit records it, the lease is released, and nothing is left
  in flight.
- `a_report_the_server_did_not_take_is_sent_at_the_next_start`: the server is stopped
  before `shutdown`, the cancelled run's `finish_run` stays in the outbox, and a new host
  started against a restarted server delivers it.
- `an_unconsented_checkout_is_never_claimed_for`: a ready task in a mapped checkout without
  consent stays `idle`, and no agent process is started.
- `run_now_from_the_browser_runs_unattended_on_a_headless_runner`: with every slot busy, a
  Run now for this runner (052), requested over a browser session, is claimed and spawns
  with the unattended permission mode in `FakeCli`'s recorded argv, never `acceptEdits`.
- `a_revoked_token_ends_the_host_as_unauthenticated`: revoking the runner's token through
  047's `revoke_api_token` while the loop is idle resolves `ended()` to `Unauthenticated`
  from the heartbeat's request. After `shutdown`, the in-flight run was cancelled,
  `queue_state` is `paused`, and the exit status is `78`.
- `an_unsupported_protocol_ends_the_host_as_upgrade_required`: a server built to refuse the
  runner's protocol version (by whatever seam 046's tests already use for this) resolves
  `ended()` to `UpgradeRequired` with the minimum version in its message, and nothing is
  claimed.
- `the_headless_runner_serves_no_operator_endpoint`: on the proxy's loopback port, `POST
  /mcp` is `404`, a live run's `/mcp/run/{token}` answers `tools/list` with the run-scoped
  surface (055), and the reported doctor result has no `mcp_port` row.
- `run_refuses_an_unpaired_store` and `run_refuses_when_the_keychain_has_no_token`: each is
  exit `78` with Scope 6's sentences, and neither makes a request.

**`status` and the service definitions.**

- `status_reads_only_the_local_store`: with the server stopped, `status` over a store with
  two checkouts, one held lease, a usage-limit pause, `summaries_only` and one outbox row
  prints exactly the Scope 7 shape for those values. The `Service` line is `running` while
  another handle holds the lock and `stopped` otherwise. An unpaired store prints `Not
  paired.` and exits `78`.
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
  runner comes back and claims again. On the Mac, replace the binary with a rebuilt one,
  and record whether the keychain shows an access prompt nobody can answer, and if it
  does, whether `rimaia-runner status` in a terminal clears it (Notes).
  Then, with both runners mapping one scratch GitHub repository, task 057's checks, which
  need this binary as their first production host:
  - run a task whose plan tells the agent not to push, and see the branch appear on GitHub
    at the recorded `head_sha`;
  - revoke push access, run again, and read the push error on the card;
  - put the Mac to sleep mid-run, wait past its lease, move the task to the Linux runner
    with "run elsewhere", wake the Mac, and confirm that its worktree is kept and fenced
    and that nothing new reached GitHub from it.

## Notes

**Read first.** ADR-0027 points 4 to 6 (the three modes, the port, the crate layout).
ADR-0030 points 3 and 5 (one token shape; pairing). ADR-0031 points 5 to 7 (per-runner
recovery, queue control per runner, Run now and presence). ADR-0012 in full, for the
consent wording. Also ADR-0032 point 4 and point 6's last bullet, ADR-0033 points 1 and 2,
ADR-0035 points 5 and 6, ADR-0036 point 5, and ADR-0037 points 4 and 5. Seam entries:
**D15** and its amendment (what quitting does), **D22** (the gate lives on `start` and
`resume`, and its test consequences), **D31** points 8, 10, 11 and 13 and its 2026-10-04
amendment (where the binary builds its port, the HTTP adapter, the one reaction to
`Conflict`, the suite's server harness, `find_repositories` and `report_runner`), **D33**'s
line for 058 (the binary uses the runner's cache and adds no third one), **D34**'s `clap`
row, D8 (the closed codes behind the exit status), D25 (keychain, not row), D28's runner
set, D30 (`rimaia-run`), D32 (054's local commands, whose core functions the binary
calls), and D3, D9, D10, D11, D19 and D20. Read D4 and D6 as prohibitions.

**The logging dependencies are already decided.** D34's rows for `tracing-subscriber` and
`tracing-appender` name this task: promote both from `src-tauri/Cargo.toml` to
`[workspace.dependencies]` at the shell's `0.3` (default features plus `env-filter`) and
`0.2`, switch `src-tauri` to `{ workspace = true }`, and reference both from
`crates/runner`. Both are already in `Cargo.lock`, so it gains nothing, and 062 reuses the
subscriber line. Hand-rolling a subscriber is not a fallback: `eprintln!` loses every
`tracing` line the loop and `rimaia-core` emit.

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
`rimaia-runner status` reads the item too (Scope 7), so running it once in a terminal after
an update and answering "Always Allow" is the candidate step, simpler than running `run` in
the foreground. The manual check decides which instruction the docs give. Write the one it
proves, not this guess.

**What the earlier tasks provide, and what this task assumes.**

- 040: `RunnerStore::open`, `AppPaths::runner_db_file()` and `runner_identity`, whose
  `server_url` has been NULL until now. This task is its first writer on a headless runner.
- 041: `MachineStore`, `MachineContext`, the checkout contract, and `set_run_environment`.
  066: the consent setter.
- 042: `rimaia_runner::queue::build`, `QueueHandle::start` and `stop`, the queue's shutdown,
  `SoloBoard`'s four reads, and `ClaimTarget::Next` listing only consented checkouts.
- 043 and 053: `held_leases`, the per-runner reconcile, `authorize_start`, `Conflict`,
  `heartbeat::spawn` and its tick, `claim_wait` and `CLAIM_WAIT_MAX`, and `app_version` on
  the heartbeat.
- 046: the `Rimaia-Protocol` header and `UpgradeRequired`. 047: `POST /api/v1/auth/pair`
  answering `{ runnerId, token }`, `create_pairing_code` and `revoke_api_token`.
- 048: `LocalEvents`. 052: `HttpBoard`, and the relayed Run now.
- 054: `machine::checkouts::map` over a `&dyn BoardPort`, `unmap_repository_checkout`'s
  core function, `normalized_remote`, the startup verification, `CredentialKey`,
  `find_repositories` (D31's 2026-10-04 amendment), and `report_runner` with its dirty
  resend.
- 055: `run_proxy::bind`. 056: `OutboxBoard` and its sender, `host_secrets`,
  `UPLOAD_DISCLOSURE` and the `upload_transcripts` accessor. 057: `BranchPostcondition` and
  `worktrees` on `claim(Next)`.

Where one of these landed under a different name, use that name. Where one is missing
entirely, stop: this task composes, and it does not build a missing layer on the way.

**What 059 expects.** `rimaia_runner::host` with `HostConfig` and `HostEnd`, which it starts
in connected mode, with `Unauthenticated` turned into "sign in again" rather than an exit.
The `RunnerConfig` the host builds, so the desktop pushes to origin and redacts its tokens
without composing anything itself. `CredentialKey::RunnerToken`, beside which it adds its
two token variants, and `credentials::runner_token` and `save_runner_token`. `HttpBoard`'s
refusal channel. `headless::default_label`. The `runner.lock` convention: the connected
desktop should take the same lock on its own data directory, so that `RIMAIA_DATA_DIR`
pointed at both cannot put two loops on one `runner.db`. 069 reads this task's seam entry
for the settings that still have no control.

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
- `crates/core/src/doctor/mod.rs` (`Environment`) and `checks.rs` (`mcp_port`).
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

**Size.** L. It composes names from fourteen earlier tasks, and its main risk is misreading
one of their contracts, not the volume. Roughly: the binary and CLI parsing 200 lines;
paths, lock and exit status 200; pair, the token key and the two questions 300; checkout
and consent 300; the host and the runner-only reads 450; status 150; the service
definitions 200; docs and seam entries 300; and about 1,350 lines of tests. That is about
3,450 lines. If it runs over, cut in this order, each into a follow-up with
the next free number placed directly after 058 in `tasks/README.md`: first `service` and
its tests (the docs can show a hand-written unit instead), then `status`. `pair`,
`checkout`, `consent` and `run` are the task. Without any one of them a headless runner
cannot do a night's work.
