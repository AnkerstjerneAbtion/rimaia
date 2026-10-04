import { afterEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import * as commands from "../../lib/commands";
import commandsSource from "../../lib/commands.ts?raw";
import * as events from "../../lib/events";
import type { ReviewDigest, RunState } from "../../types";
import { ANSWERS } from "./answers";
import { FIXTURE_SENTINEL } from "./constants";
import { buildScenario, SCENARIO_NAMES } from "./seed";
import type { Scenario, ScenarioName } from "./seed";
import { createFixtureTransports } from "./transport";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

afterEach(() => {
  commands.setCommandTransport(undefined);
  events.setEventTransport(undefined);
});

/** The same shape `scripts/check-command-wiring.sh` parses. */
function commandNamesSent(): string[] {
  return [...commandsSource.matchAll(/call<[^(]*>\(\s*["']([A-Za-z0-9_]+)["']/g)].map(
    (match) => match[1],
  );
}

describe("fixture answers", () => {
  it("has an answer or an explicit refusal for every command commands.ts sends", () => {
    const sent = commandNamesSent();

    expect(sent.length).toBeGreaterThan(90);
    expect(sent.filter((name) => !(name in ANSWERS))).toEqual([]);
    // The other direction: a row for a command nothing sends is a stale
    // fixture, and a stale fixture is a screen nobody ships.
    expect(Object.keys(ANSWERS).filter((name) => !sent.includes(name))).toEqual([]);
  });

  it("never reaches invoke or listen in fixture mode", async () => {
    const transports = createFixtureTransports(buildScenario("busy"));
    commands.setCommandTransport(transports.command);
    events.setEventTransport(transports.event);

    const wrappers = Object.entries(commands).filter(
      ([name, value]) =>
        typeof value === "function" && name !== "toRimaiaError" && name !== "setCommandTransport",
    ) as Array<[string, () => Promise<unknown>]>;
    const subscribers = Object.entries(events).filter(
      ([name, value]) => typeof value === "function" && name.startsWith("subscribeTo"),
    ) as Array<[string, (callback: () => void) => Promise<unknown>]>;

    const settled = await Promise.allSettled([
      ...wrappers.map(([, wrapper]) => wrapper()),
      ...subscribers.map(([, subscribe]) => subscribe(() => {})),
    ]);

    expect(wrappers.length).toBeGreaterThan(90);
    expect(subscribers.length).toBeGreaterThanOrEqual(7);
    // A refusal is a rejection with a code the UI can render; anything else
    // rejecting means a wrapper threw on the fixture's answer.
    for (const result of settled) {
      if (result.status === "rejected") {
        expect(["invalid", "internal", "not_found"]).toContain(result.reason?.code);
      }
    }
    expect(vi.mocked(invoke)).not.toHaveBeenCalled();
    expect(vi.mocked(listen)).not.toHaveBeenCalled();
  });

  it("answers the review verdicts without changing the seed", async () => {
    const transports = createFixtureTransports(buildScenario("busy"));
    const before = await transports.command("list_tasks", { filter: {} });
    const inReview = (before as Array<{ id: string; column: string }>).find(
      (task) => task.column === "in_review",
    );
    expect(inReview).toBeDefined();
    const taskId = inReview?.id;

    await transports.command("approve_task", { taskId });
    await transports.command("reject_task", { taskId, note: "No." });
    await transports.command("request_task_changes", { taskId, note: "Fix." });

    expect(await transports.command("list_tasks", { filter: {} })).toEqual(before);
  });

  it("answers an empty review digest in every scenario", async () => {
    for (const name of SCENARIO_NAMES) {
      const transports = createFixtureTransports(buildScenario(name));
      const digest = (await transports.command("get_review_digest")) as ReviewDigest;

      expect([name, digest.entries]).toEqual([name, []]);
      expect(Object.keys(digest.totals.counts).sort()).toEqual(
        [
          "blocked",
          "cancelled",
          "completed",
          "failed",
          "interrupted",
          "running",
          "skipped",
          "waiting_retry",
        ],
      );
      expect(Object.values(digest.totals.counts).every((count) => count === 0)).toBe(true);
    }
  });

  it("rejects a command with no row instead of hanging", async () => {
    const transports = createFixtureTransports(buildScenario("empty"));

    await expect(transports.command("not_a_command")).rejects.toEqual({
      code: "internal",
      message: "fixture mode has no answer for `not_a_command`",
    });
  });
});

const RUN_STATES: RunState[] = [
  "idle",
  "queued",
  "running",
  "blocked",
  "waiting_retry",
  "failed",
  "cancelled",
];

function runningRuns(scenario: Scenario) {
  return scenario.runs.filter((run) => run.status === "running");
}

describe("fixture seed", () => {
  const scenarios = new Map(
    SCENARIO_NAMES.map((name) => [name, buildScenario(name)] as [ScenarioName, Scenario]),
  );
  const busy = scenarios.get("busy") as Scenario;

  it("seeds every state the scope lists", () => {
    const states = new Set(busy.tasks.map((task) => task.runState));
    for (const state of RUN_STATES) expect(states).toContain(state);

    const perColumn = (column: string) => busy.tasks.filter((task) => task.column === column).length;
    expect(perColumn("not_ready")).toBeGreaterThan(1);
    expect(perColumn("in_review")).toBeGreaterThan(1);
    expect(perColumn("done")).toBe(1);
    expect(perColumn("ready")).toBe(20);

    expect(busy.tasks.some((task) => task.lastRun?.status === "interrupted")).toBe(true);
    expect(busy.tasks.some((task) => (task.blockingTitle ?? "").length > 40)).toBe(true);
    expect(busy.tasks.some((task) => task.title.length >= 80)).toBe(true);
    expect(busy.tasks.some((task) => task.title.includes(FIXTURE_SENTINEL))).toBe(true);

    expect(runningRuns(busy)).toHaveLength(3);
    expect(runningRuns(scenarios.get("one-run") as Scenario)).toHaveLength(1);
    expect(runningRuns(scenarios.get("two-runs") as Scenario)).toHaveLength(2);
    expect(scenarios.get("two-runs")?.capacity.mode).toBe("parallel");

    const results = busy.doctor.results;
    for (const status of ["pass", "warn", "fail"] as const) {
      expect(results.some((result) => result.status === status)).toBe(true);
    }
    expect(results.some((result) => result.status === "warn" && result.dismissed)).toBe(true);
    expect(results.some((result) => result.status === "warn" && !result.dismissed)).toBe(true);

    const empty = scenarios.get("empty") as Scenario;
    expect(empty.tasks).toEqual([]);
    expect(empty.runs).toEqual([]);
    expect(scenarios.get("welcome")?.appInfo.onboardingDismissed).toBe(false);
    expect(scenarios.get("error")?.runsReadError).not.toBeNull();
  });

  it("admits exactly the runs in flight, in every scenario that has any", () => {
    for (const [name, scenario] of scenarios) {
      const running = runningRuns(scenario);
      if (running.length === 0) continue;

      const runningTaskIds = scenario.tasks
        .filter((task) => task.runState === "running")
        .map((task) => task.id)
        .sort();
      expect([name, scenario.queueStatus.runningTaskIds.slice().sort()]).toEqual([
        name,
        runningTaskIds,
      ]);
      expect(runningTaskIds).toEqual(running.map((run) => run.taskId).sort());
      expect(scenario.capacity.maxConcurrency).toBeGreaterThanOrEqual(running.length);
      expect(scenario.tails.map((tail) => tail.runId).sort()).toEqual(
        running.map((run) => run.id).sort(),
      );

      // D21: every repository's own cap admits what is running in it.
      for (const repository of scenario.repositories) {
        const inRepository = running.filter((run) => run.repositoryId === repository.id);
        expect(repository.maxConcurrency).toBeGreaterThanOrEqual(inRepository.length);
      }
    }
    expect(busy.capacity.mode).toBe("parallel");
    expect(busy.capacity.maxConcurrency).toBeGreaterThanOrEqual(3);
  });
});
