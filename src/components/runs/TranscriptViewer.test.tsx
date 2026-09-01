import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { TranscriptViewer } from "./TranscriptViewer";
import type { TranscriptPage } from "../../types";

// Mocked at the Tauri seam — see `StorageSection.test.tsx`'s comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

function page(overrides: Partial<TranscriptPage> = {}): TranscriptPage {
  return {
    entries: [
      {
        line: 1,
        kind: {
          type: "assistant",
          blocks: [{ kind: "text", text: "Reading the file." }],
        },
      },
      {
        line: 2,
        kind: {
          type: "user",
          blocks: [
            { kind: "tool_result", toolUseId: "toolu_1", isError: false, content: "ok" },
          ],
        },
      },
    ],
    offset: 0,
    totalLines: 2,
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
});

describe("TranscriptViewer", () => {
  it("renders assistant text and a collapsed tool result", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "read_run_transcript_page") return page();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<TranscriptViewer runId="run-1" />);

    expect(await screen.findByText("Reading the file.")).toBeInTheDocument();
    expect(screen.getByText("Tool result")).toBeInTheDocument();
    // Collapsed by default: `<details>` with no `open` attribute.
    expect(screen.getByText("Tool result").closest("details")).not.toHaveAttribute("open");
  });

  it("highlights an errored result entry", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "read_run_transcript_page") {
        return page({
          entries: [
            {
              line: 1,
              kind: { type: "result", summary: null, errors: ["boom"], isError: true },
            },
          ],
          totalLines: 1,
        });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<TranscriptViewer runId="run-1" />);

    expect(await screen.findByText("boom")).toBeInTheDocument();
  });

  it("pages forward and back without refetching outside the requested window", async () => {
    const requestedOffsets: number[] = [];
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "read_run_transcript_page") {
        const offset = (args as { offset: number }).offset;
        requestedOffsets.push(offset);
        return offset === 0
          ? {
              entries: [{ line: 1, kind: { type: "assistant", blocks: [{ kind: "text", text: "First page" }] } }],
              offset: 0,
              totalLines: 150,
            }
          : {
              entries: [{ line: 101, kind: { type: "assistant", blocks: [{ kind: "text", text: "Second page" }] } }],
              offset,
              totalLines: 150,
            };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<TranscriptViewer runId="run-1" />);
    await screen.findByText("First page");

    fireEvent.click(screen.getByRole("button", { name: "Next" }));

    expect(await screen.findByText("Second page")).toBeInTheDocument();
    expect(requestedOffsets).toEqual([0, 100]);
  });

  it("searches the transcript and jumps to the matching line on click", async () => {
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "read_run_transcript_page") {
        const offset = (args as { offset: number }).offset;
        return offset === 0
          ? page()
          : {
              entries: [
                {
                  line: 6,
                  kind: {
                    type: "assistant",
                    blocks: [{ kind: "text", text: "Landed on the hit" }],
                  },
                },
              ],
              offset,
              totalLines: 6,
            };
      }
      if (command === "search_run_transcript") {
        expect((args as { query: string }).query).toBe("cargo test");
        return [{ line: 6, snippet: "…cargo test…" }];
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<TranscriptViewer runId="run-1" />);
    await screen.findByText("Reading the file.");

    fireEvent.change(screen.getByPlaceholderText("Search this transcript…"), {
      target: { value: "cargo test" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Search" }));

    const hit = await screen.findByRole("button", { name: /Line 6:/ });
    fireEvent.click(hit);

    expect(await screen.findByText("Landed on the hit")).toBeInTheDocument();
  });
});
