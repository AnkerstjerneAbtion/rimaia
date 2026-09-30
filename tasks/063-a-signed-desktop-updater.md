---
id: "063"
title: A signed desktop updater
milestone: v0.5
status: ready
depends_on: ["059"]
adrs: ["0037"]
size: S
---

# A signed desktop updater

## Goal

Give the desktop app the update path
[ADR-0037](../docs/adr/0037-hosting-backups-and-version-skew.md) point 5 asks for, and give
the web the out-of-date list the same point promises:

- **The desktop checks a signed update feed once at launch**, through
  `tauri-plugin-updater`, and offers what it finds. Nothing installs without a click.
- **A connected desktop that the server refuses for its version is offered the update
  immediately**, not at the next launch. Its runner has already stopped taking work
  (ADR-0037 point 4), so the offer is the only way forward, and it says so.
- **Every runner's reported version is judged against the board's**, and the account page
  and the browser's runners section mark a runner that is out of date. Headless runners
  are covered by this and by nothing else: they do not update themselves.

**The signing keys and the feed's address are supplied by a person later.** This task
lands the machinery with both empty, so a development build, CI and every fork never
contact a feed. Tests use a fake feed on loopback and a test key generated once with the
Tauri CLI and committed as a fixture. A test proves the shipped configuration never
trusts that key.

## Why now

052 to 059 made version skew possible. Before them, solo was one process on one version.
Now a server deploy can leave every member's desktop older than the board it reports to.
ADR-0037 point 4 answers that loudly: a runner that is too old stops claiming and says
why. Without this task, "says why" is a dead end. The person reading the refusal has
nothing to click, and a headless runner's owner has nothing on the web that tells them
which machine to go and update.

Three earlier tasks left exactly this piece open. 050 reloads the page on
`upgrade_required` in the browser and leaves the desktop modes to this task. 049 reserved
`canUpdate` on `ClientCapabilities`. D28 gives `runners.app_version` a writer (053's
heartbeat) and names this task as its reader. 064 is the final documentation pass and
should document an updater that exists, not one that is planned.

## Scope

**1. One version for every artefact.** The judgement in Scope 6 compares a runner's
version with the board's, and the updater compares the feed's with the app's. Both are
meaningless if the crates can drift apart.

- The root `Cargo.toml` gains `[workspace.package] version = "0.1.0"`. `crates/core`,
  `crates/runner`, `crates/server` and `src-tauri` each say `version.workspace = true`.
- `"version"` is removed from `src-tauri/tauri.conf.json`. Tauri then reads it from
  `src-tauri/Cargo.toml`, so the bundle's version and the updater's current version are the
  workspace's.
- `rimaia_core::APP_VERSION: &str = env!("CARGO_PKG_VERSION")` is the one constant.
  `commands::app::get_app_info` uses it. So does whatever 053 and 058 send as
  `app_version`: if either reads its own `env!` instead, switch it to the constant, which
  now has the same value by construction.
- `package.json`'s `version` is not a source and is left alone. Nothing reads it.

**2. The plugin, wired from Rust only.** D34 approves `tauri-plugin-updater = "2"` and
`@tauri-apps/plugin-updater`. This task takes the Cargo half and **declines the npm half**.

- The check has two triggers, and one of them is in Rust: the connected runner loop meets
  `UpgradeRequired` on a claim or a heartbeat, with no webview involved. One owner for the
  check, the throttle and the held `Update` is therefore the shell, and the webview asks
  it through local commands (Scope 5), the same shape 049 gave `choose_folder`.
- So `package.json` does not change, and `updater:default` is **not** added to
  `src-tauri/capabilities/default.json`. The webview cannot start a download or an install
  by any path other than `install_update`.
- `src-tauri/Cargo.toml` gains `tauri-plugin-updater = "2"`, declared the way the other
  plugins are and as D34 spells it. `src-tauri/src/lib.rs` registers it beside
  `tauri_plugin_opener`, `dialog` and `notification`.
- D34 gets `### Amendment — 063: the npm half is not taken`, with the reason above and
  the release overlay below. Declining an approved package is still a visible difference
  from the entry, so it is recorded rather than left for a reviewer to find.

**3. Configuration: empty in the tree, filled for a release.**

