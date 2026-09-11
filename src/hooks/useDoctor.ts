import { useCallback, useEffect, useState } from "react";

import { dismissDoctorWarning, restoreDoctorWarning, runDoctor, toRimaiaError } from "../lib/commands";
import { dismissalFor, matchesDismissal } from "../lib/doctor";
import type { DoctorCheckResult, DoctorDismissal, DoctorReport, RimaiaError } from "../types";

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
  /** Puts one warn row down (task 027). A no-op against a `fail`. */
  readonly dismiss: (result: DoctorCheckResult) => Promise<void>;
  /** Brings one back, matched or stale. */
  readonly restore: (dismissal: DoctorDismissal) => Promise<void>;
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

  /**
   * Applies one write's answer to the report already on screen, rather than
   * re-probing the machine to find out what a settings write did.
   *
   * The environment has not changed — a dismissal is a preference — so a full
   * `rerun` here would spend eight subprocesses and a second or two to hide or
   * show one line. `dismissals` comes back from the command, and `dismissed`
   * is re-derived from it exactly as `DoctorReport::new` derives it, warn rows
   * only.
   */
  const applyDismissals = useCallback((dismissals: DoctorDismissal[]) => {
    setReport((current) =>
      current === null
        ? current
        : {
            results: current.results.map((result) => ({
              ...result,
              dismissed:
                result.status === "warn" &&
                dismissals.some((dismissal) => matchesDismissal(result, dismissal)),
            })),
            dismissals,
          },
    );
  }, []);

  const dismiss = useCallback(
    async (result: DoctorCheckResult) => {
      // Guarded here as well as in Rust, so a stray call from a component that
      // renders both statuses cannot store a dismissal that would never mark
      // anything. `CheckResult::answered_by` is what makes it true rather than
      // merely tidy.
      if (result.status !== "warn") return;
      try {
        applyDismissals(await dismissDoctorWarning(dismissalFor(result)));
        setError(null);
      } catch (thrown) {
        setError(toRimaiaError(thrown));
      }
    },
    [applyDismissals],
  );

  const restore = useCallback(
    async (dismissal: DoctorDismissal) => {
      try {
        applyDismissals(await restoreDoctorWarning(dismissal));
        setError(null);
      } catch (thrown) {
        setError(toRimaiaError(thrown));
      }
    },
    [applyDismissals],
  );

  return { report, running, error, checkedAt, rerun, dismiss, restore };
}
