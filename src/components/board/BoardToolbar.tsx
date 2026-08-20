import type { RefObject } from "react";

import type { Repository } from "../../types";

interface BoardToolbarProps {
  readonly repositories: readonly Repository[];
  readonly selectedRepositoryId: string | null;
  readonly onRepositoryChange: (id: string | null) => void;
  readonly searchQuery: string;
  readonly onSearchChange: (value: string) => void;
  readonly searchInputRef: RefObject<HTMLInputElement | null>;
  readonly onNewTask: () => void;
  readonly newTaskDisabled: boolean;
}

/** The repository filter, title search and "New task" button — task 005's
 *  "repository filter (all repositories, or one)" plus the `/` and `n`
 *  keyboard entries `Board` binds against this input and this button. */
export function BoardToolbar({
  repositories,
  selectedRepositoryId,
  onRepositoryChange,
  searchQuery,
  onSearchChange,
  searchInputRef,
  onNewTask,
  newTaskDisabled,
}: BoardToolbarProps) {
  return (
    <div className="board-toolbar">
      <label className="board-repo-filter">
        Repository
        <select
          value={selectedRepositoryId ?? ""}
          onChange={(event) => onRepositoryChange(event.target.value || null)}
        >
          <option value="">All repositories</option>
          {repositories.map((repository) => (
            <option key={repository.id} value={repository.id}>
              {repository.name}
            </option>
          ))}
        </select>
      </label>

      <input
        ref={searchInputRef}
        type="search"
        className="board-search"
        placeholder="Search titles… (press /)"
        aria-label="Search task titles"
        value={searchQuery}
        onChange={(event) => onSearchChange(event.target.value)}
      />

      <button type="button" onClick={onNewTask} disabled={newTaskDisabled} title="Press n">
        New task
      </button>
    </div>
  );
}
