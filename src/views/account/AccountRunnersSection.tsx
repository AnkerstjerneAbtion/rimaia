import { useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  createPairingCode,
  listRunners,
  toRimaiaError,
  unpairRunner,
} from "../../lib/commands";
import { lastSeen } from "../../lib/format";
import type { PairingCode, RimaiaError, RunnerListing } from "../../types";
import { useAccountRead } from "./useAccountRead";

/**
 * The exact line a person types on a headless machine (task 050 Scope 6,
 * printed by task 058): this origin, then the code.
 */
export function pairingLine(origin: string, code: string): string {
  return `rimaia-runner pair ${origin} ${code}`;
}

/**
 * The runners this person has paired (ADR-0030 point 5), each unpairable
 * behind a confirmation that names it, and a pairing code for the next one.
 * Another person's runner is never here, even in a shared team: a runner works
 * for exactly one user.
 */
export function AccountRunnersSection({ now = new Date() }: { readonly now?: Date }) {
  const { value: runners, error: readError, reload } = useAccountRead(listRunners);
  const [confirming, setConfirming] = useState<RunnerListing | null>(null);
  const [unpairing, setUnpairing] = useState(false);
  const [pairing, setPairing] = useState<PairingCode | null>(null);
  const [creatingCode, setCreatingCode] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  async function unpair(runner: RunnerListing) {
    setUnpairing(true);
    setError(null);
    try {
      await unpairRunner(runner.id);
      setConfirming(null);
      reload();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setUnpairing(false);
    }
  }

  async function pairARunner() {
    setCreatingCode(true);
    setError(null);
    try {
      setPairing(await createPairingCode());
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setCreatingCode(false);
    }
  }

  return (
    <section className="panel account-section" aria-labelledby="account-runners">
      <h3 id="account-runners">Runners</h3>
      <p className="muted">The machines that run your tasks.</p>
      {(readError ?? error) && (
        <ErrorBanner
          error={(readError ?? error) as RimaiaError}
          onDismiss={error ? () => setError(null) : undefined}
        />
      )}
      {runners === null && !readError && <p className="muted">Reading…</p>}
      {runners && runners.length === 0 && <p className="muted">No runners are paired.</p>}
      {runners && runners.length > 0 && (
        <ul className="account-rows">
          {runners.map((runner) => (
            <li key={runner.id} className="account-row">
              <div className="account-row-main">
                <span className="account-row-title">{runner.label}</span>
                <span className="muted tabular-nums">
                  {runner.provider} · {runner.appVersion ? `v${runner.appVersion}` : "version unknown"}{" "}
                  · last seen {lastSeen(runner.lastSeenAt, now)}
                </span>
                {confirming?.id === runner.id && (
                  <div className="account-confirm" role="group" aria-label={`Unpair ${runner.label}`}>
                    <p>
                      Unpair <strong>{runner.label}</strong>? It stops taking work and its token
                      is revoked. Its past runs keep its name.
                    </p>
                    <div className="account-dialog-actions">
                      <button
                        type="button"
                        className="btn-danger"
                        disabled={unpairing}
                        onClick={() => unpair(runner)}
                      >
                        {unpairing ? "Unpairing…" : `Unpair ${runner.label}`}
                      </button>
                      <button type="button" onClick={() => setConfirming(null)}>
                        Keep it
                      </button>
                    </div>
                  </div>
                )}
              </div>
              {confirming?.id !== runner.id && (
                <button
                  type="button"
                  onClick={() => setConfirming(runner)}
                  aria-label={`Unpair the runner ${runner.label}`}
                >
                  Unpair
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {pairing ? (
        <div className="account-dialog" role="group" aria-label="Pair a runner">
          <p>
            On the machine that should run tasks, run this within ten minutes. The code works
            once.
          </p>
          <code className="account-secret">
            {pairingLine(window.location.origin, pairing.code)}
          </code>
          <p className="muted tabular-nums">Expires {pairing.expiresAt.slice(11, 16)} UTC.</p>
          <div className="account-dialog-actions">
            <button type="button" onClick={() => setPairing(null)}>
              Done
            </button>
          </div>
        </div>
      ) : (
        <div className="account-actions">
          <button type="button" onClick={pairARunner} disabled={creatingCode}>
            {creatingCode ? "Making a code…" : "Pair a runner"}
          </button>
        </div>
      )}
    </section>
  );
}
