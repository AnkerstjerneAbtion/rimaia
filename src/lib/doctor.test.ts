import { describe, expect, it } from "vitest";

import { hasBlockingProblem, problems, resultsFor, statusLabel, worstStatus } from "./doctor";
import type { DoctorCheck, DoctorCheckResult, DoctorReport, DoctorStatus } from "../types";

function result(
  check: DoctorCheck,
  status: DoctorStatus,
  overrides: Partial<DoctorCheckResult> = {},
): DoctorCheckResult {
  return {
    check,
    label: check,
    repository: null,
    status,
    detail: `${check} is ${status}`,
    remediation: status === "pass" ? null : `fix ${check}`,
    ...overrides,
  };
}

function report(...results: DoctorCheckResult[]): DoctorReport {
  return { results };
}

describe("problems", () => {
  it("drops passing rows so a healthy installation shows no banner", () => {
    expect(problems(report(result("git", "pass"), result("claude_cli", "pass")))).toEqual([]);
  });

  it("orders failures before warnings regardless of the order they were collected in", () => {
    const ordered = problems(
      report(
        result("github_cli", "warn"),
        result("claude_cli", "fail"),
        result("git", "pass"),
        result("data_directory", "fail"),
      ),
    );

    expect(ordered.map((row) => row.check)).toEqual([
      "claude_cli",
      "data_directory",
      "github_cli",
    ]);
  });

  it("treats a missing report as nothing to show rather than as a problem", () => {
    expect(problems(null)).toEqual([]);
    expect(hasBlockingProblem(null)).toBe(false);
  });
});

describe("hasBlockingProblem", () => {
  it("is true when any check fails", () => {
    expect(hasBlockingProblem(report(result("git", "pass"), result("disk_space", "fail")))).toBe(
      true,
    );
  });

  // Task 018's contract: "Fails block queue start. Warnings do not."
  it("is false when the worst row is a warning", () => {
    expect(hasBlockingProblem(report(result("github_cli", "warn")))).toBe(false);
  });
});

describe("resultsFor", () => {
  it("selects only the checks a welcome step owns", () => {
    const selected = resultsFor(
      report(result("mcp_port", "pass"), result("repository_path", "fail")),
      ["repository_path"],
    );

    expect(selected.map((row) => row.check)).toEqual(["repository_path"]);
  });
});

describe("worstStatus", () => {
  it("reports the worst status present", () => {
    expect(worstStatus([result("git", "pass"), result("github_cli", "warn")])).toBe("warn");
    expect(worstStatus([result("github_cli", "warn"), result("claude_cli", "fail")])).toBe("fail");
  });

  // A step whose checks have not run is not a step that passed — rendering it
  // as done is exactly the click-counting the welcome flow avoids.
  it("distinguishes 'no checks ran' from 'every check passed'", () => {
    expect(worstStatus([])).toBeNull();
    expect(worstStatus([result("git", "pass")])).toBe("pass");
  });
});

describe("statusLabel", () => {
  it("calls a failure blocked, because that is what it does to the queue", () => {
    expect(statusLabel("fail")).toBe("Blocked");
    expect(statusLabel("warn")).toBe("Warning");
    expect(statusLabel("pass")).toBe("OK");
  });
});
