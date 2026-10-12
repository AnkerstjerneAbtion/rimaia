import { useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { reportSignedOut } from "../../lib/auth";
import { revokeSession, toRimaiaError } from "../../lib/commands";
import type { Account, RimaiaError } from "../../types";

/**
 * Who is signed in, and Sign out (task 050 Scope 6).
 *
 * Signing out revokes the current session, then puts the sign-in screen up at
 * once rather than waiting for the next request to be refused: 047 has no
 * sign-out route, because `board<T>` is the only HTTP sender, and the two
 * cookies it leaves hold a dead secret.
 */
export function YouSection({ account }: { readonly account: Account }) {
  const [signingOut, setSigningOut] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  async function signOut() {
    setSigningOut(true);
    setError(null);
    try {
      await revokeSession(account.current.id);
      reportSignedOut();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
      setSigningOut(false);
    }
  }

  return (
    <section className="panel account-section" aria-labelledby="account-you">
      <h3 id="account-you">You</h3>
      <div className="account-you">
        {account.avatarUrl && (
          <img className="account-avatar" src={account.avatarUrl} alt="" width={40} height={40} />
        )}
        <div className="account-you-text">
          <span className="account-login">{account.login}</span>
          <span className="muted">
            Signed in with GitHub, in {account.teams.length}{" "}
            {account.teams.length === 1 ? "team" : "teams"}
          </span>
        </div>
        {account.current.kind === "session" && (
          <button type="button" onClick={signOut} disabled={signingOut}>
            {signingOut ? "Signing out…" : "Sign out"}
          </button>
        )}
      </div>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
    </section>
  );
}
