import type { View } from "../types";

const VIEWS: { id: View; label: string; hint: string }[] = [
  { id: "board", label: "Board", hint: "Tasks waiting, running and done" },
  { id: "runs", label: "Runs", hint: "What the agent did overnight" },
  { id: "settings", label: "Settings", hint: "Repositories, instructions, storage" },
];

interface SidebarProps {
  current: View;
  onNavigate: (view: View) => void;
}

export function Sidebar({ current, onNavigate }: SidebarProps) {
  return (
    <nav className="sidebar" aria-label="Main">
      <div className="sidebar-brand">
        <span className="sidebar-title">Rimaia</span>
        <span className="sidebar-tagline">
          Review in the morning, agent in the afternoon
        </span>
      </div>

      <ul className="sidebar-nav">
        {VIEWS.map((view) => (
          <li key={view.id}>
            <button
              type="button"
              className="sidebar-link"
              aria-current={current === view.id ? "page" : undefined}
              title={view.hint}
              onClick={() => onNavigate(view.id)}
            >
              {view.label}
            </button>
          </li>
        ))}
      </ul>
    </nav>
  );
}
