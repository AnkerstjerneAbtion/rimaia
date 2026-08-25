import { useEffect, useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { getAppInfo, revealAppDataDir, toRimaiaError } from "../../lib/commands";
import type { AppInfo, RimaiaError } from "../../types";

export function StorageSection() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    getAppInfo().then(setInfo, (thrown) => setError(toRimaiaError(thrown)));
  }, []);

  async function openInFinder() {
    setError(null);
    try {
      await revealAppDataDir();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }

  return (
    <section className="panel">
      <h3>Storage</h3>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
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
          <button type="button" onClick={openInFinder}>
            Open in Finder
          </button>
        </>
      ) : (
        !error && <p className="muted">Reading…</p>
      )}
    </section>
  );
}
