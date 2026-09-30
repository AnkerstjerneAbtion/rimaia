---
id: "047"
title: "Identity: sign-in, sessions and tokens"
milestone: v0.5
status: ready
depends_on: ["046"]
adrs: ["0030", "0029", "0034", "0037"]
size: L
---

# Identity: sign-in, sessions and tokens

## Goal

Make the server's `Caller` real. After task 046 every board route takes a `Caller` as its
first extractor, and the only production `Authenticate` is `RefuseAll`, so the server
refuses everything (D32 point 7). After this task it lets in exactly the callers ADR-0030
describes and nobody else:

- **a person in a browser**, signed in with GitHub (authorization code with PKCE, scope
  `read:user`), holding a server-side session in an `HttpOnly`, `Secure`, `SameSite=Lax`
  cookie and sending a CSRF token on every cookie request;
- **a desktop app**, holding an `rmd_` token it got through the browser with a loopback
  redirect (ADR-0030 point 4). The server half of that exchange is this task's; the app's
  half is task 059's;
- **a runner**, holding an `rmr_` token it got by redeeming a ten-minute, single-use pairing
  code, or from a connected desktop that pairs its own runner (ADR-0030 point 5);
- **an MCP client**, holding an `rmp_` personal access token, optionally restricted to some
  of its user's teams (ADR-0030 point 6).

Every one of those credentials is stored as a SHA-256 hash, is listed on an account API the
web app (050) renders, and stops working on the next request after it is revoked. The
sign-in, pairing and token-minting endpoints are rate limited. Every authentication failure
is the same `unauthenticated` answer.

This task builds the server side and the account API. It builds no screen: the sign-in page
and the account page are 050's, and the desktop's browser sign-in is 059's.

## Why now

