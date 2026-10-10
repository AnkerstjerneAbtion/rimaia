# 30. Identity: people sign in, machines pair, sessions carry tokens

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

Until now the only trust boundary was the loopback interface (ADR-0006): anything that could
reach `127.0.0.1` on the user's machine was the user. A hosted server (ADR-0027) is reached
by four kinds of caller, and each has to prove who it is differently:

| Caller | Where it runs | Needs to prove |
| --- | --- | --- |
| A person in a browser | Anywhere | Which user they are |
| The desktop app | The user's machine | Which user, without asking every launch |
| A runner | The user's machine, or a spare one with nobody at it | Which user it works for, and which runner it is |
| A Claude Code session over MCP | Anywhere | Which user it acts for |

Two things about Rimaia's users narrow the choice. Everyone who uses Rimaia has a GitHub
account, because the product's output is branches and pull requests. And nobody wants
another password.

## Decision

### 1. People sign in with GitHub, behind an identity-provider seam

Sign-in is GitHub OAuth (authorization code with PKCE), requesting the `read:user` scope and
nothing else. The server stores:

- a `users` row keyed by the provider's stable numeric id, never the login, which can be
  renamed;
- the login and avatar, for display.

**Rimaia does not use the sign-in grant to act on GitHub.** It reads nothing and writes
nothing on the user's behalf. Forge credentials for runs stay per repository, per runner and
in the keychain (ADR-0020, ADR-0033). One token for identity and a different token for
pushing keeps a stolen session from being a stolen repository.

The provider sits behind an `IdentityProvider` seam (authorize URL, code exchange, stable
subject id), so a second provider, such as Google or a generic OIDC one, is an addition, not
a rewrite. Only GitHub ships now.

### 2. Browser sessions are server-side, in a cookie

A successful sign-in creates a `sessions` row and sets an opaque session id in a cookie that
is `HttpOnly`, `Secure` and `SameSite=Lax`. Details:

- Sessions expire after 30 days idle, and can be listed and revoked from the account page.
- Requests authenticated by cookie that change anything also need a CSRF token.
- Signing out deletes the row.

Server-side sessions rather than JWTs, because revocation must be immediate. Removing someone
from a team (ADR-0029) has to take effect on their next request, not when a token expires.

### 3. Every non-browser credential is one kind of token

The desktop app, runners and MCP clients all authenticate with a **bearer token** of one
shape:

- **Format:** a random 256-bit secret with a readable prefix naming its kind: `rmd_` desktop,
  `rmr_` runner, `rmp_` personal access token.
- **Storage:** the server stores only a SHA-256 hash of it, plus owner, kind, label, created
  time, last-used time and optional expiry.
- **Management:** every token is listed on the account page, with where it was last used,
  and revoking one takes effect on its next request.

One mechanism with a *kind* field, rather than three mechanisms, because the rules around a
token (hash at rest, show once, revoke immediately, record last use) are the rules that
matter, and they must not be implemented three times.

### 4. The desktop app signs in through the browser

"Connect to a team" opens the system browser on the server's sign-in page, with a loopback
redirect URI (`http://127.0.0.1:<ephemeral>/callback`) and PKCE, as the OAuth guidance for
native apps (RFC 8252) prescribes. The callback delivers a **desktop token**, which the app
stores in the OS keychain through the same `CredentialStore` ADR-0020 introduced. The app
never sees the user's GitHub credentials or the server's session cookie.

### 5. Runners pair, and are bound to one user

A runner is a `runners` row: owner user, label (defaults to the machine's hostname),
provider (ADR-0026), paired time, last-seen time. Its token is a runner token. There are two
ways to get one:

- **Inside a connected desktop app**, the app pairs its own runner automatically, using the
  desktop token. Nothing to do.
- **Headless**, the user creates a one-time pairing code in the web UI. It is valid for ten
  minutes, and `rimaia-runner pair <server> <code>` exchanges it for a runner token. The code
  is short enough to type on a machine with no browser, and it is useless once used or
  expired.

A runner works for exactly one user, forever. Transferring a runner means pairing it again.
What a runner may *run* is ADR-0032's decision. This record only establishes whose runner it
is.

### 6. MCP clients use personal access tokens

A user creates a personal access token in the web UI and registers it:

```
claude mcp add --transport http rimaia https://<server>/mcp \
  --header "Authorization: Bearer rmp_…"
```

The token acts as its user. It may optionally be restricted to a subset of that user's
teams.

A connected desktop app also serves the operator endpoint on loopback, forwarding to the
server as its signed-in user (ADR-0035). **Once connected, that endpoint requires a personal
access token as well.** Unauthenticated loopback was safe when loopback reached only this
machine's board. Connected, it would reach every team the user belongs to, and anything on
the machine could use it, including an unattended run. The app offers to update the existing
Claude Code registration with the token in one step, so connecting costs the user one
confirmation, not a manual edit.

### 7. Solo mode has no sign-in

The implicit solo user (ADR-0029) is the only user. The loopback boundary is still the trust
boundary, exactly as ADR-0006 describes. Nothing in this record is reachable in solo mode.

### 8. Who did what is recorded

ADR-0019 recorded which *door* a write came through. The server also records *who*:

- `tasks.created_by` and `tasks.plan_updated_by` (the latter is load-bearing for ADR-0032);
- `tasks.assigned_by`;
- `runs.runner_id` and, through the runner, the user whose subscription paid.

The acting user travels on `ServiceContext` beside `source` and the team scope (ADR-0029
records why that struct may grow here). The tracing span gains `user_id` beside `source`.
`tasks.source` is unchanged and still means creation provenance.

## Consequences

- **No passwords, no email sending, no account recovery flow.** A user who loses GitHub
  access loses Rimaia access. For this audience that is an acceptable dependency.
- **Every credential is revocable from one page.** A lost laptop is handled by revoking its
  desktop and runner tokens.
- **The server is now a target.** Rate limits on the sign-in, pairing and token endpoints,
  constant-time hash comparison and secure cookie attributes are part of this feature, not
  hardening for later.
- **Seam-contract D8's closed `ErrorCode` gets its first additions since the MVP**:
  `Unauthenticated` (sign in again) and `UpgradeRequired` (ADR-0037). ADR-0031 adds
  `Conflict`. Each is a different action a client takes, which is D8's own test for when a
  code is worth having.
- **GitHub is a single point of failure for signing in, not for working.** Existing sessions
  and tokens keep working through a GitHub outage. Only new sign-ins wait.
- **ADR-0020's rejected alternative becomes possible.** A GitHub App issuing short-lived
  installation tokens needed "something to mint tokens against", which now exists. Not decided
  here. The per-repository credential seam does not block it.

## Alternatives considered

- **Email magic links.** No third-party dependency for identity. Needs an email sender,
  deliverability and a verified-address model, for users who all have GitHub accounts anyway.
- **Passwords.** Rejected without much debate: storage, reset flows and breach liability, to
  duplicate what GitHub already verifies.
- **JWT access tokens instead of server-side sessions and hashed tokens.** Stateless
  verification, which one SQLite-backed server does not need. And immediate revocation, which
  it does need, is exactly what JWTs make awkward.
- **Use the GitHub OAuth token for runs too.** One login for everything. Rejected because it
  concentrates push rights for every repository the user can reach into a credential the
  *server* holds. That is ADR-0020's ambient-credential problem, moved to a place with more
  attackers.
- **Runners authenticate as their user, with no runner identity.** Simpler. It loses the
  ability to revoke one machine, to see which machine holds a worktree, and to fence a lease
  to the runner that owns it (ADR-0031).
