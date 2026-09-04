import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { DoctorBanner } from "./DoctorBanner";
import type { DoctorCheckResult, DoctorReport } from "../types";

function report(...results: DoctorCheckResult[]): DoctorReport {
  return { results, dismissals: [] };
}

const passingGit: DoctorCheckResult = {
  check: "git",
  label: "git",
  repository: null,
  status: "pass",
  detail: "git 2.45.0",
  remediation: null,
  dismissed: false,
};

const unauthenticatedGh: DoctorCheckResult = {
  check: "github_cli",
  label: "GitHub CLI",
  repository: "rimaia",
  status: "warn",
  detail: "gh is not authenticated for github.com, used by rimaia",
  remediation: "Run `gh auth login`.",
  dismissed: false,
};

const missingClaude: DoctorCheckResult = {
  check: "claude_cli",
  label: "Claude CLI",
  repository: null,
  status: "fail",
  detail: "claude was not found on PATH",
  remediation: "Install Claude Code and make sure `claude` is on your PATH.",
  dismissed: false,
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
    render(<DoctorBanner report={report(missingClaude)} />);

    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByText("The queue cannot start until these are fixed")).toBeInTheDocument();
    expect(
      screen.getByText("Install Claude Code and make sure `claude` is on your PATH."),
    ).toBeInTheDocument();
  });

  // Task 018: "An unauthenticated gh produces a warning naming the affected
  // repository" — and a warning must not claim the queue is blocked.
  it("names the repository on a warning without claiming the queue is blocked", () => {
    render(<DoctorBanner report={report(unauthenticatedGh)} />);

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

  // Task 027 --------------------------------------------------------------

  it("offers a dismiss control on a warning, and hands back the row it was clicked on", async () => {
    const onDismiss = vi.fn();
    render(<DoctorBanner report={report(unauthenticatedGh)} onDismiss={onDismiss} />);

    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));

    expect(onDismiss).toHaveBeenCalledWith(unauthenticatedGh);
  });

  // The distinction task 027 turns on: a warn is put down, a fail is folded up.
  it("offers no dismiss control on a failure, only a collapse", () => {
    render(<DoctorBanner report={report(missingClaude)} onDismiss={vi.fn()} />);

    expect(screen.queryByRole("button", { name: "Dismiss" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hide 1 blocking problem" })).toBeInTheDocument();
  });

  it("hides an already-dismissed row without being asked to filter", () => {
    render(<DoctorBanner report={report({ ...unauthenticatedGh, dismissed: true })} />);

    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  // "The rows go away, the fact does not."
  it("collapses the failing rows and keeps the blocking count on one line", async () => {
    render(
      <DoctorBanner
        report={report(missingClaude, {
          ...missingClaude,
          check: "git",
          detail: "git 2.20 is too old for worktrees",
        })}
      />,
    );

    expect(screen.getByText("claude was not found on PATH")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Hide 2 blocking problems" }));

    expect(screen.queryByText("claude was not found on PATH")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Show 2 blocking problems" })).toBeInTheDocument();
    expect(
      screen.getByText("The queue cannot start until these are fixed"),
    ).toBeInTheDocument();
  });

  it("stays present, naming the blocking count, once every warning has been dismissed", () => {
    render(
      <DoctorBanner
        report={report(missingClaude, { ...unauthenticatedGh, dismissed: true })}
        onDismiss={vi.fn()}
      />,
    );

    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hide 1 blocking problem" })).toBeInTheDocument();
    expect(
      screen.queryByText("gh is not authenticated for github.com, used by rimaia"),
    ).not.toBeInTheDocument();
  });
});
