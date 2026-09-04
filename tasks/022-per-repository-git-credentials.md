---
id: "022"
title: Per-repository git credentials
milestone: v0.3
status: ready
depends_on: ["003", "008"]
adrs: ["0020", "0012", "0003"]
size: L
landed: "#28"
---

# Per-repository git credentials

## Goal

Let a repository carry its own forge token, held in the OS keychain, injected into that
repository's runs and no others, so an unattended run acts with the access the user granted
that repository rather than with the operator's whole GitHub account.

## Why now

ADR-0012 opted a repository into `bypassPermissions` and left the credential question open,
on the grounds that the operator knows what is in their own environment. That holds while
every queued repository is the operator's own. It stops holding the first night a queue
runs a client repository and a side project on the same machine, and it never covered the
capability half: a repository the operator's account cannot push to produces a run that
implements the plan and then cannot open the PR the base instructions demand.

## Cross-platform is a requirement of this task, not a follow-up

ADR-0002 targets macOS first and keeps Windows and Linux viable — *"no macOS-only APIs in
the core"*. This task is the one most likely to break that quietly, because every part of
it (secret storage, git authentication, credential prompts) has a different native
mechanism per platform. **Every decision below is chosen because it behaves the same on all
three**, and where it cannot, the difference is surfaced rather than papered over. A
solution that works on macOS and needs a rethink on Windows has not met this task's
contract.

## Scope

**Storage — `crates/core/src/credentials/`**

