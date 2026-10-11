import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";

import { useTeams } from "../lib/teams";

/**
 * The team on screen, at the top of the sidebar in every mode but solo (task
 * 050, ADR-0029 Consequences): a person in two teams sees two boards, and
 * nothing else says which one this is.
 *
 * With one team it is a name and no control, because a menu of one choice is a
 * button that does nothing. With several it opens a menu of them, each with
 * the person's role in it as a word, and choosing one is `App`'s to carry out.
 */
export function TeamSwitcher() {
  const teams = useTeams();
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    function onDocumentPointerDown(event: PointerEvent) {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("pointerdown", onDocumentPointerDown);
    return () => document.removeEventListener("pointerdown", onDocumentPointerDown);
  }, [open]);

  if (!teams) return null;
  const { current, choose } = teams;

  if (teams.teams.length < 2) {
    return (
      <div className="team-switcher team-switcher-single">
        <span className="team-switcher-label">Team</span>
        <span className="team-switcher-name">{current.name}</span>
      </div>
    );
  }

  function handleKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      setOpen(false);
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    const items = Array.from(
      root.current?.querySelectorAll<HTMLButtonElement>("[role='menuitemradio']") ?? [],
    );
    if (items.length === 0) return;
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    const step = event.key === "ArrowDown" ? 1 : -1;
    items[(at === -1 ? 0 : at + step + items.length) % items.length]?.focus();
  }

  return (
    <div ref={root} className="team-switcher" onKeyDown={handleKeyDown}>
      <span className="team-switcher-label" id="team-switcher-label">
        Team
      </span>
      <button
        type="button"
        className="team-switcher-button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-labelledby="team-switcher-label team-switcher-current"
        onClick={() => setOpen(!open)}
      >
        <span className="team-switcher-name" id="team-switcher-current">
          {current.name}
        </span>
        <span className="team-switcher-caret" aria-hidden="true">
          ▾
        </span>
      </button>
      {open && (
        <ul className="team-switcher-menu" role="menu" aria-label="Switch team">
          {teams.teams.map((team) => (
            <li key={team.id} role="none">
              <button
                type="button"
                role="menuitemradio"
                aria-checked={team.id === current.id}
                onClick={() => {
                  setOpen(false);
                  if (team.id !== current.id) choose(team.id);
                }}
              >
                <span className="team-switcher-item-name">{team.name}</span>
                <span className="team-switcher-item-role">
                  {team.personal ? "personal" : team.role}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
