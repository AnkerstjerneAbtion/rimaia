import { describe, expect, it } from "vitest";

import {
  dismissalFor,
  dismissedProblems,
  hasBlockingProblem,
  isStale,
  matchesDismissal,
  problems,
  resultsFor,
  statusLabel,
  worstStatus,
} from "./doctor";
import type {
  DoctorCheck,
  DoctorCheckResult,
  DoctorDismissal,
  DoctorReport,
  DoctorStatus,
} from "../types";

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
    dismissed: false,
    ...overrides,
  };
}

function report(...results: DoctorCheckResult[]): DoctorReport {
  return { results, dismissals: results.filter((row) => row.dismissed).map(dismissalFor) };
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

  // Task 027. This is the one place that decides which rows the banner shows,
  // which is why the filtering lands here rather than in the banner itself.
  it("drops a dismissed warning, and keeps every other row", () => {
    const shown = problems(
      report(
        result("github_cli", "warn", { dismissed: true }),
        result("mcp_port", "warn"),
        result("claude_cli", "fail"),
      ),
    );

    expect(shown.map((row) => row.check)).toEqual(["claude_cli", "mcp_port"]);
  });
});

describe("dismissals", () => {
  // A dismissal is an answer to a specific sentence, not a mute button on a
  // check: the same check about a different repository is a different warning.
  it("keys on check, repository and detail together", () => {
    const row = result("github_cli", "warn", { repository: "rimaia" });
    const dismissal = dismissalFor(row);

    expect(dismissal).toEqual({
      check: "github_cli",
      repository: "rimaia",
      detail: "github_cli is warn",
    });
    expect(matchesDismissal(row, dismissal)).toBe(true);
    expect(matchesDismissal({ ...row, repository: "other" }, dismissal)).toBe(false);
    expect(matchesDismissal({ ...row, detail: "a newer sentence" }, dismissal)).toBe(false);
  });

  it("lists the rows the user has put down, for Settings to give back", () => {
    const dismissed = dismissedProblems(
      report(result("github_cli", "warn", { dismissed: true }), result("mcp_port", "warn")),
    );

    expect(dismissed.map((row) => row.check)).toEqual(["github_cli"]);
  });

  // The leak task 027 names: an entry that outlived the row it answered would
  // otherwise be invisible *and* permanent.
  it("finds a dismissal that no longer matches any row on the report", () => {
    const current = report(result("mcp_port", "warn", { dismissed: true }));
    const gone: DoctorDismissal = {
      check: "github_cli",
      repository: "rimaia",
      detail: "an older sentence",
    };

    expect(isStale(current, dismissalFor(current.results[0]))).toBe(false);
    expect(isStale(current, gone)).toBe(true);
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

  // Task 027's load-bearing assertion, in the frontend's own terms: dismissal
  // is presentation, and it may not move this answer. `fail` is not dismissible
  // at all, so the only way to get here is a hand-written report — which is
  // exactly the case worth pinning, because the mistake it guards against is
  // deriving this from the filtered list.
  it("still blocks when every row on the report has been dismissed", () => {
    const dismissed = report(
      result("claude_cli", "fail", { dismissed: true }),
      result("github_cli", "warn", { dismissed: true }),
    );

    expect(problems(dismissed)).toEqual([]);
    expect(hasBlockingProblem(dismissed)).toBe(true);
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
