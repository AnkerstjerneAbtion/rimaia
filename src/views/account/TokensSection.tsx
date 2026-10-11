import { useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { listApiTokens, revokeApiToken, toRimaiaError } from "../../lib/commands";
import { lastSeen, shortUserAgent } from "../../lib/format";
import type { ApiTokenKind, ApiTokenSummary, RimaiaError, TeamListing } from "../../types";
import { NewTokenDialog } from "./NewTokenDialog";
import { useAccountRead } from "./useAccountRead";

const KIND_LABELS: Record<ApiTokenKind, string> = {
  desktop: "Desktop app",
  runner: "Runner",
  personal: "Personal",
};

/**
 * The tokens this person has: a desktop's `rmd_`, a runner's `rmr_` and an
 * MCP client's `rmp_` (ADR-0030 point 3), each with where it was last used and
 * each revocable. Never a secret: the list reads none.
 */
export function TokensSection({
  teams,
  now = new Date(),
}: {
  readonly teams: readonly TeamListing[];
  readonly now?: Date;
}) {
  const { value: tokens, error: readError, reload } = useAccountRead(listApiTokens);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [creating, setCreating] = useState(false);

  async function revoke(token: ApiTokenSummary) {
    setRevoking(token.id);
    setError(null);
    try {
      await revokeApiToken(token.id);
      reload();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setRevoking(null);
    }
  }

  return (
    <section className="panel account-section" aria-labelledby="account-tokens">
      <h3 id="account-tokens">Tokens</h3>
      <p className="muted">
        What desktop apps, runners and MCP clients sign in with. Revoking one signs that client
        out.
      </p>
      {(readError ?? error) && (
        <ErrorBanner
          error={(readError ?? error) as RimaiaError}
          onDismiss={error ? () => setError(null) : undefined}
        />
      )}
      {tokens === null && !readError && <p className="muted">Reading…</p>}
      {tokens && tokens.length === 0 && <p className="muted">No tokens.</p>}
      {tokens && tokens.length > 0 && (
        <ul className="account-rows">
          {tokens.map((token) => (
            <li key={token.id} className="account-row">
              <div className="account-row-main">
                <span className="account-row-title">
                  {token.label}
                  <span className="account-row-flag">{KIND_LABELS[token.kind]}</span>
                  {token.current && <span className="account-row-flag">this client</span>}
                </span>
                <span className="muted tabular-nums">
                  Created {lastSeen(token.createdAt, now)} · last used{" "}
                  {lastSeen(token.lastUsedAt, now)}
                  {token.lastUsedFrom ? ` from ${shortUserAgent(token.lastUsedFrom)}` : ""} ·{" "}
                  {token.expiresAt ? `expires ${token.expiresAt.slice(0, 10)}` : "does not expire"}
                  {token.teamIds.length > 0
                    ? ` · ${token.teamIds.length} ${token.teamIds.length === 1 ? "team" : "teams"}`
                    : ""}
                </span>
              </div>
              <button
                type="button"
                disabled={revoking === token.id}
                onClick={() => revoke(token)}
                aria-label={`Revoke the token ${token.label}`}
              >
                Revoke
              </button>
            </li>
          ))}
        </ul>
      )}
      {creating ? (
        <NewTokenDialog
          teams={teams}
          onClose={() => setCreating(false)}
          onCreated={reload}
        />
      ) : (
        <div className="account-actions">
          <button type="button" onClick={() => setCreating(true)}>
            New personal access token
          </button>
        </div>
      )}
    </section>
  );
}
