import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { OnArchiveFields } from "./OnArchiveFields";
import type { Repository } from "../../types";

function repository(overrides: Partial<Repository> = {}): Repository {
  return {
    id: "repo-1",
    name: "rimaia",
    path: "/code/rimaia",
    defaultBranch: "main",
    worktreeRoot: "/data/worktrees/rimaia",
    allowUnattendedRuns: true,
    maxConcurrency: 1,
    createdAt: "2026-08-20T09:00:00Z",
    onArchive: "none",
    onArchiveScript: null,
    ...overrides,
  };
}

describe("OnArchiveFields", () => {
  it("offers the three states as radios, so two can never be chosen at once", () => {
    // The exclusivity is the schema's, and the form draws it rather than
    // enforcing it behind the user's back (ADR-0025 point 4).
    render(<OnArchiveFields repository={repository()} onChange={vi.fn()} />);

    const radios = screen.getAllByRole("radio");
    expect(radios).toHaveLength(3);
    expect(radios.filter((radio) => (radio as HTMLInputElement).checked)).toHaveLength(1);
  });

  it("applies 'leave everything alone' immediately — turning cleanup off needs no gate", () => {
    const onChange = vi.fn();
    render(
      <OnArchiveFields
        repository={repository({ onArchive: "remove_worktree" })}
        onChange={onChange}
      />,
    );

    fireEvent.click(screen.getByRole("radio", { name: "Leave everything alone" }));

    expect(onChange).toHaveBeenCalledWith("none", null);
  });

  it("does not store worktree deletion until the user acknowledges what it deletes", () => {
    const onChange = vi.fn();
    render(<OnArchiveFields repository={repository()} onChange={onChange} />);

    fireEvent.click(screen.getByRole("radio", { name: "Delete the task's worktree" }));

    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
    expect(screen.getByText(/including any uncommitted file/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /I understand/ }));
    expect(onChange).toHaveBeenCalledWith("remove_worktree", null);
  });

  it("cancelling the acknowledgement puts the radio back where it was", () => {
    const onChange = vi.fn();
    render(<OnArchiveFields repository={repository()} onChange={onChange} />);

    fireEvent.click(screen.getByRole("radio", { name: "Delete the task's worktree" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(onChange).not.toHaveBeenCalled();
    expect(
      (screen.getByRole("radio", { name: "Leave everything alone" }) as HTMLInputElement)
        .checked,
    ).toBe(true);
  });

  it("will not save the script mode with an empty path", () => {
    // The service refuses `script` without one, and a radio that flipped and
    // then errored would be showing a state the database never reached.
    render(<OnArchiveFields repository={repository()} onChange={vi.fn()} />);

    fireEvent.click(screen.getByRole("radio", { name: "Run my own script" }));

    expect(screen.getByRole("button", { name: "Save script" })).toBeDisabled();
  });

  it("sends the mode and the path together", () => {
    const onChange = vi.fn();
    render(<OnArchiveFields repository={repository()} onChange={onChange} />);

    fireEvent.click(screen.getByRole("radio", { name: "Run my own script" }));
    fireEvent.change(screen.getByLabelText("Script to run"), {
      target: { value: "  /opt/teardown.sh  " },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save script" }));

    expect(onChange).toHaveBeenCalledWith("script", "/opt/teardown.sh");
  });

  it("warns that a script gives up every guard Rimaia has", () => {
    render(
      <OnArchiveFields
        repository={repository({ onArchive: "script", onArchiveScript: "/opt/teardown.sh" })}
        onChange={vi.fn()}
      />,
    );

    expect(screen.getByText(/applies none of its own guards/)).toBeInTheDocument();
    expect(screen.getByText(/not a command line/)).toBeInTheDocument();
  });

  it("repaints from the stored row when another writer changes it", () => {
    // The MCP server is a supported writer of the same row (ADR-0006), so the
    // form follows the database rather than its own last click.
    const { rerender } = render(
      <OnArchiveFields repository={repository()} onChange={vi.fn()} />,
    );

    rerender(
      <OnArchiveFields
        repository={repository({ onArchive: "script", onArchiveScript: "/opt/elsewhere.sh" })}
        onChange={vi.fn()}
      />,
    );

    expect(
      (screen.getByRole("radio", { name: "Run my own script" }) as HTMLInputElement).checked,
    ).toBe(true);
    expect(screen.getByLabelText("Script to run")).toHaveValue("/opt/elsewhere.sh");
  });
});