- A `CredentialStore` trait: `set(repository_id, secret)`, `get`, `delete`, `status`. One
  real implementation over the [`keyring`](https://crates.io/crates/keyring) crate v3, one
  in-memory implementation behind the `testing` feature. The trait exists so tests never
  touch a real keychain — CI has no unlocked keychain and no D-Bus, and a test that needs
  one is a test that cannot run.
- Platform backends, all enabled: `apple-native` (macOS Keychain), `windows-native`
  (Windows Credential Manager), `sync-secret-service` (Linux, libsecret over D-Bus).
  Service name is the app's bundle identifier; the account is the repository id, so one
  item per repository and no parsing of composite keys.
- `keyring` compiles on a headless Linux box but *fails at runtime* without a running
  secret service. That is a real user state (a server, a fresh container, a locked login
  keyring), and `status` exists to report it as "no keychain available on this machine"
  rather than as a failed save.
- The secret is a `Secret(String)` newtype with a hand-written `Debug` printing `***`, no
  `Serialize`, no `Display`. It never reaches the database: `repositories` gains
  `credential_login TEXT NULL`, `credential_label TEXT NULL`, `credential_added_at TEXT
  NULL`, and nothing else. A new migration, append-only.
- Windows Credential Manager caps a secret at 2560 bytes and Linux imposes no such limit —
  validate length on save so the failure is a message at paste time on every platform, not
  a truncated token on one.

**Provisioning — Settings → Repositories**

- Paste a token, give it a label, save. Rimaia verifies before storing by running
  `gh api user` and `gh api repos/{owner}/{repo}` with only that token in the child's
  environment, and stores the resolved login as `credential_login`.
- Three outcomes, three different messages: **verified** (save, show login), **rejected by
  the forge** (refuse the save, ADR-0020's "refused at paste time"), and **could not
  verify** — `gh` is not installed — which saves with an unverified marker, because a
  missing local tool says nothing about the token. Task 018's doctor re-runs the same check.
- Verification uses `gh`, not an HTTP client: it needs no new dependency, it is the same
  binary and the same auth precedence the run will use, and task 018 already lists `gh` as
  a per-repository prerequisite. Argument vectors, never `sh -c` (Windows has no `sh`).
- After saving, the value is write-only: **replace** and **remove**, never **show**.
- When `origin` is an SSH remote, the pane says so — the credential covers `gh` API calls
  and any HTTPS remote, and the push will use the machine's SSH key regardless. ADR-0020
  point 6: silence here would let a user believe Rimaia controls an access path it does not.

**Injection — `crates/core/src/runner/process.rs`**

Same seam as the `CLAUDE_*` strip, and a third environment rule alongside the two in that
module's header. All of it is env vars on the child; nothing is written to disk, nothing is
a command argument (`ps` is world-readable on Unix, and the Windows equivalents are no
better).

- `GH_TOKEN` for the `gh` CLI.
- HTTPS git auth through git's own environment configuration, which is identical on all
  three platforms and needs no credential helper:
  `GIT_CONFIG_COUNT=1`, `GIT_CONFIG_KEY_0=http.https://github.com/.extraheader`,
  `GIT_CONFIG_VALUE_0=Authorization: Basic <base64("x-access-token:" + token)>`.
  Deliberately not a `credential.helper` shell snippet: that is `sh -c` by another name and
  does not exist on Windows.
- Inherited `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*` are removed
  first. Appending to an operator's existing count is an off-by-one that silently drops
  either their config or ours.
- `GIT_TERMINAL_PROMPT=0`, and the Git Credential Manager's non-interactive setting, so a
  bad credential fails immediately instead of blocking on a prompt no one will answer.
  Windows ships GCM by default and is where this bites.
- When a repository has a credential, inherited `GH_TOKEN`, `GITHUB_TOKEN`,
  `GH_ENTERPRISE_TOKEN` and `GH_CONFIG_DIR` are removed before the repository's own is
  added (ADR-0020 point 5). A repository without one keeps today's ambient behaviour
  exactly.
- **Fail closed**: a repository with `credential_login` set whose keychain item is missing,
  or whose unlock the user denies, refuses to start the run with an error naming the
  repository. Never a silent fall back to the ambient login.
- `Invocation` stays a pure function of (task, repository, settings, trigger) → argument
  vector. The credential is part of the *environment*, resolved at spawn; keep the argv
  contract that makes ADR-0012's flags assertable byte for byte.

**Redaction**

- The token value is scrubbed from ADR-0013's JSONL transcript and from the live tail
  before write, and from `tracing` output. A run that echoes its own environment is a
  realistic thing for an agent to do while debugging a push failure.

**CI**

- `ci.yml`'s core job gains an OS matrix — `ubuntu-latest`, `macos-latest`,
  `windows-latest` — running the same commands CLAUDE.md documents, unchanged. Compiling
  the keychain backends and the environment-building code on all three is the only thing
  that keeps this task's promise true after the next change to it.
- Keep `cargo fmt` and `clippy` on Linux only; they are platform-independent and three
  copies of the same lint run buys nothing.

## Acceptance criteria

- A repository with a saved credential runs with `GH_TOKEN` set to that token and with the
  operator's ambient `GH_TOKEN`/`GITHUB_TOKEN` absent from the child environment, asserted
  as an exact environment diff.
- A repository without a credential produces a child environment byte-identical to today's.
- A repository whose credential is configured but missing from the keychain refuses to
  start, naming the repository. It does not run with ambient credentials.
- The stored secret appears in no row of the database, no transcript, no log line, and no
  `Debug` output — asserted by a test that provisions a known sentinel value and greps
  everything the run wrote for it.
- Saving a token the forge rejects refuses the save; saving with `gh` absent stores it
  marked unverified.
- `cargo test -p rimaia-core` passes on Linux, macOS and Windows in CI, with no keychain,
  no D-Bus and no unlocked login session on any of them.
- Cloning and pushing an HTTPS remote inside a run works with the injected credential,
  verified against a real local repository in a `tempfile::TempDir` — never a mocked git.
- A repository with an SSH `origin` shows the SSH notice in its settings pane.

## Out of scope

- Every non-GitHub secret in the operator's environment: AWS profiles, npm tokens, and the
  rest are still inherited. ADR-0020 claims the forge credential and nothing else.
- Rotation, expiry scheduling and vault integration. Replace and remove only; the "expires
  in N days" check belongs to task 018's doctor, using the verification call this task
  already writes.
- Forges other than GitHub. The storage seam is forge-agnostic; the injection is not, and
  guessing at GitLab's variables before anyone has asked for them is guessing.
- GitHub App installation tokens (ADR-0020's alternatives). They need a hosted component
  ADR-0002 declines.

## Notes

The honest limit, stated in ADR-0020 and worth repeating to whoever implements this: a
`bypassPermissions` run can read its own environment, so this bounds what a stolen token is
*worth*, not whether it can be stolen. The UI guidance toward a fine-grained,
single-repository token is doing as much work as the keychain is.