- `src-tauri/tauri.conf.json` gains `"plugins": { "updater": { "pubkey": "",
  "endpoints": [] } }`. The plugin needs its section to initialise, and empty values mean
  "not configured".
- `src-tauri/tauri.release.conf.json` is new. It holds `bundle.createUpdaterArtifacts:
  true` and the same `plugins.updater` keys, also empty until a person fills them.
  `package.json` gains `"tauri:release": "tauri build --config
  src-tauri/tauri.release.conf.json"`. This refines D34, which put
  `createUpdaterArtifacts` in `tauri.conf.json`. There it would make every developer's
  `npm run tauri build` demand a signing key they do not have (task 018's bundles are built
  locally). The amendment from Scope 2 records it.
- **The feed request carries nothing about the installation.** The endpoint template uses
  `{{target}}` and `{{arch}}` only, never `{{current_version}}`. The request sends no token,
  no cookie, no `Rimaia-Protocol`, no runner id and no board content. That is how a launch
  check in solo stays compatible with ADR-0037 point 6's "solo sends nothing to any Rimaia
  server": the feed is a static file, and the request is a download of it.
- `docs/releasing.md` is new and short: generate the key pair with `npm run tauri signer
  generate`, where the private key and its password live (the release environment, never
  the repository), which two values go into the overlay, the manifest shape the feed
  serves, and the manual checks in this task's Notes. 064 links it; it does not rewrite it.

**4. The rules, in core** (`crates/core/src/update.rs`). They are business rules, and CI
runs only `rimaia-core`'s tests on all three platforms, so they live there as pure types
over the injected `Clock`:

- `UpdaterSettings::from_plugin_config(&serde_json::Value) -> Option<UpdaterSettings>`:
  `None` when `pubkey` is empty or `endpoints` is empty or absent. The shell passes the
  `plugins.updater` value from its own config.
- `installs_in_place(os: &str, appimage: bool) -> bool`: false only on Linux outside an
  AppImage, where the plugin cannot replace a `.deb` or `.rpm` install.
- `UpdateOffer { current_version, version: Option<String>, notes: Option<String>, reason:
  OfferReason, server_message: Option<String> }`, with `OfferReason::{Launch,
  UpgradeRequired}` serialized as `launch` and `upgrade_required` ("Enums, not strings").
  `version: None` only occurs with `UpgradeRequired`. It means the server refused this
  version and the feed had nothing newer, or could not be reached.
- `UpdateGate`, which decides when to ask the feed:
  - `on_launch()` asks for a check exactly once per process.
  - `on_upgrade_required(message)` asks for a check unless one finished within
    `UPGRADE_RECHECK_INTERVAL` (15 minutes). A refused desktop meets the refusal on every
    board write, and a static feed does not need four requests a second. Fifteen minutes
    also bounds how long a release published after the refusal takes to reach the machine.
  - An offer already found is surfaced again at once, with its reason upgraded to
    `upgrade_required` and the server's message attached, without a second check.
  - `record_check(Result<Option<FeedUpdate>>)` stores the answer and the time. A failed
    check is an answer with no version. It is logged at `warn` by the shell and never
    becomes an error dialog.

**5. The shell** (`src-tauri/src/updater.rs`, plus rows in `commands/app.rs` or a new
`commands/update.rs`).

- `AppState` gains the `UpdateGate` and a `Mutex<Option<tauri_plugin_updater::Update>>`
  holding the last update found.
- **At launch**, after the main window is shown and only when `UpdaterSettings` is `Some`,
  one check is spawned. It never delays startup and never reaches D11's startup failure: a
  feed that is down is not a broken installation.
- **The check** builds `app.updater_builder()` with the settings' endpoints and pubkey, runs
  it, records the answer in the gate, and emits the local event `update:offered` with the
  `UpdateOffer` when there is one.
