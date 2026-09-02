import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { StorageSection } from "./StorageSection";

// `commands.ts` is the only module that imports `invoke`; mocking it here
// exercises the real call path (including `toRimaiaError`) instead of
// stubbing the frontend's own command wrappers.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

const APP_INFO = {
  appVersion: "0.1.0",
  dataDir: "/home/user/.local/share/rimaia",
  dbFile: "/home/user/.local/share/rimaia/rimaia.db",
  logsDir: "/home/user/.local/share/rimaia/logs",
};

/** One worktree, clean and merged — the case every guard passes. */
const CLEAN_WORKTREE = {
  taskId: "task-1",
  taskTitle: "Add the parser",
  repositoryId: "repo-1",
  repositoryName: "rimaia",
  column: "done",
  runState: "idle",
  path: "/data/worktrees/rimaia/task-1",
  exists: true,
  branch: "rimaia/task-1-add-the-parser",
  baseRef: "main",
  sizeBytes: 2_097_152,
  lastActivity: "2026-08-20T12:00:00Z",
  merged: true,
  uncommittedChanges: 0,
  unpushedCommits: 0,
  live: false,
};

/**
 * Answers the four reads the panel makes on mount, with whatever `overrides`
 * says on top.
 *
 * A helper rather than four `if`s in every test, because a test that forgets
 * one of them fails with "unexpected command" from a mount rather than from the
 * thing it is actually about.
 */
function mockReads(overrides: Record<string, unknown> = {}) {
  const base: Record<string, unknown> = {
    get_app_info: APP_INFO,
    get_run_log_size: 0,
    get_worktree_inventory: { entries: [], totalBytes: 0 },
    get_worktree_auto_cleanup: "off",
    ...overrides,
  };
  mockInvoke.mockImplementation(async (command) => {
    if (command in base) return base[String(command)];
    throw new Error(`unexpected command: ${String(command)}`);
  });
  return base;
}

beforeEach(() => {
  mockInvoke.mockReset();
});