046 deliberately shipped a server that refuses everything, so that no route could ever exist
without a caller (D32's Why, point 7). Nothing after it can be exercised over HTTP until a
caller can be authenticated: 048's event stream filters by the caller's teams, 049's
transport needs a cookie and a CSRF header to send, 050's sign-in page needs something to
sign in to, 051's invitations need a signed-in user to accept them, and 052's runner
protocol resolves an `rmr_` token to a runner. This is the task all of M3 and M4 wait on.

ADR-0030's Consequences also say the rate limits, the constant-time comparisons and the
cookie attributes are "part of this feature, not hardening for later". They land here
because a server that authenticates without them is a server that should not be deployed,
and 062 deploys it.

## Scope

**1. The migration.** `src-tauri/migrations/20261003120300_identity.sql`, with D28 part 6's
DDL exactly: `sessions`, `api_tokens`, `api_token_teams`, `pairing_codes` and their indexes.
The header comment is this task's, in the voice of the existing migrations: why only hashes
are stored, why a revoked credential is a deleted row, and why the OAuth `state`, the PKCE
verifier and the desktop's one-time code have no table (D28, "Sign-in state stays in
memory"). It is additive. Nothing else in the board schema changes.

**2. Secrets, in one module.** New: `crates/core/src/identity/secret.rs`. Every rule about a
secret lives here and nowhere else, so it is implemented once (ADR-0030 point 3's reason for
one token mechanism):

- `TokenKind { Desktop, Runner, Personal }`, with `prefix()` (`rmd_`, `rmr_`, `rmp_`) and
  `as_str()` in `api_tokens.kind`'s `CHECK` spelling. An enum, not a string (CLAUDE.md).
- `mint_token(kind) -> Secret`: the prefix followed by `BASE64URL_NOPAD` of 32 bytes from
  `rand`'s OS-seeded CSPRNG (D34).
- `mint_session_secret() -> Secret`: the same 32 bytes and encoding, with no prefix.
- `hash(secret) -> String`: lowercase hex SHA-256 of the whole string, prefix included, as
  D28 specifies for `secret_hash` and `code_hash`.
- `csrf_for(session_secret) -> String`: `BASE64URL_NOPAD(SHA256("rimaia-csrf-v1:" ||
  secret))`. This is how "047 derives the token from the session secret" (D28, D34): nothing
  stores it, the server recomputes it from the cookie, and knowing it reveals nothing about
  the secret. Compared against the header with `subtle::ConstantTimeEq`.
- `pkce_challenge(verifier) -> String`: `BASE64URL_NOPAD(SHA256(verifier))`, the whole of
  hand-rolled PKCE (D34), and `mint_verifier()`: 32 CSPRNG bytes, base64url, 43 characters,
  inside RFC 7636's 43–128 range.
- `mint_pairing_code() -> Secret`: eight characters drawn uniformly from Crockford's base32
  alphabet (40 bits), displayed as `XXXX-XXXX`. `normalize_pairing_code(input)` uppercases,
  drops hyphens and spaces, and maps `O` to `0` and `I`/`L` to `1`, so a code typed on a
  machine with no browser is forgiven the mistakes Crockford's alphabet exists to forgive.
- `Secret` is a newtype whose `Debug` prints `Secret(…)` and never the value, and which has no
  `Display`. Every type in this task that holds a secret, a hash or a one-time code gets the
  same redacting `Debug`.

`sha2`, `rand` and `subtle` are added to `rimaia-core` as D34's table says: workspace lines
`sha2 = "0.10"`, `rand = "0.10"`, `subtle = "2"`, referenced with `{ workspace = true }`.

**3. The identity-provider seam (ADR-0030 point 1).** New:
`crates/core/src/identity/provider.rs`.

```rust
pub trait IdentityProvider: Send + Sync + 'static {
    /// `users.identity_provider`: "github".
    fn id(&self) -> &'static str;
    fn authorize_url(&self, state: &str, code_challenge: &str, redirect_uri: &str) -> String;
    /// Exchanges the code, reads the stable subject, and drops the provider's access token
    /// before returning. Nothing the provider issues is stored or returned.
    fn exchange<'a>(&'a self, code: &'a str, code_verifier: &'a str, redirect_uri: &'a str)
        -> BoxFuture<'a, Result<ProviderIdentity>>;
}

pub struct ProviderIdentity { pub subject: String, pub login: String,
                              pub avatar_url: Option<String> }
```

- **The trait is in core, the GitHub implementation is in the server.** GitHub needs HTTPS,
  and D34 gives reqwest's `rustls` feature to `rimaia-server`, not to core, whose `reqwest`
  stays plain HTTP to `127.0.0.1` for `mcp::probe`. `BoxFuture` is D32 point 1's local alias.
- **`crates/server/src/github.rs`: `GitHub { client_id, client_secret, endpoints, http }`.**
  The authorize URL is `https://github.com/login/oauth/authorize` with `client_id`,
  `redirect_uri`, `scope=read:user`, `state`, `code_challenge` and
  `code_challenge_method=S256`, in that order, built with `reqwest::Url::parse_with_params`.
  The exchange is two requests: `POST https://github.com/login/oauth/access_token` with
  `Content-Type: application/json`, `Accept: application/json`, and a body built with
  `serde_json::to_vec` carrying `client_id`, `client_secret`, `code`, `redirect_uri` and
  `code_verifier`; then `GET https://api.github.com/user` with that token and a
  `User-Agent`. D34 approves neither reqwest's `json` nor its `form` feature ("Request bodies
  are built with `serde_json::to_vec` and an explicit `Content-Type`"), so neither
  `.json()` nor `.form()` is available, and the answers are read as bytes and parsed with
  `serde_json::from_slice`. The subject is the numeric `id` as a decimal string, never the `login`. A
  missing or non-numeric `id` is `Error::internal`. `endpoints` exists so the tests can point
  both requests at a loopback stub; production uses GitHub's.
- **Rimaia does not keep the GitHub token.** It is a local in `exchange` and is dropped when
  `exchange` returns. There is no column for it and no field that could carry it out.
- **`testing::identity::FakeIdentityProvider`**, behind the `testing` feature: the id
  `fake`, an authorize URL of `https://idp.test/authorize?state=<state>&challenge=<c>`, and
  an `exchange` that answers from a table of `code → ProviderIdentity` the test fills. It
  also records the verifier it was given, so a test can check PKCE end to end.

**4. Signing in creates or finds a user.** `identity::sign_in::upsert_user(pool, clock,
provider_id, identity) -> UserId`, in one transaction:

- **Known `(identity_provider, provider_subject)`:** update `login` and `avatar_url`, which a
  rename on GitHub changes, and return the id.
- **Unknown:** call 038's `identity::create_personal_team(conn, clock, login)`, which writes
  the user, their personal team, the owner membership and the seeded `base_instructions`
  (ADR-0029 point 2), then set `identity_provider`, `provider_subject` and `avatar_url` on
  that user in the same transaction. This is the sign-up D28 part 3 says reuses 038's
  service.

**5. Sessions (ADR-0030 point 2).** `crates/core/src/identity/sessions.rs`.

- `create(pool, clock, user_id, user_agent) -> (SessionId, Secret)`: a new row with a new
  secret on every sign-in, so no session id is ever accepted from the client (no fixation).
  `user_agent` is the request's header, truncated to 256 characters. In the same
  transaction it deletes the user's sessions that are past the idle rule, so a session
  abandoned on a machine nobody returns to is gone by the user's next sign-in instead of
  riding in every backup (ADR-0037 point 2).
- **Idle expiry is 30 days from `last_used_at`.** A session is valid while `now -
  last_used_at < 30 days`. A request on an expired session is refused and deletes the row.
  `list_sessions` applies the same rule as a filter, so an expired session that has not
  been presented again is not shown as live.
- **`last_used_at` is written at most once an hour.** A request whose session was touched
  less than an hour ago writes nothing. Every write reaches the backup stream (ADR-0037
  point 2), and an hour's precision on a thirty-day rule costs nothing.
- Signing out and revoking delete the row. A deleted session's cookie is an unknown secret.

**6. API tokens (ADR-0030 points 3, 5 and 6).** `crates/core/src/identity/tokens.rs`.

- `create_personal(ctx, label, team_ids, expires_in_days) -> NewApiToken`. The label is
  trimmed and must be 1–100 characters. Every id in `team_ids` must be a team the caller
  belongs to; one that is not is `not_found`, never a sentence that confirms it exists
  (ADR-0029 point 5). An empty list writes no `api_token_teams` rows and means every team
  the user belongs to, now and later. `expires_in_days`, if present, is 1–365.
- `create_desktop(pool, clock, user_id, label)` and `create_runner(conn, clock, user_id,
  runner_id, label)`: no expiry. A runner token is always written in the same transaction as
  its `runners` row.
- **Last use** is `last_used_at` and `last_used_from`, which is the request's `User-Agent`
  truncated to 256 characters. Written under the same once-an-hour rule as sessions, or at
  once when `last_used_from` changes. A runner heartbeating every minute (053) must not be a
  write a minute. The User-Agent is this task's reading of ADR-0030 point 3's "where it was
  last used", and the address is deliberately not it: behind 062's proxy the peer address
  is the proxy's until a trusted-hop setting exists, so it would say the same thing for
  every token, and a client IP is personal data that ADR-0037 would carry into every
  backup. If an address is wanted later, it is 062's to add, with that setting.
- `NewApiToken { token: Secret, summary: ApiTokenSummary }`. The secret leaves the server
  exactly once, in the answer to the call that minted it.
- **`unpair_runner(conn, clock, runner_id)`** deletes the runner's tokens and sets
  `runners.unpaired_at`, in one transaction. Revoking a runner-kind token goes through it, so
  "revoke the runner token" and "unpair the runner" are one act and cannot disagree. The row
  stays, because runs keep naming it (D28). 057 adds the release of the runner's pins to this
  same function.

**7. Pairing (ADR-0030 point 5).** `crates/core/src/identity/pairing.rs`.

- `create_code(ctx) -> PairingCode { code, expires_at }`: ten minutes, only the hash stored.
- `redeem(pool, clock, code, label, provider) -> PairedRunner { runner_id, token }`: one
  transaction that deletes the code row by its hash with `RETURNING`, refuses when nothing
  came back or `expires_at` has passed, then writes the `runners` row (owner, label, the
  provider as `ProviderId::as_str()`, `paired_at`) and its `rmr_` token. A code is useless
  once used, whether or not the redemption that used it succeeded. `provider` must parse as a
  `ProviderId`; add `ProviderId::parse` if no earlier task has.
- `pair_own_runner(ctx, label, provider) -> PairedRunner`: the connected desktop's automatic
  pairing, with no code. Only a `Door::Desktop` caller may use it (Scope 12).
- An unknown, used and expired code all get the same `invalid` sentence, so the answer is not
  an oracle for which codes exist.

**8. In-memory sign-in state.** `crates/core/src/identity/pending.rs`, held by the server,
never written to the database (D28):

- `PendingSignIns`: `state → { verifier, flow, created_at }`, where `flow` is `Browser` or
  `Desktop { redirect_uri, desktop_state, desktop_challenge, label }`. `take(state)` removes
  the entry, so a state is single use, and refuses one older than ten minutes by the
  injected `Clock`.
- `DesktopCodes`: `hash(code) → { user_id, desktop_challenge, redirect_uri, label,
  created_at }`, ten minutes, single use.
- Both prune expired entries whenever they are written, so neither grows without bound. A
  server restart empties both, which costs a user mid-sign-in one click (D28).

**9. Rate limits (ADR-0030 Consequences, D34).** `crates/core/src/identity/rate_limit.rs`:
a fixed-window counter keyed by `(Bucket, key)`, reading the injected `Clock`. A window
starts at a key's first hit and resets when it has passed. Expired windows are pruned on
write.

| Bucket | Where | Key | Limit |
| --- | --- | --- | --- |
| `SignIn` | `GET /auth/github`, `GET /auth/desktop`, `GET /auth/github/callback` | peer address | 30 per 10 minutes |
| `DesktopToken` | `POST /api/v1/auth/desktop_token` | peer address | 10 per 10 minutes |
| `Pair` | `POST /api/v1/auth/pair` | peer address | 10 per 10 minutes |
| `MintToken` | `create_personal_access_token`, `create_pairing_code`, `pair_own_runner` | user id | 20 per hour |

- **A refusal is `Error::invalid`**, with the message `Too many attempts. Try again in
  {n} seconds.`, where `n` is the whole seconds left in the window, rounded up. D8 admits no
  new code for it: the action the client takes is to wait, and the sentence says how long.
  Its HTTP status is therefore 400, by D32 point 3's table. A dedicated 429 would be a D8 and
  D32 amendment, which this task does not make.
- **The peer address is axum's `ConnectInfo<SocketAddr>`.** Behind 062's reverse proxy every
  request has the proxy's address, and the per-address limit becomes one limit for the
  whole instance. That is still a limit, and fails closed. Reading a forwarded header safely
  needs to know which proxy to trust, which is 062's decision. 047 reads no forwarded header.
- **One limiter, shared.** The server builds one `Arc<RateLimiter>` and gives it both to
  `ServerState`, for the auth routes, and to `BoardHost`, for the three minting commands.
  `BoardHost` gains `rate_limits` here, under D32 point 2's rule that it grows only through
  the task that needs a field. The solo shell builds one too, which nothing in solo reaches.

**10. `Authenticate`, for real (D32 point 7).** `crates/core/src/identity/authenticate.rs`:
`SessionsAndTokens { pool, clock }` implements `api::caller::Authenticate`, and replaces
`RefuseAll` in the server's production wiring. `RefuseAll` stays for tests that want it.

- **`Credential::Session { secret, csrf }`:** look up `hash(secret)`, apply idle expiry,
  compare `csrf` with `csrf_for(secret)` in constant time, touch, and answer `Door::Browser {
  session_id }`. A missing CSRF header is a failure, on reads as well as writes (D32's Why).
- **`Credential::Bearer(token)`:** the prefix names the kind; look up `hash(token)`; refuse
  when the row's `kind` disagrees with the prefix, when `expires_at` has passed, or, for a
  runner token, when the runner has `unpaired_at`. Touch, and answer `Door::Desktop {
  token_id }` for `rmd_`, `Door::Mcp { token_id: Some(id) }` for `rmp_`, and
  `Door::Runner { runner_id }` for `rmr_`.
- **`Caller.teams` is read from `team_memberships` on this request**, never from a cache on
  the session or the token (D32 point 7). For a personal access token with
  `api_token_teams` rows, it is the intersection of those rows with the user's current
  memberships. Removing someone from a team (051) therefore takes effect on their next
  request, which is the reason ADR-0030 chose server-side sessions over JWTs.
- **Every failure is `Error::unauthenticated()`, with one message: `Sign in to continue.`**
  Unknown, expired, revoked, wrong kind, unpaired, missing CSRF and wrong CSRF are
  indistinguishable to the caller. The span records which one it was, never the credential.
- **A hash lookup is not a timing oracle for the secret.** The database compares hashes of
  256-bit secrets, and learning a prefix of a hash teaches nothing about the secret. `subtle`
  is used wherever a secret itself is compared in memory: the CSRF header, the OAuth `state`
  against the sign-in cookie, and the desktop's PKCE challenge. D34 also names the pairing
  code's hash; with that hash as the table's primary key there is no in-memory comparison
  left to make, and the code says so in a comment rather than adding one for show.

046 added `ErrorCode::Unauthenticated` and its `src/types.ts` twin (D32 point 3). This task
adds `Error::unauthenticated()` with the one message above and is the first production code
that returns it. If 046 did not add the variant, add it here, with its TypeScript twin.

**11. The server's half.** In `crates/server`:

- **`caller.rs` gains its cookie half** (D32 point 7). It reads `rimaia_session` with the
  `X-Rimaia-CSRF` header, or `Authorization: Bearer`, and refuses a request that carries
  both. Cookies are read and set with `axum-extra`'s `CookieJar`, feature `cookie` only
  (D34); `cargo tree -d` still shows one `axum`.
- **Board routes accept `Door::Browser` and `Door::Desktop` only** (D32 point 7's table). An
  `rmp_` or `rmr_` token on `POST /api/v1/<board command>` is `unauthenticated`. `/mcp`
  (060) and `/api/v1/runner/*` (052) are their tasks' to mount, over the same
  `SessionsAndTokens`.
- **Routes outside the registry.** They are not board commands: they run before there is a
  caller, or they must set or clear a cookie, which `dispatch` cannot.

  | Route | Does |
  | --- | --- |
  | `GET /auth/github` | Browser sign-in. Mints `state` and a verifier, records a `Browser` pending entry, sets `rimaia_signin` = `state`, and redirects to the provider |
  | `GET /auth/desktop?redirect_uri&state&code_challenge&code_challenge_method` | Desktop sign-in (ADR-0030 point 4). `redirect_uri` must be `http://127.0.0.1:<port>/callback` (RFC 8252 §7.3) and the method `S256`; anything else is refused before the provider is contacted. Otherwise as above, with a `Desktop` entry |
  | `GET /auth/github/callback?code&state` | Compares `state` with `rimaia_signin` in constant time, takes the pending entry, exchanges, upserts the user. `Browser`: creates a session, sets the cookies, clears `rimaia_signin`, redirects to `/`. `Desktop`: mints a one-time code into `DesktopCodes` and redirects to `redirect_uri?code=…&state=<desktop_state>`, creating no browser session |
  | `POST /api/v1/auth/desktop_token` `{ code, codeVerifier, redirectUri }` | Takes the code, checks `pkce_challenge(codeVerifier)` against the stored challenge in constant time and the redirect URI for equality, and answers `NewApiToken` for an `rmd_` token labelled as `/auth/desktop` was asked |
  | `POST /api/v1/auth/pair` `{ code, label, provider }` | Scope 7's `redeem`. Answers `PairedRunner`. This is the call 058's `rimaia-runner pair` makes |
  | `POST /api/v1/auth/sign_out` | Cookie and CSRF required. Deletes the session and clears both cookies. Answers `null` |

  The JSON routes use 046's error shape and status table, parse the body themselves as 046's
  board routes do, and apply 046's protocol check as a `Write` board row does. The three
  browser-navigated `GET`s cannot carry a header and do not check it. A failed callback,
  including a user who declines at GitHub, redirects to `/?sign_in=denied`,
  `/?sign_in=expired` or `/?sign_in=failed`, which is the whole vocabulary 050 renders.
  Nothing is echoed from the query string into a page.
- **The cookies.** All are `Secure` and `Path=/`, and none sets `Domain`.

  | Cookie | Value | `HttpOnly` | `SameSite` | `Max-Age` |
  | --- | --- | --- | --- | --- |
  | `rimaia_session` | the session secret | yes | `Lax` | 400 days, the browser maximum; the server's idle rule is the expiry that counts |
  | `rimaia_csrf` | `csrf_for(secret)` | **no**: 049's transport reads it and sends it as `X-Rimaia-CSRF` | `Lax` | as `rimaia_session` |
  | `rimaia_signin` | the OAuth `state` | yes | `Lax` | 600 |

  This is how the web app obtains the CSRF token, which D32 left to 047: a readable cookie
  holding a value derived from the unreadable one. A page on another origin can make the
  browser send the session cookie, but cannot read `rimaia_csrf` to send the header with it.
  `rimaia_signin` is what refuses a callback started in someone else's browser (login CSRF).
- **Configuration.** Three variables join the ones 046 reads, each named in the startup
  refusal when missing (D11): `RIMAIA_PUBLIC_URL` (the origin; the callback is
  `<origin>/auth/github/callback`), `RIMAIA_GITHUB_CLIENT_ID` and
  `RIMAIA_GITHUB_CLIENT_SECRET`. The config struct's `Debug` redacts the secret.
- **`reqwest`'s TLS feature is enabled at `rimaia-server`'s use site**, spelled `rustls` on
  the pinned 0.13 (D34), never on the workspace line. D34 asks this task to confirm all three
  CI runners build `aws-lc-sys` in the same commit. If any needs a system package, stop and
  ask, as `libdbus` was for `keyring`.
- **Nothing secret reaches a log line.** 046's trace layer records the matched route, not the
  URI, so the callback's `code` and `state` are not recorded. No `tracing` field in this task
  holds a secret, a hash, a code, a cookie or a header value. The span of every
  authenticated request carries `user_id` and the door (ADR-0030 point 8), which 046's
  `command` span already does.

**12. The account API: eight board rows (D32).** Handlers in
`crates/core/src/api/board/account.rs`, registered in `api/registry.rs`, each with a
`board<T>` wrapper in `src/lib/commands.ts` and its types in `src/types.ts`:

| Command | Effect | Does |
| --- | --- | --- |
| `get_account` | Read | The user (`id`, `login`, `avatarUrl`), their teams with name and role, and which credential this request used |
| `list_sessions` | Read | The user's live sessions: `id`, `userAgent`, `createdAt`, `lastUsedAt`, `current` |
| `revoke_session` | Write | `{ id }`. Deletes one of the caller's sessions |
| `list_api_tokens` | Read | `ApiTokenSummary`: `id`, `kind`, `label`, `runnerId`, `teamIds`, `createdAt`, `lastUsedAt`, `lastUsedFrom`, `expiresAt`, `current`. Never a hash |
| `create_personal_access_token` | Write | `{ label, teamIds?, expiresInDays? }` → `NewApiToken` |
| `revoke_api_token` | Write | `{ id }`. A runner token unpairs its runner (Scope 6) |
| `create_pairing_code` | Write | → `PairingCode` |
| `pair_own_runner` | Write | `{ label, provider }` → `PairedRunner`. `Door::Desktop` only; any other door is `invalid`, naming the desktop app |

- **They are the caller's own, and the handler reads the door in core** (D32 point 7's own
  example). Another user's session or token id is `not_found`. `current` is computed from
  `request.caller.door`.
- **Solo has no accounts** (ADR-0030 point 7). A `Door::Shell` caller is refused with
  `invalid`: `Accounts exist only on a Rimaia server.` The rows still exist in solo, because
  the registry is one list and the wiring script requires a wrapper for each.
- **They publish no `ChangeEvent`.** Sessions and tokens belong to a user, not a team, and
  `Change` has no variant for them. A second open tab sees a revocation on its next read.
- **046's registry test holds them to the same bar as every other row.** Each gets a case in
  `crates/server/tests/commands.rs`, run as user A against user B's ids, where every answer
  must be `not_found`, and each `Read` publishes nothing.

**13. Documentation.** CLAUDE.md's Gotchas gains one bullet: no secret goes into a log
line, a `Debug` output, an error message or a URL the server records; tokens, session
secrets, pairing codes, OAuth codes and `state` are compared through
`identity::secret` and stored only as its hashes. The server's three new environment
variables are listed wherever 046 documented its own.

## Out of scope

- **Every screen.** The sign-in page, the account page, and the `?sign_in=` messages are
  050's. The HTTP transport that reads `rimaia_csrf`, and the handling of `unauthenticated`
  as "show sign-in", are 049's and 050's.
- **The desktop's half of sign-in**: the loopback listener, the keychain, the mode chooser,
  and the one-step Claude Code re-registration with a personal access token. All 059's.
- **The headless binary** that calls `/api/v1/auth/pair`: 058.
- **`RunnerCaller` and the runner protocol's routes**: 052. **`/mcp` on the server** and its
  use of `SessionsAndTokens`: 060.
- **Teams, invitations, roles, and deleting an account.** 051. Deleting a user cascades to
  its sessions, tokens and codes through D28's foreign keys, and 051 owns the service.
- **Recording who did what** on `tasks` and `runs` (ADR-0030 point 8's columns): 045 and
  052. The span's `user_id` is 038's.
- **A forwarded client address** for the rate limiter behind a proxy: 062.
- **Who may sign up.** Signing in with any GitHub account creates a user and a personal team,
  which is ADR-0029 point 2's "signing up creates a personal team". Whether the hosted
  instance should admit only some accounts is not decided by any ADR. This task does not add
  an allowlist; see Notes.
- **A second identity provider.** The seam allows one; only GitHub ships (ADR-0030 point 1).
- **Any use of the GitHub grant beyond reading the user.** Forge credentials for runs stay
  per repository and per runner (ADR-0020, ADR-0033).

## Acceptance criteria

- `src-tauri/migrations/20261003120300_identity.sql` exists under exactly that name with D28
  part 6's DDL, and is the only new migration. Both offline query caches are regenerated with
  D33's recipe and committed, and `SQLX_OFFLINE=true cargo check --workspace --all-targets`
  passes with them.
- `sha2`, `rand` and `subtle` are workspace lines used only by `rimaia-core`; `axum-extra`
  (feature `cookie`) is used only by `rimaia-server`; reqwest's `rustls` feature is enabled
  in `rimaia-server`'s manifest and not on the workspace line; `cargo tree -d` shows one
  `axum`. No other dependency is added, in either ecosystem.
- These tests exist in `rimaia-core` and pass, using `TestClock` for every time, with no
  `sleep` and no mocked store:
  - `a_token_carries_the_prefix_of_its_kind_and_256_bits_after_it`;
  - `a_stored_hash_is_the_hex_sha256_of_the_whole_token`;
  - `the_pkce_challenge_matches_rfc_7636_appendix_b`: verifier
    `dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk` gives exactly
    `E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuEJSstw-cM`;
  - `the_csrf_token_is_derived_from_the_session_secret_and_is_neither_it_nor_its_hash`;
  - `a_typed_pairing_code_is_forgiven_case_hyphens_and_lookalikes`;
  - `no_secret_type_prints_its_value_in_debug`, over `Secret`, `NewApiToken`,
    `PairedRunner`, `PairingCode`, the pending entries and the server config;
  - `a_second_sign_in_with_the_same_subject_finds_the_same_user_and_takes_the_new_login`;
  - `a_first_sign_in_creates_a_user_a_personal_team_and_an_owner_membership`;
  - `a_session_expires_after_thirty_idle_days_and_its_row_is_deleted`, which passes at 30
    days less one second after the last touch and fails at exactly 30 days;
  - `a_session_is_touched_at_most_once_an_hour`;
  - `a_revoked_session_is_refused_on_its_next_request`;
  - `a_cookie_request_without_the_csrf_header_is_refused_even_for_a_read`, and the same
    with a wrong header;
  - `each_token_kind_answers_its_own_door`: `rmd_` is `Desktop`, `rmp_` is `Mcp` with its
    id, `rmr_` is `Runner` with its runner;
  - `a_token_whose_prefix_disagrees_with_its_row_is_refused`;
  - `an_expired_personal_access_token_is_refused`;
  - `a_restricted_personal_access_token_sees_only_the_teams_it_names_and_still_belongs_to`;
  - `removing_a_membership_takes_effect_on_the_next_request`;
  - `revoking_a_runner_token_unpairs_its_runner_and_keeps_the_row`;
  - `a_pairing_code_redeems_once_within_ten_minutes`, and
    `an_unknown_a_used_and_an_expired_code_get_the_same_answer`;
  - `a_pending_sign_in_is_single_use_and_expires_after_ten_minutes`, for both maps;
  - `a_rate_limit_refuses_the_attempt_after_its_limit_and_resets_with_the_window`, with
    the exact message `Too many attempts. Try again in 600 seconds.` for the eleventh `Pair`
    attempt at the window's start;
  - `every_authentication_failure_says_the_same_thing`: every failure above serializes to
    exactly `{"code":"unauthenticated","message":"Sign in to continue."}`;
  - `account_commands_refuse_the_solo_shell`, for all eight rows, with the exact message.
- These tests exist in `crates/server/tests/` and pass, against the real router on
  `127.0.0.1:0`, a temporary board database, `TestClock` and `FakeIdentityProvider`, with
  `reqwest` following no redirects:
  - `a_browser_signs_in_and_calls_a_board_command`: `/auth/github` redirects to the fake
    provider with a `state` and an S256 challenge and sets `rimaia_signin`; the callback
    sets `rimaia_session` and `rimaia_csrf` with exactly the attributes in Scope 11's
    table; a board command with the cookie and the header succeeds, and the fake provider
    saw the verifier whose challenge was sent;
  - `a_callback_without_its_sign_in_cookie_is_refused` (login CSRF), and
    `a_callback_state_is_single_use` and `a_callback_after_ten_minutes_is_refused`;
  - `sign_out_deletes_the_session_and_clears_both_cookies`;
  - `the_desktop_signs_in_through_a_loopback_redirect`: the redirect carries a code and the
    desktop's own `state`; `desktop_token` with the right verifier answers an `rmd_` token
    that authenticates as `Desktop`; the same code twice, a wrong verifier, and a different
    redirect URI are each refused;
  - `a_desktop_redirect_that_is_not_loopback_is_refused_before_the_provider`, for
    `https://`, `localhost`, another host, and a missing `S256`;
  - `a_headless_runner_pairs_with_a_code`: `create_pairing_code` over a session, then
    `/api/v1/auth/pair` answers a runner id and an `rmr_` token that authenticates as that
    `Runner`; a second redemption is refused;
  - `only_browser_and_desktop_may_call_a_board_route`: an `rmp_` and an `rmr_` token each
    get `401 unauthenticated` with `WWW-Authenticate: Bearer`;
  - `a_request_with_both_a_cookie_and_a_bearer_token_is_refused`;
  - `the_sign_in_endpoints_are_rate_limited_per_peer`, reaching each auth route's limit and
    passing again after `TestClock` crosses the window;
  - `a_minted_secret_appears_in_exactly_one_response`: after creating a personal access
    token, `list_api_tokens` and `get_account` contain neither the token nor its hash;
  - the eight account rows each have a case in 046's `crates/server/tests/commands.rs`, so
    `every_board_command_has_a_case`, `a_team_cannot_see_another_teams_ids` (run here as
    another user's ids) and `both_transports_answer_every_case_identically` cover them.
- `github_builds_the_authorize_url_github_documents`: for client id `Iv1.test`, redirect
  `https://rimaia.test/auth/github/callback`, state `state-1` and the RFC 7636 challenge
  above, `authorize_url` is exactly
  `https://github.com/login/oauth/authorize?client_id=Iv1.test&redirect_uri=https%3A%2F%2Frimaia.test%2Fauth%2Fgithub%2Fcallback&scope=read%3Auser&state=state-1&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuEJSstw-cM&code_challenge_method=S256`.
- `github_exchanges_a_code_for_the_numeric_subject_and_keeps_no_token`: against a loopback
  stub serving GitHub's documented token and `/user` shapes, the exchange sends the verifier,
  the client secret and `Accept: application/json`, and returns the numeric `id` as the
  subject; a `/user` answer without a numeric `id` is `internal`.
- `src/lib/commands.ts` has a `board<T>` wrapper for each account row, `src/types.ts` its
  types, and `src/lib/commands.test.ts` asserts each wrapper's exact invoke name and payload,
  mocked at `@tauri-apps/api/core` as the file already does. `./scripts/check-command-wiring.sh`
  passes. No component changes.
- The server refuses to start without `RIMAIA_PUBLIC_URL`, `RIMAIA_GITHUB_CLIENT_ID` or
  `RIMAIA_GITHUB_CLIENT_SECRET`, naming the missing variable, and never printing the secret.
- `grep -rn 'RefuseAll' crates/server/src` finds no production wiring.
- CLAUDE.md carries the Gotchas bullet from Scope 13.
- Every CI check passes on all three operating systems, `aws-lc-sys` included, with the exact
  command list CLAUDE.md and `ci.yml` share after 046.
- **Needs a person, and the PR body carries it as a checklist:** with a real GitHub OAuth app
  (client id and secret supplied by a person) and `RIMAIA_PUBLIC_URL=http://localhost:<port>`,
  sign in from a real browser; sign out; sign in again and see two sessions on
  `list_sessions`; create and revoke a personal access token with `curl`; confirm that the
  GitHub app's authorization page asks for nothing beyond reading the profile.

## Notes

**Read first.** ADR-0030 in full; it is this task's only ADR, and every point in it but 7
and 8 lands here or names the task that finishes it. Then seam-contract D28: the
`20261003120300_identity.sql` DDL, its lead-in paragraph, and "Sign-in state stays in memory"
in its Why. D32 points 2, 3, 7 and 9 and its Binds line for 047 (`Authenticate`, the cookie
and header names, the doors each surface accepts, CSRF on every cookie request). D33 point 3,
the two-cache recipe. D34's rows for `sha2`, `rand`, `subtle`, `axum-extra` and reqwest's TLS
feature, and its "Hand-written" paragraph for PKCE and rate limiting. D8 (no rate-limit
code), D10 (ids are UUID strings), D11 (the server's startup refusal), D4's amendment (the
file name, and that it is frozen once this lands). D6 and D34 as prohibitions. ADR-0029
points 2 and 5 for sign-up and not-found; ADR-0037 points 2 and 6 for why nothing secret is
written to a table or a log it does not need to be in.

**Files to start from.** On `main` today: `crates/core/src/error.rs` (the constructor goes
beside `invalid`), `crates/core/src/clock.rs` and `crates/core/src/testing/clock.rs`,
`crates/core/src/testing/mod.rs` (register `testing::identity`), `crates/core/src/mcp/mod.rs`
(`RunHandles` mints and revokes per-run tokens: the nearest precedent for a secret that lives
in memory and dies on time), `crates/core/src/credentials/redact.rs` (how the codebase keeps
a secret out of output), `crates/core/src/runner/provider/mod.rs` (`ProviderId`),
`src/lib/commands.ts`, `src/lib/commands.test.ts`, `src/types.ts`,
`scripts/check-command-wiring.sh`, `Cargo.toml`, `crates/core/Cargo.toml`,
`.github/workflows/ci.yml`. From earlier tasks on this branch:
`crates/core/src/identity/mod.rs` (038: `create_personal_team`, `Role`),
`crates/core/src/context.rs` (038's `scope` and `actor`, 046's `for_caller`),
`crates/core/src/api/{mod.rs,registry.rs,caller.rs}` and `crates/core/src/api/board/` (046),
`crates/core/src/testing/api.rs` (046's `FixedCaller`), `crates/server/src/caller.rs`,
`crates/server/Cargo.toml` and `crates/server/tests/commands.rs` (046). A good layout for the
new core code is one file per Scope item under `crates/core/src/identity/`, and for the
server `crates/server/src/{auth.rs,github.rs}` plus `crates/server/tests/identity.rs`.

**Migration.** `src-tauri/migrations/20261003120300_identity.sql` (D28, D4 amendment).

**What the chain provides.** 038: the `users`, `teams`, `team_memberships` and `runners`
tables, `create_personal_team`, and `Role`. 039: every board service already filters by
`ctx.scope`, so a `Caller` whose teams are right is a request that sees the right board. 046:
`rimaia-server`, `ServerState` with a clock, the registry and `dispatch`, `Caller`, `Door`,
`Credential`, `Authenticate`, `RefuseAll`, `FixedCaller`, the bearer half of the extractor,
the status table, `ErrorCode::Unauthenticated`, the protocol header, and the per-row HTTP
test suite. 047 changes none of 046's shapes; it fills in the one implementation 046 left
refusing.

**What the next tasks expect.**

- 048: `SessionsAndTokens` behind `GET /api/v1/events`, which accepts the same two doors.
- 049: the `rimaia_csrf` cookie to read and the `X-Rimaia-CSRF` header to send;
  `unauthenticated` as the one signal to sign in again.
- 050: `/auth/github` to link to, `/auth/sign_out` to post to, the `?sign_in=` vocabulary,
  and the eight account rows for the account page and the team switcher's list.
- 051: `upsert_user` as the only way a user appears; `get_account`'s team list, which it may
  extend. Deleting an account cascades through this task's tables.
- 052: `Door::Runner { runner_id }` from an `rmr_` token, which `RunnerCaller` narrows to.
  057: `unpair_runner`, to which it adds releasing pins.
- 058: `POST /api/v1/auth/pair` with `{ code, label, provider }`, answering `{ runnerId,
  token }`.
- 059: `/auth/desktop`, `/api/v1/auth/desktop_token`, `pair_own_runner`, and
  `create_personal_access_token` for the loopback endpoint's token.
- 060: `Door::Mcp { token_id: Some(_) }` with the token's team restriction already applied to
  `Caller.teams`.
- 062: the three environment variables, and the forwarded-address question Scope 9 leaves
  open.

**Two things a reviewer should see stated, not discovered.**

- *Open sign-up.* Nothing in ADR-0027 to 0037 limits which GitHub accounts may create a user
  on the hosted instance. This task implements ADR-0029 point 2 as written. If the hosted
  instance must admit only some accounts before 062 deploys it, that is a new decision for an
  ADR amendment, not an implementation detail to add here.
- *Rate-limit refusals are 400, not 429.* That follows from D8 and D32 point 3 as they stand.
  Changing it is an amendment to both, and belongs in its own commit with its own reason.

**Size.** L, and near the ceiling: roughly 1,300 lines of core (secrets, provider seam,
sessions, tokens, pairing, pending state, rate limiter, `Authenticate`, account handlers)
with about as much again in tests, 500 of server routes and the GitHub client, 250 of
TypeScript wrappers, types and tests, and the migration, before the two `.sqlx` caches. If it
runs over, cut the desktop half, the `/auth/desktop` and `desktop_token` routes,
`DesktopCodes` and `pair_own_runner`, into the first commit of 059. Nothing between 047 and
059 uses them, and the browser, pairing-code and personal-token paths stand on their own.
The rate limiter and the constant-time comparisons are not candidates for cutting: ADR-0030
says they are part of the feature.
