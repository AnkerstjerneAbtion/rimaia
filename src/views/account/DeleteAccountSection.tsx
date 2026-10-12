import { useState } from "react";
import type { FormEvent } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { reportSignedOut } from "../../lib/auth";
import { deleteAccount, listTeamMembers, toRimaiaError } from "../../lib/commands";
import type { Account, RimaiaError, TeamListing, TeamMember } from "../../types";
import { useAccountRead } from "./useAccountRead";

/**
 * The teams that would stop an account's deletion: shared teams the person
 * is the only owner of (task 051 Scope 7). Deleting the account anyway would
 * leave each with nobody to administer or delete it, so the server refuses.
 */
export function blockingTeams(teams: readonly TeamListing[]): TeamListing[] {
  return teams.filter((team) => !team.personal && team.role === "owner" && team.ownerCount === 1);
}

/**
 * Deleting the account (task 051 Scope 12, ADR-0029 point 6): what blocks it,
 * who else loses the personal team it takes with it, and the login typed
 * exactly. The server holds every door to the same rules.
 */
export function DeleteAccountSection({
  account,
  teams,
}: {
  readonly account: Account;
  readonly teams: readonly TeamListing[];
}) {
  const personal = teams.find((team) => team.personal) ?? null;
  const blocking = blockingTeams(teams);
  const [typed, setTyped] = useState("");
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const matches = typed === account.login;

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!matches || deleting) return;
    setDeleting(true);
    setError(null);
    try {
      await deleteAccount(typed);
      // Every session and token went with the account.
      reportSignedOut();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
      setDeleting(false);
    }
  }

  return (
    <section className="panel account-section" aria-labelledby="account-delete">
      <h3 id="account-delete">Delete account</h3>
      <p className="muted">
        Deletes your account, your personal team and everything in it. Tasks you wrote in other
        teams stay, by a former member.
      </p>
      {blocking.length > 0 && (
        <div className="account-blocking">
          <p>You are the only owner of these teams. Make someone else an owner, or delete them, first:</p>
          <ul>
            {blocking.map((team) => (
              <li key={team.id}>{team.name}</li>
            ))}
          </ul>
        </div>
      )}
      {personal && <PersonalTeamMembers personal={personal} account={account} />}
      <form className="account-confirm" onSubmit={submit} aria-label="Delete account">
        <label className="account-field">
          {`Type ${account.login} to confirm`}
          <input
            type="text"
            value={typed}
            onChange={(event) => setTyped(event.target.value)}
            autoComplete="off"
          />
        </label>
        {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
        <div className="account-dialog-actions">
          <button type="submit" className="btn-danger" disabled={!matches || deleting}>
            {deleting ? "Deleting…" : "Delete account"}
          </button>
        </div>
      </form>
    </section>
  );
}

/** The other people in the personal team, who lose it with the account. */
function PersonalTeamMembers({
  personal,
  account,
}: {
  readonly personal: TeamListing;
  readonly account: Account;
}) {
  const members = useAccountRead(() => listTeamMembers(personal.id));
  const others: TeamMember[] = (members.value ?? []).filter(
    (member) => member.userId !== account.id,
  );
  if (members.error) return <ErrorBanner error={members.error} />;
  if (others.length === 0) return null;
  return (
    <div className="account-blocking">
      <p>These people are in your personal team and will lose it:</p>
      <ul>
        {others.map((member) => (
          <li key={member.userId}>{member.login}</li>
        ))}
      </ul>
    </div>
  );
}
