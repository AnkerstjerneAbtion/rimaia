import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { WelcomeView } from "./WelcomeView";
import type { DoctorReport, Repository } from "../types";

// Mocked at the Tauri seam, like every other view test here.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function repository(overrides: Partial<Repository> = {}): Repository {
  return {
    id: "repo-1",
    name: "rimaia",
    path: "/src/rimaia",
    defaultBranch: "main",
    worktreeRoot: "/data/worktrees",
    allowUnattendedRuns: false,
    ...overrides,
  } as Repository;
}

const healthy: DoctorReport = { results: [], dismissals: [] };

/** Every command the welcome screen and its embedded controls read on mount. */
function respondWith({
  repositories = [] as Repository[],
  instructions = "",
  doctor = healthy,
} = {}) {
  mockInvoke.mockImplementation(async (command) => {
    switch (command) {
      case "run_doctor":
        return doctor;
      case "list_repositories":
        return repositories;
      case "get_base_instructions":
        return instructions;
      case "get_run_environment":
        return "inherit";
      case "get_mcp_status":
        return {
          state: "listening",
          configuredPort: 4517,
          boundAddress: "127.0.0.1:4517",
          message: null,
        };
      case "dismiss_onboarding":
        return null;
      default:
        throw new Error(`unexpected command: ${String(command)}`);
    }
  });
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("WelcomeView", () => {
  it("walks the four setup steps in order", async () => {
    respondWith();
    const { container } = render(<WelcomeView onFinish={vi.fn()} />);

    await screen.findByRole("heading", { level: 3, name: /Add the MCP server/ });
    // Scoped to the step list: step three embeds the real Instructions
    // section, which brings its own <h3> along with it.
    const headings = container.querySelectorAll(".welcome-step > h3");
    expect([...headings].map((heading) => heading.textContent)).toEqual([
      expect.stringContaining("Register a repository"),
      expect.stringContaining("Enable unattended runs"),
      expect.stringContaining("Set base instructions"),
      expect.stringContaining("Add the MCP server"),
    ]);
  });

  // The rule the whole screen is built on: a step is done when the thing is
  // true, not when someone clicked a button on this screen.
  it("marks steps done from live configuration rather than from clicks", async () => {
    respondWith({
      repositories: [repository({ allowUnattendedRuns: true })],
      instructions: "Always open a pull request.",
    });
    render(<WelcomeView onFinish={vi.fn()} />);

    await waitFor(() => expect(screen.getAllByText("Done").length).toBeGreaterThanOrEqual(3));
    expect(screen.getByText("Enabled for rimaia.")).toBeInTheDocument();
  });

  it("leaves a step to do when its configuration is absent", async () => {
    respondWith({ repositories: [], instructions: "" });
    render(<WelcomeView onFinish={vi.fn()} />);

    // Steps one, two and three are all unsatisfied on a fresh install.
    await waitFor(() => expect(screen.getAllByText("To do")).toHaveLength(3));
  });

  it("shows the live add command rather than a hardcoded port", async () => {
    respondWith();
    render(<WelcomeView onFinish={vi.fn()} />);

    expect(
      await screen.findByText("claude mcp add --transport http rimaia http://127.0.0.1:4517/mcp"),
    ).toBeInTheDocument();
  });

  it("surfaces a failing check against the step it belongs to", async () => {
    respondWith({
      doctor: {
        results: [
          {
            check: "repository_path",
            label: "Repository paths",
            repository: "rimaia",
            status: "fail",
            detail: "/src/rimaia no longer exists",
            remediation: "Re-register the repository at its new location.",
            dismissed: false,
          },
        ],
        dismissals: [],
      },
    });
    render(<WelcomeView onFinish={vi.fn()} />);

    expect(
      await screen.findByText("Re-register the repository at its new location."),
    ).toBeInTheDocument();
  });
});
