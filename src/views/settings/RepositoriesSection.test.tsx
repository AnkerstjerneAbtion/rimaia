import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

import { RepositoriesSection } from "./RepositoriesSection";
import type { Repository, StrategyCatalogueView, StrategyDefaults } from "../../types";

// `commands.ts` and `events.ts` both bottom out in `@tauri-apps/api/core`'s
// `invoke` - mocking here, rather than the wrappers, exercises the real
// `toRimaiaError` path exactly as `StorageSection.test.tsx` does.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// `@tauri-apps/api/event`'s real `listen` rejects under jsdom (there is no
// Tauri IPC bridge to call `transformCallback` on), which every other test
// below relies on: the component's own `.then(success, () => {/* no live
// refresh */})` swallows that rejection silently. Mocked here so the one
// test below that cares about the live-refresh path can drive it directly.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

// `@tauri-apps/plugin-dialog` is a separate node_modules package that
// Vitest's dep graph does not route through the `@tauri-apps/api/core` mock
// above (its own `invoke` import resolves outside the transformed module
// graph) - it has no `toRimaiaError` logic of its own to exercise, so it's
// mocked directly at its own boundary instead.
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);
const mockOpen = vi.mocked(open);

function repository(overrides: Partial<Repository> = {}): Repository {
  return {
    id: "repo-1",
    name: "rimaia",
    path: "/Users/dev/code/rimaia",
    defaultBranch: "main",
    worktreeRoot: "/Users/dev/.local/share/rimaia/worktrees/rimaia",
    allowUnattendedRuns: false,
    maxConcurrency: 1,
    createdAt: "2026-08-20T12:00:00+00:00",
    ...overrides,
  };
}

const CATALOGUE_JSON = `{
  "models": [{ "id": "opus", "label": "Opus" }, { "id": "sonnet", "label": "Sonnet" }],
  "efforts": [{ "id": "low", "label": "Low" }, { "id": "high", "label": "High" }],
  "planner": { "model": "haiku", "effort": "low", "max_turns": 6 }
}`;

function catalogueView(): StrategyCatalogueView {
  return {
    catalogue: {
      models: [
        { id: "opus", label: "Opus" },
        { id: "sonnet", label: "Sonnet" },
      ],
      efforts: [
        { id: "low", label: "Low" },
        { id: "high", label: "High" },
      ],
      planner: { model: "haiku", effort: "low", max_turns: 6 },
    },
    json: CATALOGUE_JSON,
    defaultJson: CATALOGUE_JSON,
  };
}

/**
 * Installs an `invoke` implementation that already answers the two reads every
 * row makes for its default strategy (task 020), and delegates everything else
 * to `handler`.
 *
 * Tests that are not about the strategy control would otherwise each have to
 * re-list those two commands, and their own "unexpected command" fallbacks
 * would fail on them.
 */
function mockBackend(handler: (command: string, args?: unknown) => unknown) {
  mockInvoke.mockImplementation(async (command, args) => {
    if (command === "get_strategy_catalogue") return catalogueView();
    if (command === "get_strategy_defaults") {
      return { mode: "default" } satisfies StrategyDefaults;
    }
    // Task 022's per-row credential read, answered here for the same reason
    // the two above are: every row makes it, and no test in this file is
    // about it.
    if (command === "get_repository_credential_status") {
      return {
        configured: false,
        login: null,
        label: null,
        addedAt: null,
        store: { state: "absent" },
        sshRemote: false,
      };
    }
    return handler(command, args);
  });
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockOpen.mockReset();
  mockListen.mockReset();
  // The default every test but the live-refresh one below wants: a
  // subscription that resolves (so the component's success branch runs) and
  // never fires.
  mockListen.mockResolvedValue(vi.fn());
});

