import type { View } from "../types";

const VIEWS: { id: View; label: string; hint: string }[] = [
  { id: "board", label: "Board", hint: "Tasks waiting, running and done" },
  { id: "runs", label: "Runs", hint: "What the agent did overnight" },
  { id: "analytics", label: "Analytics", hint: "What it has cost, and what it did" },
  { id: "settings", label: "Settings", hint: "Repositories, instructions, storage" },
];

interface SidebarProps {
  current: View;
  onNavigate: (view: View) => void;
  version?: string | null;
}

export function Sidebar({ current, onNavigate, version }: SidebarProps) {
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

      <div className="sidebar-section">
        <span className="sidebar-section-label">Navigate</span>
        <ul className="sidebar-nav">
          {VIEWS.map((view) => (
            <li key={view.id}>
              <button
                type="button"
                className="sidebar-link"
                aria-current={current === view.id ? "page" : undefined}
                onClick={() => onNavigate(view.id)}
              >
                <span className="sidebar-link-label">{view.label}</span>
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
