import { describe, expect, it } from "vitest";

import { SOLO_CAPABILITIES } from "./capabilities";
import { worktreeIsHere } from "./worktreeHere";
import { BROWSER, connected } from "../test/capabilities";

const STUDIO = { id: "runner-studio", label: "Studio Mac", unpaired: false };

describe("worktreeIsHere", () => {
  it("is true in solo, whatever the task says", () => {
    expect(worktreeIsHere({ worktreeRunner: null }, SOLO_CAPABILITIES)).toBe(true);
    expect(worktreeIsHere({ worktreeRunner: STUDIO }, SOLO_CAPABILITIES)).toBe(true);
  });

  it("is false in a browser for any task", () => {
    expect(worktreeIsHere({ worktreeRunner: STUDIO }, BROWSER)).toBe(false);
    expect(worktreeIsHere({ worktreeRunner: null }, BROWSER)).toBe(false);
  });

  it("is true in connected mode only when the local runner is the task's", () => {
    expect(worktreeIsHere({ worktreeRunner: STUDIO }, connected("runner-studio"))).toBe(true);
    expect(worktreeIsHere({ worktreeRunner: STUDIO }, connected("runner-laptop"))).toBe(false);
    expect(worktreeIsHere({ worktreeRunner: STUDIO }, connected(null))).toBe(false);
  });

  it("is false outside solo when no run has touched the task", () => {
    expect(worktreeIsHere({ worktreeRunner: null }, connected("runner-studio"))).toBe(false);
  });
});
