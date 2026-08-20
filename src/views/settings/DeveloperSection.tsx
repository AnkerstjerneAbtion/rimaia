import { useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { debugProvokeError, toRimaiaError } from "../../lib/commands";
import type { RimaiaError } from "../../types";

/** Dev build only - the composer gates rendering on `import.meta.env.DEV`.
 *  Exercises the same backend-error → `ErrorBanner` path as every other
 *  section, against a command that always fails, so a broken mapping is
 *  visible without waiting for a real one. */
export function DeveloperSection() {
  const [error, setError] = useState<RimaiaError | null>(null);

  async function triggerError() {
    setError(null);
    try {
      await debugProvokeError();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }

  return (
    <section className="panel panel-dev">
      <h3>Development</h3>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <p className="muted">
        Checks that a backend error reaches the UI as a sentence. Not present in release
        builds.
      </p>
      <button type="button" onClick={triggerError}>
        Trigger a backend error
      </button>
    </section>
  );
}
