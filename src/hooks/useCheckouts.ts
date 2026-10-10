import { useMemo } from "react";

import { listCheckouts } from "../lib/commands";
import { subscribeToRepositoriesChanged } from "../lib/events";
import type { CheckoutView, RimaiaError } from "../types";
import { createSharedRead } from "./sharedRead";

export interface UseCheckoutsResult {
  /** This computer's checkouts, keyed by repository id. A repository with no
   *  entry is not set up on this computer (task 066). */
  readonly checkouts: ReadonlyMap<string, CheckoutView>;
  /** True until the first read answers, so a caller can tell "not set up" from
   *  "not read yet". */
  readonly loading: boolean;
  readonly error: RimaiaError | null;
  /** Reads again now, after this component's own write. */
  readonly reload: () => void;
}

const useCheckoutList = createSharedRead(listCheckouts, subscribeToRepositoriesChanged);

/**
 * This computer's clone of each repository (task 066): the path, the worktree
 * root, the cap, the consent and the archive policy the board's `Repository`
 * no longer carries. Components join it to a repository by id.
 *
 * Re-read on `repositories:changed`, which every checkout write publishes
 * until task 048 turns it into `LocalChange::Checkouts` on the same wire name.
 * Every payload means "re-read", including the lagged forwarder's empty array
 * (seam-contract D7). One read is shared by every component mounted at once.
 */
export function useCheckouts(): UseCheckoutsResult {
  const { value, error, reload } = useCheckoutList();
  const checkouts = useMemo(
    () => new Map((value ?? []).map((checkout) => [checkout.repositoryId, checkout])),
    [value],
  );
  return { checkouts, loading: value === null && error === null, error, reload };
}
