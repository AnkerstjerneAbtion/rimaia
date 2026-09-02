import { describe, expect, it } from "vitest";

import { formatBytes } from "./format";

describe("formatBytes", () => {
  it("renders sub-kilobyte counts in bytes", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
  });

  it("renders sub-megabyte counts in kilobytes to one decimal place", () => {
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
  });

  it("renders everything else in megabytes to one decimal place", () => {
    expect(formatBytes(1024 * 1024)).toBe("1.0 MB");
    expect(formatBytes(12 * 1024 * 1024 + 512 * 1024)).toBe("12.5 MB");
  });
});