- **Three local commands**, each a `local(…)` row in D32's registry, an entry in the single
  `generate_handler!` list, and a `local<T>` wrapper in `src/lib/commands.ts`:
  - `get_update_offer` returns `UpdateOffer | null`. It is how a window that mounted after
    the event still sees the offer.
  - `report_upgrade_required { message }` feeds the gate from the webview's transport.
  - `install_update` runs **download, then the exit path, then install, then restart.**
    The plugin's `download` verifies the signature against the configured pubkey and fails
    on a mismatch before anything is written. Then comes `shut_down(&app)`, the same
    function a quit runs: it pauses the queue (D15) and ends in-flight runs as
    `interrupted` (D9). Only then is the update installed, because on Windows the installer
    ends the process itself. Last, `AppHandle::restart()`, whose `ExitRequested` carries a
    code and is therefore let through by the closure in `lib.rs`. D34 declines
    `tauri-plugin-process` for exactly this. A signature failure is `Error::internal`
    with the message "The update's signature did not match. Nothing was installed." D8's
    error type does not grow.
- **The connected runner loop** hands the refusal to the shell. `crates/runner` must not
  depend on `tauri`, so the host passes a callback when it builds the loop. The shell's
  callback calls `on_upgrade_required` and, if the gate asks, checks. If 052 or 053
  already made a refused runner stop claiming, reuse it. If neither did, this task adds
  it, with the test named below, because ADR-0037 point 4's "stops taking work" is not
  optional. The headless runner's callback logs the server's message once at `error`.
- **No MCP tool** for any of the three commands. Installing restarts the process that
  serves the MCP endpoint, and an agent run inside it would be ending itself. The other two
  exist only to drive that banner. This is a deliberate ADR-0021 exception, like
  `delete_task`, and each registry row carries a one-line comment saying so.

**6. Runners that are out of date.**

- `crates/core/src/api/board/` (wherever 050 put `list_runners`) gains a pure
  `runner_currency(reported: Option<&str>, board: &str) -> RunnerCurrency`. The versions
  are `MAJOR.MINOR.PATCH` with an optional `-pre` suffix. A pre-release sorts below its
  release, and anything unparseable is `Unknown`, never an error. It is hand-parsed:
  D34 approves no `semver` crate, and three integers do not need one.
- Each `list_runners` row gains `currency: { state, boardVersion }`, with `state` one of
  `current`, `out_of_date`, `ahead` and `unknown`, and `boardVersion` equal to
  `APP_VERSION`. No query changes; the column is already selected. If one does change,
  regenerate the board cache with D33's recipe.
- `src/views/AccountView.tsx`'s runners list and `src/views/settings/RunnersSection.tsx`
  show one line per runner that is not `current`:
  - `out_of_date`: "Out of date. The server runs {boardVersion}."
  - `ahead`: "Newer than the server ({boardVersion})."
  - `unknown`: "Version not reported yet."
- **The judgement is about versions, not protocols.** A runner one patch behind is marked
  out of date while it still works, and the web cannot tell it apart from one that is being
  refused. Telling them apart would need the protocol a runner last sent, which has no D28
  column. This is recorded, not guessed at.

**7. The frontend.**

- `src/types.ts` gains `UpdateOffer`, `OfferReason`, `RunnerCurrency` and
  `ClientCapabilities.canUpdate`. `crates/core/src/api/capabilities.rs` gains the Rust
  field. `ClientCapabilities::desktop` takes it as a parameter: `UpdaterSettings` is `Some`
  and `installs_in_place` holds. The browser's fixed answer is `canUpdate: false`.
- `src/lib/events.ts` gains `subscribeToUpdateOffered`, a local subscription on
  `update:offered`.
- **In connected mode**, an `upgrade_required` from a board command or from the event
  stream calls `reportUpgradeRequired(message)`. It is wired in the same `src/lib/` place
  where 050 reacts to `upgrade_required` in the browser, so no component handles the code.
  Browser mode keeps 050's one reload and is untouched. Solo never sees the code.
- `src/components/UpdateBanner.tsx` is new and sits in `src/App.tsx` beside
  `DoctorBanner`. It reads `getUpdateOffer()` on mount and subscribes to the event. The
  copy, exactly:
  - Launch: "Rimaia {version} is available. You have {current}." with **Install and
    restart** and **Later**. Later hides it until the next launch.
  - Upgrade required, with a version: "The server no longer accepts Rimaia {current}, so
    this machine has stopped taking work. Rimaia {version} is available." with **Install
    and restart** and no Later.
  - Upgrade required, without a version: "The server no longer accepts Rimaia {current},
    so this machine has stopped taking work. No update is available right now. The server
    said: {serverMessage}"
  - With `canUpdate` false, the Install button is replaced by "Install it the way you
    installed Rimaia."
  - **Install and restart**, with runs in flight (from the same read `QueueControls`
    uses), first asks: "Restarting stops {n} running task(s). They are marked interrupted,
    and the queue stays paused until you start it." with **Restart and install** and
    **Cancel**.
  - While `install_update` runs, the button reads "Installing…" and is disabled. An error
    renders through `ErrorBanner`.
