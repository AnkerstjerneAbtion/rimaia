import { useState } from "react";
import type { FormEvent } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { createPersonalAccessToken, toRimaiaError } from "../../lib/commands";
import type { RimaiaError, TeamListing } from "../../types";

interface NewTokenDialogProps {
  /** The teams the person could restrict the token to. */
  readonly teams: readonly TeamListing[];
  /** Closes the dialog. The secret goes with it. */
  readonly onClose: () => void;
  /** Called once a token exists, so the list behind it can re-read. The list
   *  holds no secret. */
  readonly onCreated: () => void;
}

/**
 * "New personal access token" (ADR-0030 points 3 and 6): a label, an optional
 * subset of the person's teams and an optional expiry in days.
 *
 * **The secret is shown once.** It lives in this component's state only: never
 * in storage, never logged, and never part of a re-read. Closing the dialog
 * unmounts it, which is the whole of "dropped from component state".
 */
export function NewTokenDialog({ teams, onClose, onCreated }: NewTokenDialogProps) {
  const [label, setLabel] = useState("");
  const [teamIds, setTeamIds] = useState<ReadonlySet<string>>(new Set());
  const [days, setDays] = useState("");
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const parsedDays = days.trim() === "" ? undefined : Number(days);
  const daysAreLegal =
    parsedDays === undefined || (Number.isInteger(parsedDays) && parsedDays >= 1);

  async function create(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (creating || label.trim() === "" || !daysAreLegal) return;
    setCreating(true);
    setError(null);
    try {
      const created = await createPersonalAccessToken(
        label.trim(),
        teamIds.size > 0 ? [...teamIds] : undefined,
        parsedDays,
      );
      setSecret(created.token);
      onCreated();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setCreating(false);
    }
  }

  async function copy() {
    if (secret === null) return;
    try {
      await navigator.clipboard.writeText(secret);
      setCopied(true);
    } catch {
      // The secret stays on screen and selectable, so a refusal is no dead end.
      setError({ code: "internal", message: "could not copy the token to the clipboard" });
    }
  }

  return (
    <div className="account-dialog" role="dialog" aria-label="New personal access token">
      {secret === null ? (
        <form onSubmit={create}>
          <label className="account-field">
            Label
            <input
              type="text"
              value={label}
              onChange={(event) => setLabel(event.target.value)}
              placeholder="Claude Code on my laptop"
              autoFocus
            />
          </label>
          {teams.length > 1 && (
            <fieldset className="account-field">
              <legend>Teams</legend>
              <p className="muted">Leave all unchecked for every team you are in, now and later.</p>
              {teams.map((team) => (
                <label key={team.id} className="account-check">
                  <input
                    type="checkbox"
                    checked={teamIds.has(team.id)}
                    onChange={(event) => {
                      const next = new Set(teamIds);
                      if (event.target.checked) next.add(team.id);
                      else next.delete(team.id);
                      setTeamIds(next);
                    }}
                  />
                  {team.name}
                </label>
              ))}
            </fieldset>
          )}
          <label className="account-field">
            Expires after (days)
            <input
              type="number"
              min={1}
              value={days}
              onChange={(event) => setDays(event.target.value)}
              placeholder="Never"
            />
          </label>
          {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
          <div className="account-dialog-actions">
            <button
              type="submit"
              className="btn-primary"
              disabled={creating || label.trim() === "" || !daysAreLegal}
            >
              {creating ? "Creating…" : "Create token"}
            </button>
            <button type="button" onClick={onClose}>
              Cancel
            </button>
          </div>
        </form>
      ) : (
        <div>
          <p>
            <strong>This is the only time it will be shown.</strong> Copy it now; Rimaia keeps
            only a hash.
          </p>
          <code className="account-secret">{secret}</code>
          {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
          <div className="account-dialog-actions">
            <button type="button" onClick={copy}>
              {copied ? "Copied" : "Copy token"}
            </button>
            <button type="button" className="btn-primary" onClick={onClose}>
              Done
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
