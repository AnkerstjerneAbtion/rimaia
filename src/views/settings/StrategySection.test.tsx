import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { StrategySection } from "./StrategySection";
import type { StrategyCatalogueView } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts` or `lib/events.ts` — see
// `StorageSection.test.tsx`'s comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

/** Deliberately not the real `DEFAULT_CATALOGUE_JSON`: the frontend never
 *  holds a second copy of the built-in list — it writes back whatever
 *  `defaultJson` the backend handed it, which is the whole point of that field
 *  crossing the boundary. */
const DEFAULT_JSON = `{
  "models": [{ "id": "opus", "label": "Opus" }, { "id": "sonnet", "label": "Sonnet" }],
  "efforts": [{ "id": "low", "label": "Low" }, { "id": "high", "label": "High" }],
  "planner": { "model": "haiku", "effort": "low", "max_turns": 6 }
}`;

function catalogueView(overrides: Partial<StrategyCatalogueView> = {}): StrategyCatalogueView {
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
    json: DEFAULT_JSON,
    defaultJson: DEFAULT_JSON,
    ...overrides,
  };
}

/** The three reads the section makes on mount, so every test below only has to
 *  say what it is actually about. */
function mockBackend(handler: (command: string, args?: unknown) => unknown = unexpected) {
  mockInvoke.mockImplementation(async (command, args) => {
    if (command === "get_strategy_catalogue") return catalogueView();
    if (command === "get_strategy_defaults") return { mode: "default" };
    if (command === "get_strategy_approval") return "automatic";
    return handler(command, args);
  });
}

function unexpected(command: string): never {
  throw new Error(`unexpected command: ${command}`);
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("StrategySection", () => {
  it("renders the parse message the backend refused an edited catalogue with, inline", async () => {
    // The reason the catalogue is a textarea at all: the honest failure mode of
    // hand-edited JSON is serde's own sentence, and hiding it behind "invalid"
    // would leave the user hunting for the brace themselves.
    mockBackend((command) => {
      if (command === "set_strategy_catalogue") {
        throw {
          code: "invalid",
          message:
            "the catalogue is not valid JSON: EOF while parsing an object at line 4 column 0",
        };
      }
      return unexpected(command);
    });

    render(<StrategySection />);

    const editor = await screen.findByLabelText("Strategy catalogue");
    fireEvent.change(editor, { target: { value: '{"models": [' } });
    fireEvent.blur(editor);

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent(
      "the catalogue is not valid JSON: EOF while parsing an object at line 4 column 0",
    );
    // The rejected draft stays on screen: it is the thing with the missing
    // brace in it, and reverting would throw away what was just typed.
    expect(editor).toHaveValue('{"models": [');
  });

  it("writes the backend's own default catalogue when Restore defaults is clicked", async () => {
    const edited = '{"models": [], "efforts": [], "planner": {"max_turns": 2}}';
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_strategy_catalogue") {
        return catalogueView({ catalogue: emptyCatalogue(), json: edited });
      }
      if (command === "get_strategy_defaults") return { mode: "default" };
      if (command === "get_strategy_approval") return "automatic";
      if (command === "set_strategy_catalogue") {
        expect(args).toEqual({ value: DEFAULT_JSON });
        return catalogueView();
      }
      return unexpected(command);
    });

    render(<StrategySection />);

    expect(await screen.findByLabelText("Strategy catalogue")).toHaveValue(edited);
    fireEvent.click(screen.getByRole("button", { name: "Restore defaults" }));

    // Repainted from the command's own result, not from a re-read — and the
    // bytes written are the backend's, never a second copy of the list kept
    // here.
    await waitFor(() =>
      expect(screen.getByLabelText("Strategy catalogue")).toHaveValue(DEFAULT_JSON),
    );
  });

  it("preselects automatic approval, which is what an unset key reads as", async () => {
    mockBackend();

    render(<StrategySection />);

    const automatic = await screen.findByRole("radio", {
      name: "Run the implementation immediately after planning (recommended for overnight queues)",
    });
    expect(automatic).toBeChecked();
    expect(
      screen.getByRole("radio", { name: "Wait for me to accept the strategy" }),
    ).not.toBeChecked();
    // The stub is labelled as one, so the next reader does not file the
    // missing gate as a bug.
    expect(screen.getByText(/read by nothing yet/)).toBeInTheDocument();
  });

  it("stores the approval setting even though nothing reads it yet", async () => {
    mockBackend((command) => {
      if (command === "set_strategy_approval") return null;
      return unexpected(command);
    });

    render(<StrategySection />);
    fireEvent.click(
      await screen.findByRole("radio", { name: "Wait for me to accept the strategy" }),
    );

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_strategy_approval", { value: "manual" }),
    );
  });

  it("writes the global default under no repository id at all", async () => {
    mockBackend((command) => {
      if (command === "set_strategy_defaults") return null;
      return unexpected(command);
    });

    render(<StrategySection />);
    fireEvent.change(await screen.findByLabelText("Model"), { target: { value: "sonnet" } });

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_strategy_defaults", {
        repositoryId: null,
        value: { mode: "default", model: "sonnet" },
      }),
    );
  });

  it("keeps a stored model the catalogue no longer lists, with a hint rather than dropping it", async () => {
    // The backend spawns a stored id verbatim whether or not the list still
    // carries it, so a control that could only show catalogue members would be
    // showing a lie about the next run.
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") return catalogueView();
      if (command === "get_strategy_defaults") return { mode: "manual", model: "opus-4-1" };
      if (command === "get_strategy_approval") return "automatic";
      return unexpected(command);
    });

    render(<StrategySection />);

    expect(await screen.findByLabelText("Model")).toHaveValue("opus-4-1");
    expect(
      screen.getByRole("option", { name: "opus-4-1 — not in the catalogue" }),
    ).toBeInTheDocument();
  });

  it("rewrites the whole catalogue document when the planner's model changes", async () => {
    // The planner budget lives inside the catalogue key, so there is one
    // writer and one document — the cost is that the stored text is
    // renormalized, which is why this asserts the exact bytes.
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_strategy_catalogue") return catalogueView();
      if (command === "get_strategy_defaults") return { mode: "default" };
      if (command === "get_strategy_approval") return "automatic";
      if (command === "set_strategy_catalogue") {
        expect(args).toEqual({
          value: JSON.stringify(
            {
              ...catalogueView().catalogue,
              planner: { model: "sonnet", effort: "low", max_turns: 6 },
            },
            null,
            2,
          ),
        });
        return catalogueView();
      }
      return unexpected(command);
    });

    render(<StrategySection />);
    fireEvent.change(await screen.findByLabelText("Planner model"), {
      target: { value: "sonnet" },
    });

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("set_strategy_catalogue", expect.anything()),
    );
  });

  it("refuses to store a turn limit that is not a whole number, without calling the backend", async () => {
    mockBackend();

    render(<StrategySection />);
    const limit = await screen.findByLabelText("Planner turn limit");
    fireEvent.change(limit, { target: { value: "0" } });
    fireEvent.blur(limit);

    // `JSON.stringify` would send `null` for an empty box and a zero would be
    // a planner that cannot take a turn — neither is worth a round trip, and
    // the box snaps back to what is stored.
    expect(mockInvoke).not.toHaveBeenCalledWith("set_strategy_catalogue", expect.anything());
    expect(limit).toHaveValue(6);
  });

  it("renders the error banner when the catalogue cannot be read at all", async () => {
    mockInvoke.mockRejectedValue({ code: "internal", message: "the database is locked" });

    render(<StrategySection />);

    const banner = await screen.findAllByRole("alert");
    expect(banner[0]).toHaveTextContent("the database is locked");
  });
});

function emptyCatalogue() {
  return { models: [], efforts: [], planner: { model: null, effort: null, max_turns: 2 } };
}