- **Fixture mode (028):** rows for the three new commands, `canUpdate` in the capability
  rows, and a scenario `update` showing the three banner variants. The `browser` scenario
  gains one runner of each `currency` state. Run `npm run screenshot` and look at them.

**8. CI and CLAUDE.md.** The signed round trip needs the plugin, and the plugin lives in the
shell, where nothing runs tests today.

- `src-tauri/Cargo.toml` gains `[dev-dependencies]` of `tauri` with the `test` feature
  (the mock runtime, no window) and the workspace's `axum` and `tokio` for the fake feed.
  All three are crates already in the tree. If the `test` feature pulls a crate that is
  not in `Cargo.lock`, stop and ask (D6).
- `.github/workflows/ci.yml`'s `shell` job gains one step after the check:
  `cargo test -p rimaia --lib updater`. It runs on Linux only, where the job already
  installs WebKit's headers.
- CLAUDE.md's command block gains the same line, identically.

## Out of scope

- **Generating the real key pair, choosing the feed's host, publishing a release, and a
  release workflow.** A person does all four, following `docs/releasing.md`.
- **Serving the feed from `rimaia-server`.** The feed is a static manifest wherever
  releases are published. An `/api/v1` route would tie every desktop's update to one
  server's availability, and to a self-hosted server that may be older.
- **Offering the release that matches the server.** The feed offers the latest release. A
  desktop newer than a self-hosted server within one minor is served (ADR-0037 point 4).
  Beyond that it is refused as newer, and the banner shows the server's own message, which
  names what it accepts.
- **Updating the headless runner.** It is installed by whatever installed it, and the web
  list tells its owner when to. Its refusal logging is 052's and 058's, and this task
  only supplies the callback.
- **Periodic checks, background downloads, installing without a click, update channels
  and rollback.** ADR-0037 says "checks at launch".
- **macOS notarisation and Windows code signing.** Those sign the bundle for the OS and are
  task 018's packaging. The updater's signature is a separate key and a separate check.
- **Updating the server.** 062's deploys.
- **Recording which protocol a runner last spoke** (Scope 6). It needs a D28 amendment.

## Acceptance criteria

Rust tests use `crates/core`'s fake `Clock` and contain no `sleep`. The feed tests serve
real bytes over a real loopback socket and verify real signatures. Frontend tests are
vitest, mocking at `@tauri-apps/api/core` and at 049's HTTP mock, never the wrappers, and
they assert exact command names, arguments and copy.

**The gate, in `crates/core/src/update.rs`:**

- `the_launch_check_is_asked_for_once_per_process`.
- `upgrade_required_checks_when_nothing_was_checked_recently`.
- `upgrade_required_within_fifteen_minutes_of_a_check_does_not_check_again`. The clock is
  advanced to 14:59 (no check) and then to 15:00 (a check).
- `an_offer_already_found_is_surfaced_at_once_as_upgrade_required`: it carries the server's
  message and makes no second check.
- `a_failed_check_is_an_answer_with_no_version`: under `upgrade_required` it yields an
  offer with `version: None`, and under `launch` it yields no offer.
- `an_empty_pubkey_or_no_endpoints_means_updates_are_not_configured`, and
  `a_filled_in_config_is_configured`.
- `linux_outside_an_appimage_cannot_install_in_place`. macOS, Windows and an AppImage can.

**Currency, as core unit tests plus one through `list_runners`:**

- `a_runner_on_the_boards_version_is_current`, `an_older_patch_is_out_of_date`,
  `a_newer_runner_is_ahead`, `a_runner_that_never_reported_is_unknown`,
  `an_unparseable_version_is_unknown_not_an_error`,
  `a_prerelease_sorts_below_its_release`.
