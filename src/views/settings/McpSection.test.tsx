import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { McpSection } from "./McpSection";
import type { McpProbe, McpStatus } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts` or `lib/events.ts` — see
// `StorageSection.test.tsx`'s comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function listening(overrides: Partial<McpStatus> = {}): McpStatus {
  return {
    state: "listening",
    configuredPort: 4517,
    boundAddress: "127.0.0.1:4517",
    message: null,
    ...overrides,
  };
}

function probe(overrides: Partial<McpProbe> = {}): McpProbe {
  return {
    endpoint: "http://127.0.0.1:4517/mcp",
    latencyMs: 7,
    serverName: "rimaia",
    protocolVersion: "2025-11-25",
    toolCount: 10,
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("McpSection", () => {
  it("renders the bound URL and the claude mcp add command once get_mcp_status resolves", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);

    expect(await screen.findByText("http://127.0.0.1:4517/mcp")).toBeInTheDocument();
    expect(
      screen.getByText("claude mcp add --transport http rimaia http://127.0.0.1:4517/mcp"),
    ).toBeInTheDocument();
    expect(screen.getByText("Listening")).toBeInTheDocument();
  });

  it("builds the URL from the address the server actually bound, not from the configured port", async () => {
    // The case the whole panel exists for: the two disagree, and the command
    // the user copies has to name the port a client can actually reach.
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") {
        return listening({ configuredPort: 4517, boundAddress: "127.0.0.1:4599" });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);

    expect(await screen.findByText("http://127.0.0.1:4599/mcp")).toBeInTheDocument();
    expect(
      screen.getByText("claude mcp add --transport http rimaia http://127.0.0.1:4599/mcp"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/4517\/mcp/)).not.toBeInTheDocument();
  });

  it("copies the claude mcp add command to the clipboard without a backend round trip", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Copy command" }));

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(
        "claude mcp add --transport http rimaia http://127.0.0.1:4517/mcp",
      ),
    );
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("shows an error when the clipboard write itself fails", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    Object.assign(navigator, { clipboard: { writeText } });
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Copy command" }));

    expect(
      await screen.findByText("could not copy the command to the clipboard"),
    ).toBeInTheDocument();
  });

  it("renders the port-in-use failure with the backend's own message instead of an empty panel", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") {
        return {
          state: "port_in_use",
          configuredPort: 4599,
          boundAddress: null,
          message:
            "the MCP server could not start: port 4599 on 127.0.0.1 is already in use. " +
            "Another Rimaia window, or another program, is listening on it. " +
            "Change the port in Settings → MCP, or quit whatever is using it.",
        } satisfies McpStatus;
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);

    expect(await screen.findByText(/Port 4599 is already in use/)).toBeInTheDocument();
    expect(screen.getByText(/quit whatever is using it/)).toBeInTheDocument();
  });

  it("disables Copy and Test connection, with a stated reason, while the server is not listening", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") {
        return { state: "stopped", configuredPort: 4517, boundAddress: null, message: null };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);

    expect(await screen.findByRole("button", { name: "Copy command" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Test connection" })).toBeDisabled();
    expect(screen.getByText(/no address to hand a client/)).toBeInTheDocument();
  });

  it("leaves the server alone while the port field is edited but not submitted", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);
    const input = await screen.findByLabelText("Port");
    fireEvent.change(input, { target: { value: "45" } });
    fireEvent.blur(input);

    // The reason this is a form and not commit-on-blur: "45" is a privileged
    // port, and half a keystroke should never restart a server.
    expect(mockInvoke).not.toHaveBeenCalledWith("set_mcp_port", expect.anything());
  });

  it("calls set_mcp_port with the submitted port and shows the new URL from its own result", async () => {
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_mcp_status") return listening();
      if (command === "set_mcp_port") {
        expect(args).toEqual({ port: 4600 });
        return listening({ configuredPort: 4600, boundAddress: "127.0.0.1:4600" });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);
    fireEvent.change(await screen.findByLabelText("Port"), { target: { value: "4600" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("http://127.0.0.1:4600/mcp")).toBeInTheDocument();
  });

  it("refuses to submit a port outside the legal range without calling the backend", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);
    fireEvent.change(await screen.findByLabelText("Port"), { target: { value: "80" } });

    // A JSON number outside `0..=65535` fails serde inside Tauri and comes back
    // as a bare string rather than a RimaiaError, so the form must not send one
    // — and 80 is privileged besides.
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(mockInvoke).not.toHaveBeenCalledWith("set_mcp_port", expect.anything());
  });

  it("keeps Save disabled while the port is unchanged", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);

    await screen.findByText("http://127.0.0.1:4517/mcp");
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("renders the backend's rejection message when set_mcp_port fails", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      if (command === "set_mcp_port") {
        return Promise.reject({
          code: "invalid",
          message:
            "port 80 is not usable: ports below 1024 need privileges Rimaia does not have. " +
            "Pick a port between 1024 and 65535.",
        });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);
    fireEvent.change(await screen.findByLabelText("Port"), { target: { value: "4600" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText(/ports below 1024 need privileges/)).toBeInTheDocument();
    // The last known status stays on screen: the panel still knows where the
    // server is, and the refusal did not move it.
    expect(screen.getByText("http://127.0.0.1:4517/mcp")).toBeInTheDocument();
  });

  it("reports the round trip when test_mcp_connection resolves", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      if (command === "test_mcp_connection") return probe();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Test connection" }));

    const answer = await screen.findByRole("status");
    expect(answer).toHaveTextContent("Answered in 7 ms");
    expect(answer).toHaveTextContent("rimaia 2025-11-25");
    expect(answer).toHaveTextContent("10 tools");
  });

  it("renders the probe's own connection error when test_mcp_connection rejects", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") return listening();
      if (command === "test_mcp_connection") {
        return Promise.reject({
          code: "invalid",
          message: "could not reach http://127.0.0.1:4517/mcp: connection refused",
        });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<McpSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Test connection" }));

    expect(await screen.findByText(/connection refused/)).toBeInTheDocument();
  });

  it("re-reads the status when settings:changed fires", async () => {
    // `mcp_port` is a settings key, so any other writer announces itself there
    // — which is the third of the three ways this panel stays fresh without an
    // event of its own.
    let reads = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_mcp_status") {
        reads += 1;
        return listening(
          reads === 1 ? {} : { configuredPort: 4600, boundAddress: "127.0.0.1:4600" },
        );
      }
      throw new Error(`unexpected command: ${command}`);
    });
    let announce: (() => void) | null = null;
    mockListen.mockImplementation(async (event, handler) => {
      if (event === "settings:changed") {
        announce = () => (handler as (payload: unknown) => void)({ payload: null });
      }
      return vi.fn();
    });

    render(<McpSection />);
    await screen.findByText("http://127.0.0.1:4517/mcp");

    await waitFor(() => expect(announce).not.toBeNull());
    announce!();

    expect(await screen.findByText("http://127.0.0.1:4600/mcp")).toBeInTheDocument();
  });

  it("renders the error banner when get_mcp_status rejects", async () => {
    mockInvoke.mockRejectedValue({ code: "internal", message: "the mcp handle mutex is poisoned" });

    render(<McpSection />);

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("the mcp handle mutex is poisoned");
  });
});
