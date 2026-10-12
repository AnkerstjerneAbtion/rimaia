/**
 * `"512 B"`, `"3.4 KB"`, `"12.0 MB"` — task 015's Settings storage report and
 * its per-task "prune this task's logs" action share one formatter, so a
 * byte count reads the same wherever it appears rather than two components
 * quietly disagreeing on a rounding rule.
 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * A `User-Agent` as a person would name the device: `"Firefox on macOS"`.
 *
 * Only for the account page's lists, where a session or token is recognised by
 * where it was used. A string it cannot read is cut short rather than guessed
 * at, and a missing one is "Unknown device". Order matters: Edge and Opera
 * carry "Chrome", and Chrome carries "Safari".
 */
export function shortUserAgent(userAgent: string | null): string {
  if (userAgent === null || userAgent.trim() === "") return "Unknown device";
  const browser = [
    ["Edg/", "Edge"],
    ["OPR/", "Opera"],
    ["Firefox/", "Firefox"],
    ["Chrome/", "Chrome"],
    ["Safari/", "Safari"],
  ].find(([marker]) => userAgent.includes(marker))?.[1];
  const system = [
    ["Windows", "Windows"],
    ["iPhone", "iOS"],
    ["iPad", "iOS"],
    ["Android", "Android"],
    ["Mac OS X", "macOS"],
    ["Macintosh", "macOS"],
    ["Linux", "Linux"],
  ].find(([marker]) => userAgent.includes(marker))?.[1];
  if (browser && system) return `${browser} on ${system}`;
  if (browser) return browser;
  return userAgent.length > 40 ? `${userAgent.slice(0, 40)}…` : userAgent;
}

/**
 * When something last happened, for the account page: relative to `now` within
 * a month, and the date after that, so a token last used in March still says
 * March in October. `null` is "never".
 */
export function lastSeen(timestamp: string | null, now: Date): string {
  if (timestamp === null) return "never";
  const then = Date.parse(timestamp);
  if (Number.isNaN(then)) return "";
  const elapsed = now.getTime() - then;
  const minute = 60_000;
  if (elapsed < minute) return "just now";
  if (elapsed < 60 * minute) return `${Math.floor(elapsed / minute)}m ago`;
  if (elapsed < 24 * 60 * minute) return `${Math.floor(elapsed / (60 * minute))}h ago`;
  if (elapsed < 30 * 24 * 60 * minute) return `${Math.floor(elapsed / (24 * 60 * minute))}d ago`;
  return new Date(then).toISOString().slice(0, 10);
}
