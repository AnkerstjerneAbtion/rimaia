import { useState } from "react";

import { open } from "@tauri-apps/plugin-dialog";

import { ErrorBanner } from "./ErrorBanner";
import { registerRepository, toRimaiaError } from "../lib/commands";
import type { RimaiaError } from "../types";

/**
 * "Choose a directory, register it" — extracted from `RepositoriesSection` so
 * the welcome flow's first step is the *same* control rather than a second
 * implementation of it (task 018).
 *
 * That matters beyond saving a few lines: registration validates the path, and
 * a welcome screen with its own copy would be free to drift into accepting
 * something Settings rejects. The one behaviour that stays with each caller is
 * what to do afterwards — `RepositoriesSection` re-reads its list, the welcome
 * flow advances a step — so `onRegistered` is a prop and nothing else is.
 */
export function RepositoryAddForm({ onRegistered }: { onRegistered: () => void }) {
  const [error, setError] = useState<RimaiaError | null>(null);
  const [adding, setAdding] = useState(false);

  async function handleAdd() {
    setError(null);
    let selected: string | string[] | null;
    try {
      selected = await open({ directory: true, multiple: false, title: "Choose a repository" });
    } catch (thrown) {
      setError(toRimaiaError(thrown));
      return;
    }
    if (!selected || Array.isArray(selected)) return; // cancelled

    setAdding(true);
    try {
      await registerRepository({ path: selected });
      onRegistered();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setAdding(false);
    }
  }

  return (
    <div className="repo-add">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <button type="button" onClick={handleAdd} disabled={adding}>
        {adding ? "Adding…" : "Add repository"}
      </button>
    </div>
  );
}
