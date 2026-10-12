import { useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { reportSignedOut } from "../../lib/auth";
import { listSessions, revokeSession, toRimaiaError } from "../../lib/commands";
import { lastSeen, shortUserAgent } from "../../lib/format";
import type { RimaiaError, SessionSummary } from "../../types";
import { useAccountRead } from "./useAccountRead";

/**
 * The browsers this person is signed in on (ADR-0030 point 2), each
 * revocable. Revoking the current one is signing out.
 */
export function SessionsSection({ now = new Date() }: { readonly now?: Date }) {
  const { value: sessions, error: readError, reload } = useAccountRead(listSessions);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  async function revoke(session: SessionSummary) {
    setRevoking(session.id);
    setError(null);
    try {
      await revokeSession(session.id);
      if (session.current) reportSignedOut();
      else reload();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setRevoking(null);
    }
  }

  return (
    <section className="panel account-section" aria-labelledby="account-sessions">
      <h3 id="account-sessions">Sessions</h3>
      <p className="muted">The browsers you are signed in on.</p>
      {(readError ?? error) && (
        <ErrorBanner
          error={(readError ?? error) as RimaiaError}
          onDismiss={error ? () => setError(null) : undefined}
        />
      )}
      {sessions === null && !readError && <p className="muted">Reading…</p>}
      {sessions && sessions.length === 0 && <p className="muted">No browser sessions.</p>}
      {sessions && sessions.length > 0 && (
        <ul className="account-rows">
          {sessions.map((session) => (
            <li key={session.id} className="account-row">
              <div className="account-row-main">
                <span className="account-row-title">
                  {shortUserAgent(session.userAgent)}
                  {session.current && <span className="account-row-flag">this browser</span>}
                </span>
                <span className="muted tabular-nums">
                  Signed in {lastSeen(session.createdAt, now)} · last used{" "}
                  {lastSeen(session.lastUsedAt, now)}
                </span>
              </div>
              <button
                type="button"
                disabled={revoking === session.id}
                onClick={() => revoke(session)}
                aria-label={`Revoke the session on ${shortUserAgent(session.userAgent)}`}
              >
                Revoke
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
