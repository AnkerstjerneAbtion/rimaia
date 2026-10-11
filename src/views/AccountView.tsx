import { ErrorBanner } from "../components/ErrorBanner";
import { getAccount } from "../lib/commands";
import { useTeams } from "../lib/teams";
import { AccountRunnersSection } from "./account/AccountRunnersSection";
import { SessionsSection } from "./account/SessionsSection";
import { TokensSection } from "./account/TokensSection";
import { YouSection } from "./account/YouSection";
import { useAccountRead } from "./account/useAccountRead";

/**
 * The account page (task 050, ADR-0030 points 2, 3 and 5): who is signed in,
 * and the sessions, tokens and runners that sign in as them, each revocable.
 * A sidebar entry in every mode but solo, which has no accounts.
 *
 * Built on 047's eight account rows plus `list_runners` and `unpair_runner`.
 * Each section owns its read and its errors, as Settings' sections do.
 */
export function AccountView() {
  const { value: account, error } = useAccountRead(getAccount);
  const teams = useTeams();

  return (
    <div className="view account-view">
      <header className="view-header">
        <h2>Account</h2>
        <p>Who you are signed in as, and what can act as you.</p>
      </header>
      {error && <ErrorBanner error={error} />}
      {account === null && !error && <p className="muted">Reading…</p>}
      {account && (
        <>
          <YouSection account={account} />
          <SessionsSection />
          <TokensSection teams={teams?.teams ?? []} />
          <AccountRunnersSection />
        </>
      )}
    </div>
  );
}
