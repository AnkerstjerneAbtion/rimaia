import { useClientCapabilities } from "../lib/capabilities";
import type { View } from "../types";
import { TeamSwitcher } from "./TeamSwitcher";

const VIEWS: { id: View; label: string; hint: string }[] = [
  { id: "board", label: "Board", hint: "Tasks waiting, running and done" },
  { id: "review", label: "Review", hint: "Last night, and what waits for a verdict" },
  { id: "runs", label: "Runs", hint: "What the agent did overnight" },
  { id: "analytics", label: "Analytics", hint: "What it has cost, and what it did" },
  { id: "settings", label: "Settings", hint: "Repositories, instructions, storage" },
];

interface SidebarProps {
  current: View;
  onNavigate: (view: View) => void;
  version?: string | null;
  /** Tasks waiting in `in_review`; shown beside the Review entry when above zero. */
  reviewCount?: number;
}

/** Account is a sidebar entry in every mode but solo, which has no accounts
 *  (task 050, ADR-0030 point 7). */
const ACCOUNT: { id: View; label: string; hint: string } = {
  id: "account",
  label: "Account",
  hint: "Sessions, tokens and your runners",
};

export function Sidebar({ current, onNavigate, version, reviewCount = 0 }: SidebarProps) {
  const { mode } = useClientCapabilities();
  const views = mode === "solo" ? VIEWS : [...VIEWS, ACCOUNT];
  return (
    <nav className="sidebar" aria-label="Main">
      <div className="sidebar-brand">
        <span className="sidebar-mark" aria-hidden="true">
          R
        </span>
        <div className="sidebar-brand-text">
          <span className="sidebar-title">Rimaia</span>
          <span className="sidebar-tagline">
            Review in the morning, agent in the afternoon
          </span>
        </div>
      </div>

      {mode !== "solo" && <TeamSwitcher />}

      <div className="sidebar-section">
        <span className="sidebar-section-label">Navigate</span>
        <ul className="sidebar-nav">
          {views.map((view) => (
            <li key={view.id}>
              <button
                type="button"
                className="sidebar-link"
                aria-current={current === view.id ? "page" : undefined}
                onClick={() => onNavigate(view.id)}
              >
                <span className="sidebar-link-label">
                  {view.label}
                  {view.id === "review" && reviewCount > 0 && (
                    <span className="sidebar-link-count tabular-nums">{reviewCount}</span>
                  )}
                </span>
                <span className="sidebar-link-hint">{view.hint}</span>
              </button>
            </li>
          ))}
        </ul>
      </div>

      {version && (
        <div className="sidebar-footer">
          <span className="sidebar-version tabular-nums">v{version}</span>
        </div>
      )}
    </nav>
  );
}
