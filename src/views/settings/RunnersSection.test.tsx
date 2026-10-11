import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { installTransports } from "../../lib/client";
import { ClientCapabilitiesProvider } from "../../lib/capabilities";
import { BROWSER } from "../../test/capabilities";
import { mockHttp } from "../../test/http";
import type { HttpMock } from "../../test/http";
import { runner } from "../../test/web";
import { SettingsView } from "../SettingsView";
import { RunnersSection } from "./RunnersSection";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const NOW = new Date("2026-10-04T09:00:00Z");
let http: HttpMock;

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  http = mockHttp();
  http.answer("list_runners", [
    runner(),
    runner({
      id: "runner-ci",
      label: "Build box",
      appVersion: null,
      eligibility: "assigned_then_pool",
      lastSeenAt: null,
    }),
  ]);
  installTransports("browser");
});

afterEach(() => {
  cleanup();
  installTransports("solo");
  vi.unstubAllGlobals();
});

describe("RunnersSection", () => {
  it("shows the board's own facts about each runner", async () => {
    render(<RunnersSection now={NOW} />);

    expect(await screen.findByRole("heading", { name: "Studio Mac" })).toBeInTheDocument();
    expect(screen.getByText("v0.5.2")).toBeInTheDocument();
    expect(screen.getByText("Takes tasks assigned to you")).toBeInTheDocument();
    expect(screen.getByText("4m ago")).toBeInTheDocument();
    expect(screen.getByText("Takes tasks assigned to you, then the team pool")).toBeInTheDocument();
    expect(screen.getByText("Not reported")).toBeInTheDocument();
    expect(screen.getByText("never")).toBeInTheDocument();
  });

  it("says where each runner's settings are changed", async () => {
    render(<RunnersSection now={NOW} />);

    expect(
      await screen.findByText("Studio Mac's settings are changed on that machine, not here."),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Build box's settings are changed on that machine, not here."),
    ).toBeInTheDocument();
    // The board cannot tell a desktop runner from a headless one (task 058).
    expect(document.body.textContent).not.toMatch(/desktop app|command line|rimaia-runner/i);
  });

  it("says a runner has not reported a doctor result", async () => {
    render(<RunnersSection now={NOW} />);

    expect(await screen.findByText("Studio Mac has not reported a doctor result yet.")).toBeInTheDocument();
    expect(screen.getByText("Build box has not reported a doctor result yet.")).toBeInTheDocument();
  });

  it("shows no runner setting's value", async () => {
    render(
      <ClientCapabilitiesProvider capabilities={BROWSER}>
        <SettingsView />
      </ClientCapabilitiesProvider>,
    );
    await screen.findByRole("heading", { name: "Studio Mac" });

    // ADR-0034's 2026-10-04 amendment, held by a test: the board holds no
    // runner setting, so the section reads none and renders none.
    const asked = http.requests.map((request) => request.name);
    for (const local of ["get_run_environment", "get_queue_status", "get_run_capacity", "list_schedules", "get_mcp_status"]) {
      expect(asked, local).not.toContain(local);
    }
    expect(vi.mocked(invoke)).not.toHaveBeenCalled();
    expect(screen.queryByText(/Runs at once|Schedules|Concurrency|MCP|Run environment|Storage|Doctor/)).toBeNull();
    expect(document.querySelector("#settings-runners")).not.toBeNull();
  });
});
