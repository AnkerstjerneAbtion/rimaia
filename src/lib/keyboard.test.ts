import { describe, expect, it } from "vitest";

import { isActivatableTarget, isEditableTarget } from "./keyboard";

describe("isEditableTarget", () => {
  it("treats inputs, textareas and contenteditable elements as typing surfaces", () => {
    expect(isEditableTarget(document.createElement("input"))).toBe(true);
    expect(isEditableTarget(document.createElement("textarea"))).toBe(true);
    // jsdom implements neither `contentEditable`'s setter nor
    // `isContentEditable` at all (a documented jsdom gap) - the attribute is
    // what `isEditableTarget` falls back to, and what a test can set.
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    expect(isEditableTarget(editable)).toBe(true);
  });

  it("does not treat a plain element, or null, as a typing surface", () => {
    expect(isEditableTarget(document.createElement("div"))).toBe(false);
    expect(isEditableTarget(null)).toBe(false);
  });
});

describe("isActivatableTarget", () => {
  it("treats buttons, links with an href and anything inside them as controls that answer Enter", () => {
    const button = document.createElement("button");
    const inner = document.createElement("span");
    button.append(inner);
    expect(isActivatableTarget(button)).toBe(true);
    expect(isActivatableTarget(inner)).toBe(true);

    const link = document.createElement("a");
    link.href = "https://example.com";
    expect(isActivatableTarget(link)).toBe(true);
  });

  it("does not treat a plain element, an anchor without an href, or null as one", () => {
    expect(isActivatableTarget(document.createElement("div"))).toBe(false);
    expect(isActivatableTarget(document.createElement("a"))).toBe(false);
    expect(isActivatableTarget(null)).toBe(false);
  });
});
