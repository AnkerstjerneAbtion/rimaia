import { useCallback, useEffect, useState } from "react";

import { runDoctor, toRimaiaError } from "../lib/commands";
import type { DoctorReport, RimaiaError } from "../types";

/**
 * The preflight doctor (task 018), read once on mount and re-runnable on
 * demand.
 *
 * There is no `doctor:changed` event and none would help: every input the
 * doctor reads lives *outside* the app — a binary on `PATH`, a `gh` login, a
 * directory that has since been moved, free space on the volume. Nothing in
 * Rimaia writes them, so nothing in Rimaia is in a position to announce that
 * they changed. The honest model is a snapshot with a visible timestamp and a
 * "Check again" button, which is exactly what `McpSection` concluded for its
 * own live check for the same reason.
 *
 * Each check shells out to a real subprocess, so this is not a read to fire on
 * every render or every navigation: mount, and explicit re-runs only.
 */
export interface UseDoctorResult {
  /** `null` until the first run resolves. */
  readonly report: DoctorReport | null;
  /** True during the first run *and* every re-run — the button needs both. */
  readonly running: boolean;
  readonly error: RimaiaError | null;
  /** When the report on screen was produced, for the "as of" line. */
  readonly checkedAt: Date | null;
  readonly rerun: () => Promise<void>;
}

export function useDoctor(): UseDoctorResult {
  const [report, setReport] = useState<DoctorReport | null>(null);
  const [running, setRunning] = useState(true);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);

  const rerun = useCallback(async () => {
    setRunning(true);
    try {
      const next = await runDoctor();
      setReport(next);
      setCheckedAt(new Date());
      setError(null);
    } catch (thrown) {
      // The previous report is deliberately left on screen. A doctor run that
      // failed to *execute* says nothing about the checks it never got to, and
      // blanking eight rows because one IPC call failed would report a healthy
      // machine as unknown.
      setError(toRimaiaError(thrown));
    } finally {
      setRunning(false);
    }
  }, []);

  useEffect(() => {
    void rerun();
  }, [rerun]);

  return { report, running, error, checkedAt, rerun };
}
