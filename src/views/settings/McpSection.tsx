import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import { mcpAddCommand } from "../../components/McpAddCommand";
import {
  getMcpStatus,
  setMcpPort,
  testMcpConnection,
  toRimaiaError,
} from "../../lib/commands";
import { subscribeToSettingsChanged } from "../../lib/events";
import type { McpProbe, McpStatus, RimaiaError } from "../../types";

/** The range `set_mcp_port`'s `u16` can hold, minus the privileged ports core
 *  refuses anyway. Belt and braces: a JSON number outside `0..=65535` fails
 *  serde *inside* Tauri and surfaces as a bare string rather than a
 *  `RimaiaError`, so the form must never send one. */
const LOWEST_PORT = 1024;
const HIGHEST_PORT = 65535;

/**
 * Settings → MCP (task 010, ADR-0006).
 *
 * The panel exists for one moment in particular: the port Rimaia is configured
 * for is taken, so the server is not running and the `claude mcp add` line the
 * user would copy is a lie. **Every URL on screen is therefore built from
 * `boundAddress`, never from `configuredPort`** — there is deliberately no
 * `4517` and no `127.0.0.1` literal anywhere in this file, because those two
 * values disagree in exactly the case this panel exists to explain.
 *
 * There is no `mcp:changed` event, and none is needed: the status changes at
 * exactly two moments, and a channel serves neither. At startup, before any
 * listener exists — an emit would be lost, and the panel has to read on mount
 * regardless. And on {@link setMcpPort}, where the caller *is* this panel and
 * already has the new status as the return value. This is verbatim the argument
 * `src/lib/events.ts` records for task 009's queue state. The third way it stays
 * fresh is `settings:changed`, since `mcp_port` is a settings key and any other
 * writer announces itself there.
 *
 * The one hole, stated rather than papered over: if the axum task died after a
 * successful bind, nothing re-emits and this cached status goes stale. That is
 * what Test connection is for, and it is presented as the only live check.
 */
export function McpSection() {
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [statusError, setStatusError] = useState<RimaiaError | null>(null);
  const [port, setPort] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<RimaiaError | null>(null);
  const [copied, setCopied] = useState(false);
  const [testing, setTesting] = useState(false);
  const [probe, setProbe] = useState<McpProbe | null>(null);
  const [probeError, setProbeError] = useState<RimaiaError | null>(null);

  const read = useCallback(async () => {
    try {
      const current = await getMcpStatus();
      setStatus(current);
      setPort(String(current.configuredPort));
      setStatusError(null);
    } catch (thrown) {
      setStatusError(toRimaiaError(thrown));
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
      void subscription.then((unlisten) => unlisten());
    };
  }, [read]);

  const listening = status?.state === "listening";
  const url = status?.boundAddress ? `http://${status.boundAddress}/mcp` : null;
  // Shared with the welcome flow's last step rather than spelled twice: the
  // rule that the command is built from `boundAddress` is the whole point of
  // the note above, and two copies of it are two chances to regress it.
  const addCommand = mcpAddCommand(status);
  const parsedPort = Number(port);
  const portIsLegal =
    Number.isInteger(parsedPort) && parsedPort >= LOWEST_PORT && parsedPort <= HIGHEST_PORT;
  const portChanged = status !== null && parsedPort !== status.configuredPort;

  async function handleSave(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!portIsLegal || !portChanged || saving) {
      return;
    }

    setSaving(true);
    setSaveError(null);
    setProbe(null);
    setProbeError(null);
    try {
      // Its own resolved status, not a re-read: the command already restarted
      // the server and knows what happened.
      const next = await setMcpPort(parsedPort);
      setStatus(next);
      setPort(String(next.configuredPort));
    } catch (thrown) {
      setSaveError(toRimaiaError(thrown));
    } finally {
      setSaving(false);
    }
  }

  async function handleCopy() {
    if (!addCommand) {
      return;
    }
    setCopied(false);
    setSaveError(null);
    try {
      await navigator.clipboard.writeText(addCommand);
      setCopied(true);
    } catch {
      // No backend command backs this — the only failure mode is the browser
      // Clipboard API refusing, and the command is left selectable so a refusal
      // is never a dead end.
      setSaveError({
        code: "internal",
        message: "could not copy the command to the clipboard",
      });
    }
  }

  async function handleTest() {
    setTesting(true);
    setProbe(null);
    setProbeError(null);
    try {
      setProbe(await testMcpConnection());
    } catch (thrown) {
      setProbeError(toRimaiaError(thrown));
    } finally {
      setTesting(false);
    }
  }

  return (
    <section className="panel mcp-section">
      <h3>MCP</h3>
      {statusError && <ErrorBanner error={statusError} onDismiss={() => setStatusError(null)} />}

      {status === null && !statusError && <p className="muted">Reading…</p>}

      {status !== null && (
        <>
          {status.state === "port_in_use" && (
            <p className="mcp-warning">
              Port {status.configuredPort} is already in use — Rimaia started without its MCP
              server. {status.message}
            </p>
          )}
          {status.state === "stopped" && (
            <p className="muted">
              The MCP server is not running.{status.message ? ` ${status.message}` : ""}
            </p>
          )}

          {listening && url && addCommand && (
            <>
              <dl className="detail-list">
                <dt>Status</dt>
                <dd>Listening</dd>
                <dt>URL</dt>
                <dd>
                  <code>{url}</code>
                </dd>
              </dl>
              <p className="muted">Register it once, in any terminal:</p>
              <code className="mcp-command">{addCommand}</code>
            </>
          )}

          <div className="mcp-actions">
            <button type="button" onClick={handleCopy} disabled={!listening}>
              {copied ? "Copied" : "Copy command"}
            </button>
            <button type="button" onClick={handleTest} disabled={!listening || testing}>
              {testing ? "Testing…" : "Test connection"}
            </button>
          </div>
          {!listening && (
            <p className="mcp-locked muted">
              Copy and Test connection are unavailable until the server is listening — there is
              no address to hand a client.
            </p>
          )}

          {probe && (
            <p role="status">
              Answered in {probe.latencyMs} ms — {probe.serverName} {probe.protocolVersion},{" "}
              {probe.toolCount} tools.
            </p>
          )}
          {probeError && <ErrorBanner error={probeError} onDismiss={() => setProbeError(null)} />}

          {/* A form, not commit-on-blur: writing this restarts the server and
              invalidates a URL the user may already have registered, so a stray
              blur halfway through typing "45" must not fire it. */}
          <form className="mcp-port-form" onSubmit={handleSave}>
            <label htmlFor="mcp-port">Port</label>
            <input
              id="mcp-port"
              type="number"
              min={LOWEST_PORT}
              max={HIGHEST_PORT}
              value={port}
              disabled={saving}
              onChange={(event) => setPort(event.target.value)}
            />
            <button type="submit" disabled={saving || !portIsLegal || !portChanged}>
              Save
            </button>
            {saving && <span className="muted">Restarting…</span>}
          </form>
          {saveError && <ErrorBanner error={saveError} onDismiss={() => setSaveError(null)} />}
        </>
      )}
    </section>
  );
}