describe("RepositoriesSection", () => {
  it("renders every registered repository with its default branch and remote", async () => {
    const repo = repository();
    mockBackend((command, args) => {
      if (command === "list_repositories") return [repo];
      if (command === "get_repository_remote_info") {
        return { remoteUrl: "git@github.com:example/rimaia.git", ghReady: true };
      }
      throw new Error(`unexpected command: ${command} ${JSON.stringify(args)}`);
    });

    render(<RepositoriesSection />);

    expect(await screen.findByText("rimaia")).toBeInTheDocument();
    expect(screen.getByText("/Users/dev/code/rimaia")).toBeInTheDocument();
    expect(screen.getByText("main")).toBeInTheDocument();
    expect(
      await screen.findByText("git@github.com:example/rimaia.git"),
    ).toBeInTheDocument();
  });

  it("shows the specific backend message for one invalid path, not a generic failure", async () => {
    mockOpen.mockResolvedValue("/tmp/not-a-repo");
    mockBackend((command) => {
      if (command === "list_repositories") return [];
      if (command === "register_repository") {
        throw { code: "invalid", message: "/tmp/not-a-repo is not a git repository" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);
    await screen.findByText("No repositories registered yet.");

    fireEvent.click(screen.getByRole("button", { name: "Add repository" }));

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("/tmp/not-a-repo is not a git repository");
  });

  it("shows a different specific backend message for a repository with no commits", async () => {
    mockOpen.mockResolvedValue("/tmp/empty-repo");
    mockBackend((command) => {
      if (command === "list_repositories") return [];
      if (command === "register_repository") {
        throw { code: "invalid", message: "/tmp/empty-repo has no commits yet" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);
    await screen.findByText("No repositories registered yet.");

    fireEvent.click(screen.getByRole("button", { name: "Add repository" }));

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("/tmp/empty-repo has no commits yet");
  });

  it("defaults the unattended opt-in to off and requires confirming the honest warning before enabling it", async () => {
    const repo = repository({ allowUnattendedRuns: false });
    mockBackend((command) => {
      if (command === "list_repositories") return [repo];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "set_repository_unattended_runs") {
        throw new Error("set_repository_unattended_runs must not be called before confirmation");
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    const checkbox = await screen.findByRole("checkbox", {
      name: "Allow unattended agent runs",
    });
    expect(checkbox).not.toBeChecked();

    fireEvent.click(checkbox);

    // The confirmation states the permission plainly, unhedged.
    const confirmDialog = await screen.findByRole("alertdialog");
    expect(confirmDialog).toHaveTextContent(
      "the agent can run any command in this repository's worktree, including network access and package installation, without asking.",
    );

    // Cancelling never calls the backend, and the checkbox stays off.
    fireEvent.click(within(confirmDialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(checkbox).not.toBeChecked();
  });

  it("enables unattended runs only once the confirmation is accepted", async () => {
    const repo = repository({ allowUnattendedRuns: false });
    let allow = false;
    mockBackend((command, args) => {
      if (command === "list_repositories") return [{ ...repo, allowUnattendedRuns: allow }];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "set_repository_unattended_runs") {
        allow = (args as { allow: boolean }).allow;
        return { ...repo, allowUnattendedRuns: allow };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    const checkbox = await screen.findByRole("checkbox", {
      name: "Allow unattended agent runs",
    });
    fireEvent.click(checkbox);

    const confirmDialog = await screen.findByRole("alertdialog");
    fireEvent.click(
      within(confirmDialog).getByRole("button", { name: "Enable unattended runs" }),
    );

    await waitFor(() => {
      expect(screen.getByRole("checkbox", { name: "Allow unattended agent runs" })).toBeChecked();
    });
  });

  it("shows the backend's refusal, including the task count, when removing a repository with tasks", async () => {
    const repo = repository();
    mockBackend((command) => {
      if (command === "list_repositories") return [repo];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "remove_repository") {
        throw { code: "invalid", message: "cannot remove this repository: 3 tasks still reference it" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    fireEvent.click(await screen.findByRole("button", { name: "Remove" }));

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("cannot remove this repository: 3 tasks still reference it");
  });

  it("live-refreshes the list when the backend publishes repositories:changed", async () => {
    const repo = repository();
    let listCalls = 0;
    mockBackend((command) => {
      if (command === "list_repositories") {
        listCalls += 1;
        return listCalls === 1 ? [] : [repo];
      }
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      throw new Error(`unexpected command: ${command}`);
    });
    let handler: ((event: { payload: string[] }) => void) | undefined;
    mockListen.mockImplementation(async (_eventName, callback) => {
      handler = callback as (event: { payload: string[] }) => void;
      return vi.fn();
    });

    render(<RepositoriesSection />);
    await screen.findByText("No repositories registered yet.");

    // The literal event name the shell forwarder publishes on (ADR-0018,
    // seam-contract D7) - renaming either side keeps the rest of the gate
    // green while live refresh silently stops, which is exactly what this
    // test exists to catch.
    expect(mockListen).toHaveBeenCalledWith("repositories:changed", expect.any(Function));

    // The empty-array payload is the shell forwarder's own "re-read this
    // entity wholesale" signal (see `events.ts`'s own contract note), so the
    // handler takes no ids and just refreshes.
    handler?.({ payload: [] });

    expect(await screen.findByText("rimaia")).toBeInTheDocument();
    expect(listCalls).toBe(2);
  });

  it("stores a default strategy against that repository's own id and keeps it on the row", async () => {
    // ADR-0016's "a repo of small tasks can default low without touching each
    // card": the write has to name the repository, since the global key and a
    // repository's differ only in that argument.
    const repo = repository();
    const stored: Record<string, StrategyDefaults> = {};
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_repositories") return [repo];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "get_strategy_catalogue") return catalogueView();
      if (command === "get_strategy_defaults") {
        const { repositoryId } = args as { repositoryId: string };
        return stored[repositoryId] ?? { mode: "default" };
      }
      if (command === "set_strategy_defaults") {
        const { repositoryId, value } = args as {
          repositoryId: string;
          value: StrategyDefaults;
        };
        stored[repositoryId] = value;
        return null;
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    const model = await screen.findByLabelText("Model");
    fireEvent.change(model, { target: { value: "sonnet" } });

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_strategy_defaults", {
        repositoryId: "repo-1",
        value: { mode: "default", model: "sonnet" },
      }),
    );

    // The mode travels on the same struct, and the model chosen a moment ago
    // is still part of it — one value per level, not three independent writes.
    fireEvent.change(screen.getByLabelText("Mode"), { target: { value: "planned" } });

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_strategy_defaults", {
        repositoryId: "repo-1",
        value: { mode: "planned", model: "sonnet" },
      }),
    );
    expect(model).toHaveValue("sonnet");
    expect(stored["repo-1"]).toEqual({ mode: "planned", model: "sonnet" });
  });

  it("defaults the per-repository cap to one and says why raising it is deliberate", async () => {
    // ADR-0010's rule, and the sentence that makes it actionable. Worktree
    // isolation genuinely makes two agents in one repository safe *for git*,
    // which is exactly why the reason to leave this at 1 has to be on screen
    // rather than in an ADR nobody reading this panel has open.
    mockBackend((command) => {
      if (command === "list_repositories") return [repository()];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    expect(await screen.findByLabelText("Runs at once")).toHaveValue(1);
    expect(
      screen.getByText(/Raise this only for a repository whose tasks genuinely do not interfere/),
    ).toBeInTheDocument();
  });

  it("writes the per-repository cap and warns about what two agents in one repository share", async () => {
    let stored = 1;
    mockBackend((command, args) => {
      if (command === "list_repositories") return [repository({ maxConcurrency: stored })];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "set_repository_max_concurrency") {
        stored = (args as { maxConcurrency: number }).maxConcurrency;
        return repository({ maxConcurrency: stored });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);
    fireEvent.change(await screen.findByLabelText("Runs at once"), { target: { value: "2" } });

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_repository_max_concurrency", {
        id: "repo-1",
        maxConcurrency: 2,
      }),
    );
    expect(
      await screen.findByText(/fight over ports, test databases and lockfiles/),
    ).toBeInTheDocument();
  });

  it("repaints the cap from what the backend kept when the write is refused", async () => {
    // Not optimistic, unlike the strategy dropdown above: this setter *refuses*
    // an out-of-range value rather than storing it, so a row that painted the
    // 40 optimistically would go on claiming a cap the queue does not have.
    mockBackend((command) => {
      if (command === "list_repositories") return [repository()];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "set_repository_max_concurrency") {
        throw { code: "invalid", message: "a repository may hold between 1 and 8 runs at once" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);
    const input = await screen.findByLabelText("Runs at once");
    fireEvent.change(input, { target: { value: "40" } });

    expect(await screen.findByRole("alert")).toHaveTextContent("between 1 and 8 runs at once");
    await waitFor(() => expect(input).toHaveValue(1));
  });

  it("reverts the row's strategy dropdown and shows the backend's refusal when the write fails", async () => {
    const repo = repository();
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_repositories") return [repo];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "get_strategy_catalogue") return catalogueView();
      if (command === "get_strategy_defaults") return { mode: "default" };
      if (command === "get_repository_credential_status") {
        return {
          configured: false,
          login: null,
          label: null,
          addedAt: null,
          store: { state: "absent" },
          sshRemote: false,
        };
      }
      if (command === "set_strategy_defaults") {
        throw { code: "internal", message: "the settings row could not be written" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    const model = await screen.findByLabelText("Model");
    fireEvent.change(model, { target: { value: "opus" } });

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("the settings row could not be written");
    // Nothing was stored, so the row must not go on claiming Opus.
    await waitFor(() => expect(model).toHaveValue(""));
  });
});
