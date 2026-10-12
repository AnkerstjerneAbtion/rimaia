import { useCallback, useEffect, useState } from "react";

import { toRimaiaError } from "../../lib/commands";
import type { RimaiaError } from "../../types";

export interface AccountRead<T> {
  /** `null` until the first read answers. */
  readonly value: T | null;
  readonly error: RimaiaError | null;
  /** Reads again, after this section's own write: there is no event for the
   *  account page, which one person looks at (task 050 Scope 4). */
  readonly reload: () => void;
}

/**
 * One account read, on mount and after the section's own writes.
 *
 * No subscription: sessions, tokens and runners belong to a user and publish
 * no `ChangeEvent`, so a second open tab sees a revocation on its next read.
 * The newest read wins, so a slow answer does not replace a fresher one.
 */
export function useAccountRead<T>(read: () => Promise<T>): AccountRead<T> {
  const [value, setValue] = useState<T | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [generation, setGeneration] = useState(0);

  useEffect(() => {
    let active = true;
    read().then(
      (next) => {
        if (!active) return;
        setValue(next);
        setError(null);
      },
      (thrown) => {
        if (active) setError(toRimaiaError(thrown));
      },
    );
    return () => {
      active = false;
    };
    // `read` is a module-level wrapper, stable for the section's life.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [generation]);

  const reload = useCallback(() => setGeneration((n) => n + 1), []);
  return { value, error, reload };
}
