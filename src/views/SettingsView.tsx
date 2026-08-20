import { useEffect, useState } from "react";

import { ErrorBanner } from "../components/ErrorBanner";
import {
  debugProvokeError,
  getAppInfo,
  revealAppDataDir,
  toRimaiaError,
} from "../lib/commands";
import type { AppInfo, RimaiaError } from "../types";

export function SettingsView() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    getAppInfo().then(setInfo, (thrown) => setError(toRimaiaError(thrown)));
  }, []);

  async function run(action: () => Promise<void>) {
    setError(null);
    try {
      await action();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }

  return (
    <div className="view">
      <header className="view-header">
        <h2>Settings</h2>
        <p>Repositories, base instructions and run behaviour arrive in task 006.</p>
      </header>

      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      <section className="panel">
        <h3>Storage</h3>
        {info ? (
          <>
            <dl className="detail-list">
              <dt>Application data</dt>
              <dd>
                <code>{info.dataDir}</code>
              </dd>
              <dt>Database</dt>
              <dd>
                <code>{info.dbFile}</code>
              </dd>
              <dt>Logs</dt>
              <dd>
                <code>{info.logsDir}</code>
              </dd>
              <dt>Version</dt>
              <dd>{info.appVersion}</dd>
            </dl>
            <button type="button" onClick={() => run(revealAppDataDir)}>
              Open in Finder
            </button>
          </>
        ) : (
          !error && <p className="muted">Reading…</p>
        )}
      </section>

      {import.meta.env.DEV && (
        <section className="panel panel-dev">
          <h3>Development</h3>
          <p className="muted">
            Checks that a backend error reaches the UI as a sentence. Not present in
            release builds.
          </p>
          <button type="button" onClick={() => run(debugProvokeError)}>
            Trigger a backend error
          </button>
        </section>
      )}
    </div>
  );
}
