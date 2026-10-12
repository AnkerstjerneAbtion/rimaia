import { createContext, useContext } from "react";
import type { ReactNode } from "react";

import type { ClientCapabilities } from "../types";

/**
 * What a desktop answers in solo, and what a component sees when nothing
 * mounted a provider: every existing test, which therefore renders solo and
 * never sends `get_client_capabilities` to an `invoke` mock that rejects
 * unknown commands.
 */
export const SOLO_CAPABILITIES: ClientCapabilities = {
  mode: "solo",
  localRunnerId: null,
  canOpenInEditor: true,
  canRevealFiles: true,
  canChooseFolders: true,
};

const Capabilities = createContext<ClientCapabilities>(SOLO_CAPABILITIES);

/**
 * Makes `capabilities` what {@link useClientCapabilities} answers below it.
 * `main.tsx` mounts it with the answer `bootstrapClient()` installed
 * transports for; the fixture entry mounts it with its scenario's.
 */
export function ClientCapabilitiesProvider({
  capabilities,
  children,
}: {
  readonly capabilities: ClientCapabilities;
  readonly children: ReactNode;
}) {
  return <Capabilities.Provider value={capabilities}>{children}</Capabilities.Provider>;
}

/**
 * What this client can do (ADR-0034 point 5). Every browser state in the
 * interface comes from this answer, never from a platform check: a component
 * that asked the platform would be guessing at what the client already knows.
 */
export function useClientCapabilities(): ClientCapabilities {
  return useContext(Capabilities);
}

/**
 * Whether this client has a machine of its own to act on: a desktop, solo or
 * connected, and not a browser. The gate for a local command that has no
 * board counterpart yet, each with a comment naming the command that forces
 * it (task 050 Scope 6).
 */
export function useHasMachine(): boolean {
  return useClientCapabilities().mode !== "browser";
}
