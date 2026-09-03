import { useCallback, useEffect, useState } from "react";

import { getMcpStatus, toRimaiaError } from "../lib/commands";
import { subscribeToSettingsChanged } from "../lib/events";
import type { McpStatus, RimaiaError } from "../types";

/** Built from `boundAddress`, never from `configuredPort` — see `McpSection`'s
 *  contract note. The two disagree in exactly the case worth showing. */
export function mcpAddCommand(status: McpStatus | null): string | null {
  if (!status || status.state !== "listening" || !status.boundAddress) return null;
  return `claude mcp add --transport http rimaia http://${status.boundAddress}/mcp`;
}

/**
 * The one line a user has to run to point Claude Code at Rimaia, with a copy
 * button — extracted from `McpSection` so the welcome flow's last step shows
 * the real, live command instead of a hardcoded example (task 018).
 *
 * A hardcoded example is precisely the bug this avoids: the port is
 * configurable and may have been taken at startup, so a welcome screen that
 * printed `4517` would hand a first-time user a line that silently connects to
 * nothing.
 */
export function McpAddCommand() {
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [copied, setCopied] = useState(false);

  const read = useCallback(async () => {
    try {
      setStatus(await getMcpStatus());
      setError(null);
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }, []);

  useEffect(() => {
    void read();
  }, [read]);

  useEffect(() => {
    const subscription = subscribeToSettingsChanged(() => {
      void read();
    });
    return () => {
      void subscription.then((unlisten) => unlisten()).catch(() => {
        // No event bridge (tests, or a non-Tauri preview) — the mount read above
        // still gives this component a correct command.
      });
    };
  }, [read]);

  const command = mcpAddCommand(status);

  async function handleCopy() {
    if (!command) return;
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  if (error) {
    return <p className="muted">The MCP server status could not be read: {error.message}</p>;
  }

  if (!command) {
    return (
      <p className="muted">
        The MCP server is not listening yet, so there is no address to register. Settings → MCP
        explains why and lets you change the port.
      </p>
    );
  }

  return (
    <>
      <p className="muted">Register it once, in any terminal:</p>
      <code className="mcp-command">{command}</code>
      <button type="button" onClick={handleCopy}>
        {copied ? "Copied" : "Copy command"}
      </button>
    </>
  );
}
