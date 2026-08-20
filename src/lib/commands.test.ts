import { describe, expect, it } from "vitest";

import { toRimaiaError } from "./commands";

describe("toRimaiaError", () => {
  it("passes a backend {code, message} payload through unchanged", () => {
    const thrown = { code: "not_found", message: "repository not found" };

    expect(toRimaiaError(thrown)).toEqual({
      code: "not_found",
      message: "repository not found",
    });
  });

  it("wraps a bare string rejection as an internal error", () => {
    expect(toRimaiaError("permission denied")).toEqual({
      code: "internal",
      message: "permission denied",
    });
  });

  it("uses a JS Error's message as an internal error", () => {
    expect(toRimaiaError(new Error("network unreachable"))).toEqual({
      code: "internal",
      message: "network unreachable",
    });
  });

  it("renders a non-conforming object as readable JSON, not [object Object]", () => {
    const result = toRimaiaError({ unexpected: "shape" });

    expect(result.code).toBe("internal");
    expect(result.message).not.toBe("[object Object]");
    expect(result.message).toBe('{"unexpected":"shape"}');
  });
});