describe("StorageSection", () => {
  it("renders the data, database and logs paths once get_app_info resolves", async () => {
    mockReads();

    render(<StorageSection />);

    expect(await screen.findByText("/home/user/.local/share/rimaia")).toBeInTheDocument();
    expect(screen.getByText("/home/user/.local/share/rimaia/rimaia.db")).toBeInTheDocument();
    expect(screen.getByText("/home/user/.local/share/rimaia/logs")).toBeInTheDocument();
  });

  it("renders the error message inside the ErrorBanner when get_app_info rejects", async () => {
    mockInvoke.mockRejectedValue({ code: "io", message: "app data directory unreadable" });

    render(<StorageSection />);

    const banner = await screen.findAllByRole("alert");
    expect(banner[0]).toHaveTextContent("app data directory unreadable");
  });

  it("renders the total run-log size once get_run_log_size resolves", async () => {
    mockReads({ get_run_log_size: 1536 });

    render(<StorageSection />);

    expect(await screen.findByText("1.5 KB")).toBeInTheDocument();
  });

  it("prunes logs older than the chosen preset and refreshes the reported size", async () => {
    let sizeAfterPrune = 5_000_000;
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return sizeAfterPrune;
      if (command === "get_worktree_inventory") return { entries: [], totalBytes: 0 };
      if (command === "get_worktree_auto_cleanup") return "off";
      if (command === "prune_run_logs") {
        expect(args).toEqual({ criterion: { kind: "older_than_days", days: 30 } });
        sizeAfterPrune = 1_000_000;
        return { runsPruned: 3, strategyTranscriptsPruned: 0, bytesFreed: 4_000_000 };
      }
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    await screen.findByText("4.8 MB");

    fireEvent.click(screen.getByRole("button", { name: "Older than 30 days" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete transcripts" }));

    expect(await screen.findByText(/Removed 3 logs, freed 3\.8 MB\./)).toBeInTheDocument();
    // The reported size refreshes to what `get_run_log_size` now answers,
    // not merely to "previous minus bytesFreed" computed client-side.
    expect(await screen.findByText("976.6 KB")).toBeInTheDocument();
  });

  it("names the planner transcripts a prune reclaimed alongside the run logs", async () => {
    // Seam-contract D17.5/D19: a `strategy-<uuid>.jsonl` has no `runs` row, so
    // it is counted separately — reporting "4 logs" for a database holding
    // three would be the wrong sentence about the right bytes.
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      if (command === "get_worktree_inventory") return { entries: [], totalBytes: 0 };
      if (command === "get_worktree_auto_cleanup") return "off";
      if (command === "prune_run_logs")
        return { runsPruned: 3, strategyTranscriptsPruned: 1, bytesFreed: 4_000_000 };
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Older than 7 days" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete transcripts" }));

    expect(
      await screen.findByText(/Removed 3 logs and 1 planner transcript, freed 3\.8 MB\./),
    ).toBeInTheDocument();
  });

  // Picking the preset only *proposes* the deletion — this one spans every
  // task on the board, so the confirm gate matters more here than anywhere.
  it("prunes nothing when the confirmation is cancelled", async () => {
    const commands: string[] = [];
    mockInvoke.mockImplementation(async (command) => {
      commands.push(String(command));
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 5_000_000;
      if (command === "get_worktree_inventory") return { entries: [], totalBytes: 0 };
      if (command === "get_worktree_auto_cleanup") return "off";
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    await screen.findByText("4.8 MB");

    fireEvent.click(screen.getByRole("button", { name: "Older than 30 days" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(commands).not.toContain("prune_run_logs");
    expect(screen.getByRole("button", { name: "Older than 30 days" })).toBeInTheDocument();
  });

  it("reports worktree disk usage alongside the run-log total", async () => {
    // Task 016's last scope bullet: one question ("why is this directory so
    // large") answered on one screen.
    mockReads({
      get_run_log_size: 1024,
      get_worktree_inventory: { entries: [CLEAN_WORKTREE], totalBytes: 2_097_152 },
    });

    render(<StorageSection />);

    expect(await screen.findByText("1.0 KB")).toBeInTheDocument();
    expect(await screen.findByText("2.0 MB across 1 worktree")).toBeInTheDocument();
  });

  it("lists each worktree with its task, branch and merged state", async () => {
    mockReads({
      get_worktree_inventory: {
        entries: [
          CLEAN_WORKTREE,
          {
            ...CLEAN_WORKTREE,
            taskId: "task-2",
            taskTitle: "Wire the board",
            branch: "rimaia/task-2-wire-the-board",
            merged: false,
            sizeBytes: 512,
          },
        ],
        totalBytes: 2_097_664,
      },
    });

    render(<StorageSection />);

    expect(await screen.findByText("Add the parser")).toBeInTheDocument();
    expect(screen.getByText("rimaia/task-1-add-the-parser")).toBeInTheDocument();
    expect(screen.getByText("merged into main")).toBeInTheDocument();
    expect(screen.getByText("Wire the board")).toBeInTheDocument();
    expect(screen.getByText("not merged into main")).toBeInTheDocument();
  });

  it("shows the uncommitted and unpushed counts that decide whether removing is safe", async () => {
    mockReads({
      get_worktree_inventory: {
        entries: [{ ...CLEAN_WORKTREE, uncommittedChanges: 3, unpushedCommits: 1, merged: false }],
        totalBytes: 2_097_152,
      },
    });

    render(<StorageSection />);

    expect(
      await screen.findByText("3 uncommitted changes, 1 commit no remote has."),
    ).toBeInTheDocument();
  });

  it("offers no removal at all for a worktree a run is working in", async () => {
    // The one guard with no override, surfaced: the button is not disabled,
    // it is absent, and the sentence says why.
    mockReads({
      get_worktree_inventory: {
        entries: [{ ...CLEAN_WORKTREE, runState: "running", live: true }],
        totalBytes: 2_097_152,
      },
    });

    render(<StorageSection />);
    await screen.findByText("Add the parser");

    expect(screen.queryByRole("button", { name: "Remove worktree" })).not.toBeInTheDocument();
    expect(screen.getByText(/no way to force this one/)).toBeInTheDocument();
  });

  it("removes one worktree only after the inline confirmation, keeping its branch by default", async () => {
    const calls: { command: string; args: unknown }[] = [];
    mockInvoke.mockImplementation(async (command, args) => {
      calls.push({ command: String(command), args });
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      if (command === "get_worktree_inventory")
        return { entries: [CLEAN_WORKTREE], totalBytes: 2_097_152 };
      if (command === "get_worktree_auto_cleanup") return "off";
      if (command === "remove_task_worktree")
        return {
          taskId: "task-1",
          path: CLEAN_WORKTREE.path,
          bytesFreed: 2_097_152,
          branchDeleted: null,
        };
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove worktree" }));

    // Proposing is not doing.
    expect(calls.map((call) => call.command)).not.toContain("remove_task_worktree");

    fireEvent.submit(await screen.findByRole("button", { name: "Remove worktree" }));

    expect(
      await screen.findByText(
        /Removed the worktree for "Add the parser" and kept its branch, freeing 2\.0 MB\./,
      ),
    ).toBeInTheDocument();
    const removal = calls.find((call) => call.command === "remove_task_worktree");
    expect(removal?.args).toEqual({
      taskId: "task-1",
      authorization: {
        uncommittedChanges: "no",
        unpushedCommits: "no",
        // ADR-0005's default, and the radio that is checked on open.
        branch: "keep",
      },
    });
  });

  it("cancels a single removal without calling the backend", async () => {
    const commands: string[] = [];
    mockInvoke.mockImplementation(async (command) => {
      commands.push(String(command));
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      if (command === "get_worktree_inventory")
        return { entries: [CLEAN_WORKTREE], totalBytes: 2_097_152 };
      if (command === "get_worktree_auto_cleanup") return "off";
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove worktree" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(commands).not.toContain("remove_task_worktree");
    expect(screen.getByRole("button", { name: "Remove worktree" })).toBeInTheDocument();
  });

  it("sends the separate confirmation when the user chooses to delete an unmerged branch", async () => {
    // Task 016: deleting an unmerged branch is a decision distinct from
    // removing the worktree, and the wire has to carry which one was made.
    const calls: { command: string; args: unknown }[] = [];
    mockInvoke.mockImplementation(async (command, args) => {
      calls.push({ command: String(command), args });
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      if (command === "get_worktree_inventory")
        return {
          entries: [{ ...CLEAN_WORKTREE, merged: false, unpushedCommits: 2 }],
          totalBytes: 2_097_152,
        };
      if (command === "get_worktree_auto_cleanup") return "off";
      if (command === "remove_task_worktree")
        return {
          taskId: "task-1",
          path: CLEAN_WORKTREE.path,
          bytesFreed: 2_097_152,
          branchDeleted: "rimaia/task-1-add-the-parser",
        };
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove worktree" }));
    fireEvent.click(await screen.findByRole("radio", { name: /even though it is not merged/ }));
    fireEvent.submit(await screen.findByRole("button", { name: "Remove worktree" }));

    await screen.findByText(/deleted rimaia\/task-1-add-the-parser/);
    const removal = calls.find((call) => call.command === "remove_task_worktree");
    expect(removal?.args).toEqual({
      taskId: "task-1",
      authorization: {
        uncommittedChanges: "no",
        unpushedCommits: "confirmed_by_user",
        branch: "delete_even_if_unmerged",
      },
    });
  });

  it("reports what a bulk cleanup refused, not only what it removed", async () => {
    // A report that counted only successes would hide the refusals, which are
    // the part the user has to act on.
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      if (command === "get_worktree_inventory")
        return { entries: [CLEAN_WORKTREE], totalBytes: 2_097_152 };
      if (command === "get_worktree_auto_cleanup") return "off";
      if (command === "cleanup_done_worktrees")
        return {
          removed: [
            {
              taskId: "task-1",
              path: CLEAN_WORKTREE.path,
              bytesFreed: 2_097_152,
              branchDeleted: null,
            },
          ],
          refused: [
            {
              taskId: "task-2",
              taskTitle: "Wire the board",
              reason: '"Wire the board" has 2 uncommitted changes in its worktree',
            },
          ],
          bytesFreed: 2_097_152,
        };
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    fireEvent.click(
      await screen.findByRole("button", { name: "Remove worktrees for Done tasks" }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Remove Done worktrees" }));

    const status = await screen.findByText(/Removed 1 worktree, freeing 2\.0 MB\./);
    expect(status).toHaveTextContent("Left 1 alone");
    expect(status).toHaveTextContent("Wire the board: \"Wire the board\" has 2 uncommitted changes");
  });

  it("runs no bulk cleanup until its confirmation is answered", async () => {
    const commands: string[] = [];
    mockInvoke.mockImplementation(async (command) => {
      commands.push(String(command));
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      if (command === "get_worktree_inventory") return { entries: [], totalBytes: 0 };
      if (command === "get_worktree_auto_cleanup") return "off";
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove merged worktrees" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(commands).not.toContain("cleanup_merged_worktrees");
  });

  it("renders auto-cleanup off and requires an acknowledgement to turn it on", async () => {
    // "Off by default; enabling it requires acknowledging what it deletes."
    const calls: { command: string; args: unknown }[] = [];
    mockInvoke.mockImplementation(async (command, args) => {
      calls.push({ command: String(command), args });
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      if (command === "get_worktree_inventory") return { entries: [], totalBytes: 0 };
      if (command === "get_worktree_auto_cleanup") return "off";
      if (command === "set_worktree_auto_cleanup") return null;
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    const toggle = await screen.findByRole("checkbox", {
      name: /Remove a task's worktree automatically/,
    });
    expect(toggle).not.toBeChecked();

    fireEvent.click(toggle);

    // Ticking the box only *proposes* it — nothing is written yet.
    expect(calls.map((call) => call.command)).not.toContain("set_worktree_auto_cleanup");
    expect(
      screen.getByText(/every task you move to Done loses its checkout/i),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "I understand — turn it on" }));

    const write = await screen.findByRole("checkbox", {
      name: /Remove a task's worktree automatically/,
    });
    expect(write).toBeChecked();
    expect(calls.find((call) => call.command === "set_worktree_auto_cleanup")?.args).toEqual({
      setting: "on_done_acknowledged",
    });
  });

  it("turns auto-cleanup off without an acknowledgement", async () => {
    // Nothing is destroyed by deciding to keep more, so the confirm gate is
    // one-directional on purpose.
    const calls: { command: string; args: unknown }[] = [];
    mockInvoke.mockImplementation(async (command, args) => {
      calls.push({ command: String(command), args });
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      if (command === "get_worktree_inventory") return { entries: [], totalBytes: 0 };
      if (command === "get_worktree_auto_cleanup") return "on_done_acknowledged";
      if (command === "set_worktree_auto_cleanup") return null;
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    const toggle = await screen.findByRole("checkbox", {
      name: /Remove a task's worktree automatically/,
    });
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);

    expect(
      (await screen.findByRole("checkbox", {
        name: /Remove a task's worktree automatically/,
      })) as HTMLInputElement,
    ).not.toBeChecked();
    expect(calls.find((call) => call.command === "set_worktree_auto_cleanup")?.args).toEqual({
      setting: "off",
    });
  });

  it("shows a worktree whose directory vanished as gone rather than offering its size", async () => {
    // Reconciliation clears the row at startup; until then the inventory has
    // to say the disk disagrees with the database rather than quietly
    // reporting zero bytes as though that were a small worktree.
    mockReads({
      get_worktree_inventory: {
        entries: [{ ...CLEAN_WORKTREE, exists: false, sizeBytes: 0, lastActivity: null }],
        totalBytes: 0,
      },
    });

    render(<StorageSection />);
    const entry = (await screen.findByText("Add the parser")).closest("li");

    expect(within(entry as HTMLElement).getByText("gone from disk")).toBeInTheDocument();
    expect(within(entry as HTMLElement).getByText("unknown")).toBeInTheDocument();
  });
});
