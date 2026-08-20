import type { ReactNode } from "react";

interface EmptyStateProps {
  title: string;
  body: string;
  /** Names the task that fills this view in, so a half-built app reads as
   *  deliberate rather than broken. */
  arrivesIn: string;
  children?: ReactNode;
}

export function EmptyState({ title, body, arrivesIn, children }: EmptyStateProps) {
  return (
    <div className="empty-state">
      <h2>{title}</h2>
      <p>{body}</p>
      <p className="empty-state-note">{arrivesIn}</p>
      {children}
    </div>
  );
}
