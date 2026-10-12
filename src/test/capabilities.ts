import type { ClientCapabilities } from "../types";

/** What a browser answers (task 049): no machine, so nothing local. */
export const BROWSER: ClientCapabilities = {
  mode: "browser",
  localRunnerId: null,
  canOpenInEditor: false,
  canRevealFiles: false,
  canChooseFolders: false,
};

/** A connected desktop whose own runner is `localRunnerId` (task 059). */
export function connected(localRunnerId: string | null): ClientCapabilities {
  return {
    mode: "connected",
    localRunnerId,
    canOpenInEditor: true,
    canRevealFiles: true,
    canChooseFolders: true,
  };
}