- `list_runners_judges_each_runner_against_the_boards_version`: three runners with
  `app_version` equal to `APP_VERSION`, `0.0.1` and `NULL` answer `current`,
  `out_of_date` and `unknown`, each with `boardVersion` equal to `APP_VERSION`.
- `every_crate_and_the_bundle_share_one_version`: the root manifest declares
  `[workspace.package] version`, each of the four member manifests says
  `version.workspace = true`, and `src-tauri/tauri.conf.json` has no `version` key.

**The runner loop, in `crates/runner`** (skip only if 052 or 053 already has an
equivalent test, and name it in the PR):

- `a_runner_refused_for_its_version_stops_claiming_and_reports_it_once`: a fake
  `BoardPort` answers `claim` with `UpgradeRequired`. After the clock advances past
  several poll intervals, the port has seen exactly one claim and the callback has fired
  exactly once, with the server's message.

**The signed round trip, in `src-tauri/src/updater.rs`** (`cargo test -p rimaia --lib
updater`). A mock app is built with the plugin, and the feed is an axum server on
`127.0.0.1:0`. The fixtures are in `src-tauri/tests/fixtures/updater/`: the test key pair
generated with `npm run tauri signer generate`, a small artifact, its signature from the
test key, and a signature from a second throwaway key. The builder's `target` is fixed so
that the manifest's platform key is the same on every OS.

- `a_feed_signed_with_the_trusted_key_downloads_the_exact_bytes`.
- `an_artifact_that_does_not_match_its_signature_is_refused`: one flipped byte is enough.
- `a_signature_from_another_key_is_refused`.
- `a_feed_with_nothing_newer_is_no_offer`.
- `the_check_sends_no_credential_and_no_board_headers`: the fake feed records the request,
  whose path holds only target and arch, and which has no `authorization`, `cookie` or
  `rimaia-protocol` header.
- `the_shipped_configs_never_trust_the_test_key`: neither `tauri.conf.json` nor
  `tauri.release.conf.json` contains the fixture's public key, and neither sets
  `dangerousInsecureTransportProtocol`. If the plugin refuses a loopback `http://` endpoint
  in a test build, that flag is set on the test's own builder only.
- `the_development_config_is_not_configured`: `tauri.conf.json`'s updater section yields
  `None` from `UpdaterSettings::from_plugin_config`.

**Frontend:**

- `UpdateBanner.test.tsx`: each of the three variants renders its exact copy. Later hides
  the launch variant, and the upgrade variant has no Later. `canUpdate: false` replaces the
  button with the install-it-yourself sentence. With two runs in flight, Install asks the
  exact confirmation first and calls `install_update` only after **Restart and install**.
  With none, it calls `install_update` directly. An offer arriving on `update:offered`
  after mount is shown. With no offer, nothing is rendered.
- A transport test: in connected mode, a board command answered `426` with
  `{"code":"upgrade_required","message":"…"}` calls `report_upgrade_required` with that
  message and does **not** reload. The same response in browser mode still reloads once,
  which is 050's test and must still pass unchanged.
- The runners list shows each `currency` line exactly, and nothing for `current`.
- 049's capability tests are updated for `canUpdate`: the browser answer is `false`.
- 028's fixture coverage test passes with the three new rows, and the `update` scenario
  renders.

**Wiring and hygiene:**

- `package.json` and `src-tauri/capabilities/default.json` are unchanged apart from the
  `tauri:release` script. No file under `src/` imports `@tauri-apps/plugin-updater`.
- `./scripts/check-command-wiring.sh` passes with the three new local rows.
- D34 carries the amendment from Scopes 2 and 3. `docs/releasing.md` exists.
- **No migration was added.**
- Every CI check passes, the new step included: `npm run typecheck`, `npm run test`,
  `npm run build`, `cargo test -p rimaia-core` and the runner and server crates' tests as
  CLAUDE.md lists them at this point, `cargo fmt --all --check`, clippy as listed,
  `cargo check --workspace --all-targets`, `cargo test -p rimaia --lib updater`, and
  `./scripts/check-command-wiring.sh`. CLAUDE.md's block and `ci.yml` still agree.

## Notes

