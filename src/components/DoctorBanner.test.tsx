import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { DoctorBanner } from "./DoctorBanner";
import type { DoctorCheckResult, DoctorReport } from "../types";

function report(...results: DoctorCheckResult[]): DoctorReport {
  return { results };
}

const passingGit: DoctorCheckResult = {
  check: "git",
  label: "git",
  repository: null,
  status: "pass",
  detail: "git 2.45.0",
  remediation: null,
};

describe("DoctorBanner", () => {
  it("renders nothing when every check passes", () => {
    const { container } = render(<DoctorBanner report={report(passingGit)} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing before the first report arrives", () => {
    const { container } = render(<DoctorBanner report={null} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("says the queue is blocked, and shows the remediation, when a check fails", () => {
    render(
      <DoctorBanner
        report={report({
          check: "claude_cli",
          label: "Claude CLI",
          repository: null,
          status: "fail",
          detail: "claude was not found on PATH",
          remediation: "Install Claude Code and make sure `claude` is on your PATH.",
        })}
      />,
    );

    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByText("The queue cannot start until these are fixed")).toBeInTheDocument();
    expect(
      screen.getByText("Install Claude Code and make sure `claude` is on your PATH."),
    ).toBeInTheDocument();
  });

  // Task 018: "An unauthenticated gh produces a warning naming the affected
  // repository" — and a warning must not claim the queue is blocked.
  it("names the repository on a warning without claiming the queue is blocked", () => {
    render(
      <DoctorBanner
        report={report({
          check: "github_cli",
          label: "GitHub CLI",
          repository: "rimaia",
          status: "warn",
          detail: "gh is not authenticated for github.com, used by rimaia",
          remediation: "Run `gh auth login`.",
        })}
      />,
    );

    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(
      screen.getByText("The queue can start, but these will limit it"),
    ).toBeInTheDocument();
    // Named in the heading as well as in the detail, so a machine with several
    // repositories does not show four identical "GitHub CLI" rows.
    expect(screen.getByRole("heading", { level: 4 })).toHaveTextContent("GitHub CLI — rimaia");
    expect(
      screen.getByText("gh is not authenticated for github.com, used by rimaia"),
    ).toBeInTheDocument();
  });
});
