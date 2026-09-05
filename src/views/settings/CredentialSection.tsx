import { useCallback, useEffect, useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  getRepositoryCredentialStatus,
  removeRepositoryCredential,
  setRepositoryCredential,
  toRimaiaError,
} from "../../lib/commands";
import type { CredentialStatus, RimaiaError } from "../../types";

/**
 * One repository's forge token (task 022, ADR-0020).
 *
 * # Write-only after saving
 *
 * There is **replace** and **remove**, and no show. Nothing on this pane, no
 * command behind it and no MCP tool anywhere can read a stored token back — the
 * only paths out of the keychain are the spawn, which puts it in a child's
 * environment, and the delete.
 *
 * # Three outcomes, three messages
 *
 * A token the forge rejects refuses the save, because a token that cannot open
 * a pull request is a run that fails at 2am having already done the work. A
 * machine without `gh` saves it *unverified* — a missing local tool says
 * nothing about the token. Anything else is verified, and the login it resolved
 * to is what the pane shows afterwards.
 *
 * # The two things the pane has to say even when everything is fine
 *
 * **An SSH `origin` is a notice, not a warning.** The credential covers `gh`
 * API calls and any HTTPS remote; a push over SSH uses the machine's own key
 * regardless. ADR-0020 point 6: silence here would let a user believe Rimaia
 * controls an access path it does not.
 *
 * **A keychain that has lost the item is the state that refuses runs.** The row
 * says this repository has a token and the keychain does not have it, and
 * Rimaia will not fall back to the operator's own login — so this is where that
 * gets said, before 2am rather than after.
 */
export function CredentialSection({ repositoryId }: { repositoryId: string }) {
  const [status, setStatus] = useState<CredentialStatus | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [token, setToken] = useState("");
  const [label, setLabel] = useState("");
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);

  const refresh = useCallback(() => {
    getRepositoryCredentialStatus(repositoryId).then(setStatus, (thrown) =>
      setError(toRimaiaError(thrown)),
    );
  }, [repositoryId]);

  useEffect(refresh, [refresh]);

  async function save() {
    setBusy(true);
    setError(null);
    setSaved(null);
    try {
      const next = await setRepositoryCredential(
        repositoryId,
        token,
        label.trim() === "" ? null : label.trim(),
      );
      setStatus(next);
      // The token is gone from this component the moment it is stored. It is
      // not recoverable from here, which is the point.
      setToken("");
      setLabel("");
      setSaved(
        next.login === null
          ? "Saved, but not checked against the forge — `gh` is not installed on this machine."
          : `Saved and verified as ${next.login}.`,
      );
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    setError(null);
    setSaved(null);
    try {
      setStatus(await removeRepositoryCredential(repositoryId));
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setBusy(false);
    }
  }

  if (status === null) {
    return (
      <div className="repo-credential">
        <h5>Forge credential</h5>
        {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      </div>
    );
  }

  return (
    <div className="repo-credential">
      <h5>Forge credential</h5>
      <p className="muted">
        A token of this repository&apos;s own, kept in this machine&apos;s keychain and given
        only to this repository&apos;s runs. Without one, runs act with whatever GitHub login
        your own environment already has. Use a fine-grained token scoped to this repository —
        an unattended run can read its own environment, so what is worth bounding is what a
        stolen token could reach.
      </p>

      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {saved && <p className="repo-credential-saved">{saved}</p>}

      {status.configured ? (
        <dl className="repo-credential-facts">
          <div>
            <dt>Account</dt>
            <dd>{status.login ?? "not verified — `gh` was not installed when it was saved"}</dd>
          </div>
          {status.label && (
            <div>
              <dt>Label</dt>
              <dd>{status.label}</dd>
            </div>
          )}
          {status.addedAt && (
            <div>
              <dt>Added</dt>
              <dd>{new Date(status.addedAt).toLocaleDateString()}</dd>
            </div>
          )}
        </dl>
      ) : (
        <p className="muted">No credential configured for this repository.</p>
      )}

      {/* The state that refuses a run. Said here, in daylight, rather than
          discovered by the queue at 2am. */}
      {status.configured && status.store.state !== "stored" && (
        <p className="repo-credential-broken" role="alert">
          {status.store.state === "absent"
            ? "This repository is configured to run with its own token, and this machine's keychain does not have it. Runs here will refuse to start until you add it again or remove the credential — Rimaia will not fall back to your own GitHub login."
            : `This machine has no usable keychain, so the token cannot be read: ${status.store.reason}`}
        </p>
      )}

      {/* Not a warning: nothing is wrong. It is the one thing the user could
          otherwise believe this feature controls and it does not. */}
      {status.sshRemote && (
        <p className="repo-credential-notice">
          This repository&apos;s <code>origin</code> is an SSH remote. The token covers GitHub
          API calls — opening the pull request — and any HTTPS remote; the push itself will use
          this machine&apos;s SSH key whatever is stored here.
        </p>
      )}

      <div className="repo-credential-form">
        <label>
          <span>{status.configured ? "Replace token" : "Token"}</span>
          <input
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={token}
            onChange={(event) => setToken(event.target.value)}
            placeholder="ghp_…"
          />
        </label>
        <label>
          <span>Label</span>
          <input
            type="text"
            value={label}
            onChange={(event) => setLabel(event.target.value)}
            placeholder="fine-grained, expires March"
          />
        </label>
        <button type="button" disabled={busy || token.trim() === ""} onClick={() => void save()}>
          {busy ? "Checking…" : status.configured ? "Replace" : "Save"}
        </button>
        {status.configured && (
          <button type="button" disabled={busy} onClick={() => void remove()}>
            Remove
          </button>
        )}
      </div>
    </div>
  );
}