**Seam entries to read:** **D34** (the updater rows, "no `tauri-plugin-process`", and the
amendment this task writes), **D32** points 3, 7 and 8 (local rows, `upgrade_required`,
"a local handler never reads the board's `ServiceContext`"), **D28** part 6 (`runners.
app_version`, and `runner_identity` for the mode), **D15** (quitting pauses the queue,
which an update restart inherits), **D9** (what `interrupted` means), **D7** (`events.ts`
is the only subscription seam), **D8** (no new error code), **D11** (why a feed outage is
not a startup failure), and **D6** as a prohibition.

**Migration:** none. `.sqlx/` should need no regeneration.

**Files to start from.** `src-tauri/src/lib.rs` (plugin registration next to line 39, and
`shut_down` plus the `RunEvent::ExitRequested` closure near line 600),
`src-tauri/src/state.rs`, `src-tauri/src/commands/app.rs` (`get_app_info`),
`src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`, `src-tauri/Cargo.toml`,
the root `Cargo.toml`, `crates/core/src/clock.rs`, `src/App.tsx`,
`src/components/DoctorBanner.tsx` (the banner to sit beside and to copy the shape of),
`src/components/runs/QueueControls.tsx`, `src/lib/commands.ts`, `src/lib/events.ts`,
`src/types.ts`, and `.github/workflows/ci.yml`. From earlier tasks on this branch:
`crates/core/src/api/{registry,capabilities}.rs` (046, 049), `src/lib/client.ts` and
`src/test/http.ts` (049), `src/views/AccountView.tsx` and
`src/views/settings/RunnersSection.tsx` (050), `crates/runner/` (040 to 042, 052, 058),
and `src/dev/fixtures/` (028).

**What earlier tasks provide.**

- **046:** `PROTOCOL_VERSION`, `api::protocol::check`, `Error::UpgradeRequired` with a
  message naming the version received and the oldest accepted, and the registry's
  `local(…)` rows.
- **049:** `ClientCapabilities` with 049's note that "063 will add `canUpdate`", the
  connected transport, and `subscribeToEventStreamFailure`.
- **050:** the browser's one reload on `upgrade_required`, "in the desktop modes,
  `upgrade_required` is left to 063's updater", and `list_runners` with `appVersion`.
- **053:** the heartbeat writes `runners.app_version`. **If no landed task writes that
  column, stop and say so.** Its writer is the heartbeat, and this task must not open a
  second report path to fill it.
- **058:** `rimaia-runner --version`, which a person compares against the web list.
- **059:** the connected desktop, its runner loop built by the shell over the HTTP
  `BoardPort`, and `get_client_capabilities` answering `connected`.

**What later tasks expect.** **064** documents the updater and links `docs/releasing.md`,
and finds CLAUDE.md and `ci.yml` already agreeing on the new step. **065** is unaffected.

**A known gap, recorded rather than fixed.** A runner whose very first heartbeat is refused
never records a version, so it shows "Version not reported yet" rather than "Out of date".
A runner that was paired before the server moved on already has its version recorded, and
that version stays accurate, because a runner that cannot report cannot have upgraded
either. Recording the version on a refused request would change what 053's heartbeat does
under refusal. That belongs in 053, not here.

**Needs a person, after landing** (the PR body carries these as a checklist, per
`tasks/README.md`'s "landed is not the same as proven"):

- Generate the release key pair, fill in `tauri.release.conf.json`, and publish one feed.
- On macOS, install an older signed build, launch it, see the launch offer, install it,
  and confirm that the app restarts on the new version with the queue paused.
- Against a local `rimaia-server` whose `PROTOCOL_VERSION` has moved two minors ahead,
  confirm that a connected desktop stops claiming, shows the upgrade banner without Later,
  and that the web list marks its runner out of date.

**Size.** Roughly 1,800 to 2,300 lines: about 300 of core rules and 350 of their tests,
250 of shell code and 250 of round-trip tests plus fixtures, 400 of frontend and 400 of its
tests, and the rest configuration, CI and `docs/releasing.md`. That is at the top of an S,
and fits one session. If it runs long, cut Scope 6 (currency, its UI lines and its tests)
into a follow-up task that depends on this one. It is independent of the updater and
touches different files. **Never cut** the signature-refusal tests or
`the_shipped_configs_never_trust_the_test_key`. Without them, nothing shows that a
committed private key cannot sign a production update.
