import { REVIEW_KEYS } from "../../lib/review";
import type { ReviewMode } from "../../lib/review";

/**
 * The keys for the current mode, always on screen and small — not behind a
 * help key, because a morning review is done by someone who has not used it
 * since yesterday. Rendered from {@link REVIEW_KEYS}, the table the handlers
 * bind, so it cannot list a key that does nothing.
 */
export function KeyLegend({
  mode,
  noteOpen = false,
  emptyQueue = false,
}: {
  mode: ReviewMode;
  noteOpen?: boolean;
  /** An empty queue has nothing to walk or decide, so only the way out is listed. */
  emptyQueue?: boolean;
}) {
  const entries = noteOpen
    ? [
        { keys: ["Ctrl or ⌘ + Enter"], label: "send the note" },
        { keys: ["Esc"], label: "cancel" },
      ]
    : REVIEW_KEYS.filter(
        (entry) => entry.mode === mode && (!emptyQueue || entry.command === "show_digest"),
      );
  return (
    <ul className="review-legend" aria-label="Keyboard shortcuts">
      {entries.map((entry) => (
        <li key={entry.label}>
          {entry.keys.map((key, index) => (
            <span key={key}>
              {index > 0 && " or "}
              <kbd>{key}</kbd>
            </span>
          ))}{" "}
          {entry.label}
        </li>
      ))}
    </ul>
  );
}
