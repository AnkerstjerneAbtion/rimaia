import { useEffect, useState } from "react";

import { useClientCapabilities } from "../lib/capabilities";

/** Why a sign-in failed, by the `sign_in` value the server's callback puts on
 *  `/` (task 047). Any other value is not echoed. */
const SIGN_IN_FAILURES: Readonly<Record<string, string>> = {
  denied: "GitHub did not grant access, so you are not signed in.",
  expired: "That sign-in took too long and has expired. Sign in again.",
  failed: "Signing in did not work. Try again in a minute.",
};

/**
 * Reads `sign_in` from the address and removes it, so a reload does not
 * explain a failure that is no longer news. Run once per mount.
 */
function takeSignInFailure(): string | null {
  const url = new URL(window.location.href);
  const reason = url.searchParams.get("sign_in");
  if (reason === null) return null;
  url.searchParams.delete("sign_in");
  window.history.replaceState(window.history.state, "", url);
  return Object.prototype.hasOwnProperty.call(SIGN_IN_FAILURES, reason) ? SIGN_IN_FAILURES[reason] : null;
}

/**
 * Shown whenever there is no session (task 050 Scope 6): the product name,
 * one sentence, and in a browser a link to GitHub.
 *
 * **A plain link, not a `fetch`**: OAuth needs a top-level navigation. A
 * connected desktop (task 059) supplies its own loopback sign-in in this place
 * (ADR-0030 point 4), so the browser's link is never rendered there.
 */
export function SignInView() {
  const { mode } = useClientCapabilities();
  // Lazy, so `replaceState` runs once and the sentence survives a re-render.
  const [failure] = useState(takeSignInFailure);
  useEffect(() => {
    document.title = "Sign in · Rimaia";
  }, []);

  return (
    <main className="sign-in">
      <div className="sign-in-card panel">
        <span className="sidebar-mark" aria-hidden="true">
          R
        </span>
        <h1>Rimaia</h1>
        <p className="sign-in-lede">
          Queue implementation plans in the evening, review what the agent did in the morning.
        </p>
        {failure && (
          <p className="sign-in-failure" role="alert">
            {failure}
          </p>
        )}
        {mode === "browser" && (
          <a className="btn-primary sign-in-link" href="/auth/github">
            Sign in with GitHub
          </a>
        )}
      </div>
    </main>
  );
}
