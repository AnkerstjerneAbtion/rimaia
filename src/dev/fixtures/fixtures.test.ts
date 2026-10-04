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

  it("answers an empty review digest in every scenario but the one that shows a night", async () => {
    for (const name of SCENARIO_NAMES) {
      const transports = createFixtureTransports(buildScenario(name));
      const digest = (await transports.command("get_review_digest")) as ReviewDigest;

      expect(Object.keys(digest.totals.counts).sort()).toEqual([
        "blocked",
        "cancelled",
        "completed",
        "failed",
        "interrupted",
        "running",
        "skipped",
        "waiting_retry",
      ]);
      if (name === "review-digest") continue;
      expect([name, digest.entries]).toEqual([name, []]);
      expect(Object.values(digest.totals.counts).every((count) => count === 0)).toBe(true);
    }
  });

  it("seeds a night led by a failure and a blocked chain, with a skipped entry and a run with no cost", async () => {
    const transports = createFixtureTransports(buildScenario("review-digest"));
    const digest = (await transports.command("get_review_digest")) as ReviewDigest;

    expect(digest.entries.map((entry) => entry.outcome).slice(0, 3)).toEqual([
      "failed",
      "blocked",
      "blocked",
    ]);
    expect(digest.entries.some((entry) => entry.outcome === "skipped")).toBe(true);
    expect(digest.entries.some((entry) => entry.runs > 0 && entry.costUsd === null)).toBe(true);
    expect(digest.totals.runs).toBeGreaterThan(0);
  });

  it("puts the task each review scenario is named for first in board order", async () => {
    const expected: Record<string, string> = {
      "review-truncated": "truncated",
      "review-pruned": "pruned",
      "review-no-commits": "none",
      "review-not-recorded": "not_recorded",
    };
    for (const name of Object.keys(expected)) {
      const scenario = buildScenario(name as ScenarioName);
      const transports = createFixtureTransports(scenario);
      const tasks = (await transports.command("list_tasks", { filter: {} })) as Array<{
        id: string;
        column: string;
      }>;
      const first = tasks.find((task) => task.column === "in_review");
      const detail = (await transports.command("get_task", { id: first?.id })) as {
        lastRun: { id: string };
      };
      const run = (await transports.command("get_run", { runId: detail.lastRun.id })) as {
        review: { source: string; bundle?: { patchTruncated: boolean; patchPrunedAt: string | null } | null };
      };
      const shown =
        run.review.source === "not_recorded"
          ? "not_recorded"
          : run.review.bundle === null
            ? "none"
            : run.review.bundle?.patchPrunedAt
              ? "pruned"
              : run.review.bundle?.patchTruncated
                ? "truncated"
                : "whole";
      expect([name, shown]).toEqual([name, expected[name]]);
    }
  });

  it("seeds three dependents, one built on it and one archived, and one dependency for review-chain", async () => {
    const transports = createFixtureTransports(buildScenario("review-chain"));
    const tasks = (await transports.command("list_tasks", { filter: {} })) as Array<{
      id: string;
      column: string;
    }>;
    const first = tasks.find((task) => task.column === "in_review")?.id;
    const dependents = (await transports.command("get_task_dependents", {
      taskId: first,
    })) as Array<{ builtOn: boolean; archivedAt: string | null }>;
    const detail = (await transports.command("get_task", { id: first })) as { dependsOn: string[] };

    expect(dependents).toHaveLength(3);
    expect(dependents.filter((dependent) => dependent.builtOn)).toHaveLength(1);
    expect(dependents.filter((dependent) => dependent.archivedAt !== null)).toHaveLength(1);
    expect(detail.dependsOn).toHaveLength(1);
  });

  it("seeds an empty review queue for review-empty", async () => {
    const transports = createFixtureTransports(buildScenario("review-empty"));
    const tasks = (await transports.command("list_tasks", { filter: {} })) as Array<{
      column: string;
    }>;
    expect(tasks.some((task) => task.column === "in_review")).toBe(false);
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
