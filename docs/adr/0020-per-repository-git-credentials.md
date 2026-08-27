# 20. Per-repository git credentials, held by Rimaia

- **Status:** Accepted
- **Date:** 2026-08-27

## Context

ADR-0012 opts a repository into `--permission-mode bypassPermissions`, and the work a run
does — commit, push, open a PR — is `git` and `gh`, which are bash. So every unattended run
authenticates to GitHub as somebody, and today that somebody is **the operator, globally**.

`crates/core/src/runner/process.rs` builds the child environment with exactly two rules:
`run_environment` chooses `inherit` or `strict_local` for the operator's *Claude Code*
configuration, and `CLAUDE_*` process-identity variables are stripped unconditionally.
Nothing else is removed and nothing is injected. Whatever `gh auth` login, `GH_TOKEN`, or
osxkeychain git credential the operator has is what a run gets — which means a run started
for one repository holds push rights to every repository and organisation that account can
reach. ADR-0012 saw this and declined to solve it: *"a user running this on a machine with
production credentials in the environment should know that."* That was the right call for
an MVP whose blast radius the user could hold in their head. It stops being right the
moment a queue runs against repositories the operator does not personally own, or against
a work account and a side project on the same machine overnight.

There is a capability half to this too, and it is not hypothetical. A repository whose
`origin` the operator's account cannot push to produces a run that implements the plan
perfectly and then cannot open the PR that the base instructions require — a failure
discovered in the morning, after the work is done.

Two constraints bound the solution. ADR-0003 keeps the store as a plaintext SQLite file on
disk, so it cannot hold a secret. ADR-0002 refuses a hosted component and an auth boundary,
so there is nothing to mint short-lived tokens from.

## Decision

**A repository may carry its own forge credential. Rimaia holds it in the OS keychain,
injects it into that repository's runs and no others, and never writes it to its own
database, its prompts, or its transcripts.**

1. **Storage is the OS keychain, never the database.** One keychain item per repository,
   under Rimaia's service name, keyed by repository id. The `repositories` row gains only
   non-secret metadata — that a credential exists, the login it verified as, the label the
   user gave it, when it was added — alongside `allow_unattended_runs`, which is already
   the per-repository security posture column. Deleting a repository deletes its keychain
   item.

2. **Provisioning verifies before it saves, and never reads back.** The user pastes a token
   in the repository's settings; Rimaia calls the forge with it, confirms it can reach that
   repository, and stores it with the login it resolved to. After that the value is
   write-only: the UI offers *replace* and *remove*, never *show*. A token that fails
   verification is refused at paste time, not discovered at 2am.

3. **Injection happens at the spawn seam, in the environment, never in argv.** The same
   place `CLAUDE_*` is stripped is where the credential is added: `GH_TOKEN` for the `gh`
   CLI, and an equivalent per-child git configuration for HTTPS remotes. Never a command
   argument — `ps` is world-readable — and never a file written under the worktree.

4. **Fail closed.** A repository configured with a credential whose keychain item is
   missing, or whose unlock the user denies, does not start its run. It reports the
   repository by name and stops. Falling back to the ambient login would hand the run
   exactly the identity this ADR exists to take away from it, at the moment nobody is
   watching.

5. **A configured credential displaces the ambient one.** When a repository has its own
   token, the inherited `GH_TOKEN`, `GITHUB_TOKEN` and their enterprise and config-dir
   companions are removed from the child before the repository's own is added, so the run
   cannot reach past its token to the operator's login. A repository with no credential
   configured behaves exactly as it does today — ambient, as ADR-0012 describes. This is an
   opt-in that makes a repository stricter, never a new way for one to be looser.

6. **This is an HTTPS-remote feature, and the UI says so.** A token does nothing for an
   `origin` of `git@github.com:owner/repo.git`; such a run authenticates with the
   operator's SSH key however many tokens are configured. When the origin is SSH the
   repository settings say plainly that the credential covers `gh` API calls only and the
   push will use the machine's key. Silence here would be the worst outcome available: a
   user believing Rimaia controls an access path it does not.

7. **The value is redacted from everything that persists.** ADR-0013's JSONL transcripts
   and the live run tail are scrubbed of it before write. And the guidance in the UI is to
   provision a **fine-grained token scoped to that one repository**, with the narrowest
   scopes that let a run push a branch and open a pull request, because that — not the
   keychain — is what actually bounds the damage.

`run_environment` and this are independent axes. That setting governs which *Claude Code*
configuration a run inherits; this governs which *forge identity* it acts as. Neither
implies the other, and `strict_local` has never had anything to say about `gh`.

## Consequences

- **The per-repository opt-in in ADR-0012 becomes as narrow as it reads.** Ticking "allow
  unattended agent runs" for one repository stops implicitly lending that run credentials
  for every repository the operator can reach. This is the whole reason for the decision.
- **It is not a sandbox, and nothing here pretends otherwise.** A `bypassPermissions` run
  can read its own environment, so a prompt-injected agent can exfiltrate the token it was
  given. What changes is what a stolen token is worth: one repository's push rights rather
  than an account's. Fine-grained scoping is load-bearing, not advice.
- **Only the GitHub half of ambient credentials is solved.** AWS profiles, npm tokens,
  `~/.aws`, and every other secret in the operator's environment are still inherited by
  every run. ADR-0012's warning stands, minus one clause.
- **A keychain is a new dependency and a new failure mode**: first access can prompt, a
  locked keychain blocks a queue rather than corrupting it, and the Linux path needs a
  running secret service. Fail-closed makes those visible as a refused run naming the
  repository, which is the recoverable version of the failure.
- **Expiry is invisible until it bites.** A PAT that lapsed silently looks exactly like a
  permissions problem at 3am. Task 018's preflight doctor is where a "this repository's
  credential expires in 6 days" check belongs, and the verification call from provisioning
  is the same call.
- **Rotation is manual.** Replace and remove, no scheduled rotation, no vault integration.
  Both are additions this decision does not block.

## Alternatives considered

- **Keep the ambient credential and document it (status quo).** No new dependency, no new
  UI, and the honest position for the MVP. Rejected now for the reason above: a repository
  the user opted in borrows the credentials of every repository they did not.
- **Encrypt the token in SQLite with an application key.** The key lives next to the
  database on the same disk, readable by the same user as the database. It converts a
  plaintext secret into a secret with a decoder ring beside it, which buys a compliance
  sentence and no security.
- **A GitHub App installation token per repository.** Genuinely better — short-lived,
  auto-expiring, scoped by installation rather than by whoever pasted a PAT. It needs an
  app registration, a private key on the user's machine, and something to mint tokens
  against, which is the hosted component and auth boundary ADR-0002 declined. Worth
  revisiting the day any hosted piece exists; the storage seam here does not preclude it,
  because "the credential for this repository" is the same shape either way.
- **One Rimaia-wide token instead of one per repository.** A single token with rights to
  everything is the ambient problem with an extra settings screen.
- **Writing the token into a per-run `GH_CONFIG_DIR`.** Simple, and `gh` reads it without
  any injection logic — but it puts the secret on disk, in a directory next to a worktree
  an unattended agent has arbitrary write access to. The environment variable at least
  stays in the process.
- **Per-task rather than per-repository credentials.** Finer than anything a real
  permission actually is: forge access is granted on repositories, and a task inherits its
  repository's. Two places to configure the same fact, and a task whose credential
  disagrees with its repository's is a bug report waiting to be filed.
