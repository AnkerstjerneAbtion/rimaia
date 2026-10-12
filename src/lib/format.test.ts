import { describe, expect, it } from "vitest";

import { formatBytes, lastSeen, shortUserAgent } from "./format";

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

describe("shortUserAgent", () => {
  it("names the browser and the system", () => {
    expect(
      shortUserAgent(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:134.0) Gecko/20100101 Firefox/134.0",
      ),
    ).toBe("Firefox on macOS");
    expect(
      shortUserAgent(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36 Edg/130.0",
      ),
    ).toBe("Edge on Windows");
    expect(
      shortUserAgent(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
      ),
    ).toBe("Safari on iOS");
  });

  it("cuts what it cannot read short, and names a missing one", () => {
    expect(shortUserAgent("curl/8.7.1")).toBe("curl/8.7.1");
    expect(shortUserAgent("x".repeat(60))).toBe(`${"x".repeat(40)}…`);
    expect(shortUserAgent(null)).toBe("Unknown device");
  });
});

describe("lastSeen", () => {
  const now = new Date("2026-10-11T12:00:00Z");

  it("is relative within a month and a date after, and never for null", () => {
    expect(lastSeen(null, now)).toBe("never");
    expect(lastSeen("2026-10-11T11:59:30Z", now)).toBe("just now");
    expect(lastSeen("2026-10-11T09:00:00Z", now)).toBe("3h ago");
    expect(lastSeen("2026-10-01T12:00:00Z", now)).toBe("10d ago");
    expect(lastSeen("2026-03-02T08:00:00Z", now)).toBe("2026-03-02");
  });
});
