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
