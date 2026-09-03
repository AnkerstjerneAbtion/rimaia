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

/**
 * The repository filter, title search and "New task" button — task 005's
 * "repository filter (all repositories, or one)" plus the `/` and `n`
 * keyboard entries `Board` binds against this input and this button.
 *
 * The two shortcuts are shown as `<kbd>` chips rather than hidden in a
 * placeholder and a `title=`. They are `aria-hidden`: the keys are a pointer
 * user's discovery aid, and a screen-reader user hearing "New task N" would
 * be read a stray letter with no way to tell it from the label. Hiding them
 * also keeps each control's accessible name exactly what it already was.
 */
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
        <span className="board-toolbar-label">Repository</span>
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

      <div className="board-search-field">
        <input
          ref={searchInputRef}
          type="search"
          className="board-search"
          placeholder="Search titles…"
          aria-label="Search task titles"
          value={searchQuery}
          onChange={(event) => onSearchChange(event.target.value)}
        />
        <kbd className="board-kbd" aria-hidden="true">
          /
        </kbd>
      </div>

      <button
        type="button"
        className="btn-primary board-new-task"
        onClick={onNewTask}
        disabled={newTaskDisabled}
      >
        New task
        <kbd className="board-kbd" aria-hidden="true">
          N
        </kbd>
      </button>
    </div>
  );
}
