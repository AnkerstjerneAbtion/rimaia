import { useCallback, useEffect, useRef, useState } from "react";

import { open } from "@tauri-apps/plugin-dialog";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  getRepositoryRemoteInfo,
  listRepositories,
  registerRepository,
  removeRepository,
  setRepositoryUnattendedRuns,
  toRimaiaError,
  updateRepository,
} from "../../lib/commands";
import { subscribeToRepositoriesChanged } from "../../lib/events";
import type { RemoteInfo, Repository, RimaiaError } from "../../types";

/**
 * ADR-0012's own wording for what the opt-in grants, used verbatim rather
 * than paraphrased. The Decision section is explicit that this sentence is
 * not allowed to soften — a reviewer reads it exactly as it renders.
 */
const UNATTENDED_RUNS_GRANT =
  "the agent can run any command in this repository's worktree, including network access and package installation, without asking.";

type RemoteInfoState =
  | { status: "loading" }
  | { status: "ready"; info: RemoteInfo }
  | { status: "error" };

interface EditDraft {
  name: string;
  defaultBranch: string;
  worktreeRoot: string;
}

function draftFrom(repository: Repository): EditDraft {
  return {
    name: repository.name,
    defaultBranch: repository.defaultBranch,
    worktreeRoot: repository.worktreeRoot,
  };
}

export function RepositoriesSection() {
  const [repositories, setRepositories] = useState<Repository[] | null>(null);
  const [listError, setListError] = useState<RimaiaError | null>(null);
  const [addError, setAddError] = useState<RimaiaError | null>(null);
  const [adding, setAdding] = useState(false);
  const [rowErrors, setRowErrors] = useState<Record<string, RimaiaError | null>>({});
  const [remoteInfos, setRemoteInfos] = useState<Record<string, RemoteInfoState>>({});
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState<EditDraft | null>(null);
  const [confirmingUnattendedId, setConfirmingUnattendedId] = useState<string | null>(null);

  const refresh = useCallback(() => {
    listRepositories().then(
      (repos) => {
        setRepositories(repos);
        setListError(null);
      },
      (thrown) => setListError(toRimaiaError(thrown)),
    );
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    // Every payload - including the empty array the shell forwarder sends
    // when a subscriber lags (see `src/lib/events.ts`'s own contract note) -
    // is read as "re-read the list", since there is no per-id fetch to
    // reconcile a partial payload against.
    let active = true;
    let unlisten: (() => void) | undefined;
    subscribeToRepositoriesChanged(() => {
      if (active) refresh();
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview): live-refresh is
        // unavailable, but every mutation below still refreshes itself.
      },
    );
    return () => {
      active = false;
      unlisten?.();
    };
  }, [refresh]);

  // Remote/`gh` inspection is fresh-per-call on the backend (never cached
  // there), but re-running it on every render would spawn `git`/`gh` in a
  // loop - fetch once per repository id we've seen and keep it until the
  // section unmounts.
  const fetchedRemoteIds = useRef<Set<string>>(new Set());
  useEffect(() => {
    if (!repositories) return;
    for (const repository of repositories) {
      if (fetchedRemoteIds.current.has(repository.id)) continue;
      fetchedRemoteIds.current.add(repository.id);
      setRemoteInfos((prev) => ({ ...prev, [repository.id]: { status: "loading" } }));
      getRepositoryRemoteInfo(repository.id).then(
        (info) =>
          setRemoteInfos((prev) => ({ ...prev, [repository.id]: { status: "ready", info } })),
        () => setRemoteInfos((prev) => ({ ...prev, [repository.id]: { status: "error" } })),
      );
    }
  }, [repositories]);

  async function handleAdd() {
    setAddError(null);
    let selected: string | string[] | null;
    try {
      selected = await open({ directory: true, multiple: false, title: "Choose a repository" });
    } catch (thrown) {
      setAddError(toRimaiaError(thrown));
      return;
    }
    if (!selected || Array.isArray(selected)) return; // cancelled

    setAdding(true);
    try {
      await registerRepository({ path: selected });
      refresh();
    } catch (thrown) {
      setAddError(toRimaiaError(thrown));
    } finally {
      setAdding(false);
    }
  }

  function beginEdit(repository: Repository) {
    setEditingId(repository.id);
    setDraft(draftFrom(repository));
    setRowErrors((prev) => ({ ...prev, [repository.id]: null }));
  }

  function cancelEdit(id: string) {
    setEditingId(null);
    setDraft(null);
    setRowErrors((prev) => ({ ...prev, [id]: null }));
  }

  async function saveEdit(id: string) {
    if (!draft) return;
    try {
      await updateRepository(id, {
        name: draft.name,
        defaultBranch: draft.defaultBranch,
        worktreeRoot: draft.worktreeRoot,
      });
      setEditingId(null);
      setDraft(null);
      setRowErrors((prev) => ({ ...prev, [id]: null }));
      refresh();
    } catch (thrown) {
      setRowErrors((prev) => ({ ...prev, [id]: toRimaiaError(thrown) }));
    }
  }

  async function handleRemove(id: string) {
    setRowErrors((prev) => ({ ...prev, [id]: null }));
    try {
      await removeRepository(id);
      refresh();
    } catch (thrown) {
      setRowErrors((prev) => ({ ...prev, [id]: toRimaiaError(thrown) }));
    }
  }

  async function handleUnattendedToggle(repository: Repository, next: boolean) {
    if (!next) {
      // Turning it off narrows what's permitted - no confirmation needed for
      // that direction, only for granting it.
      setRowErrors((prev) => ({ ...prev, [repository.id]: null }));
      try {
        await setRepositoryUnattendedRuns(repository.id, false);
        refresh();
      } catch (thrown) {
        setRowErrors((prev) => ({ ...prev, [repository.id]: toRimaiaError(thrown) }));
      }
      return;
    }
    setConfirmingUnattendedId(repository.id);
  }

  async function confirmEnableUnattended(repository: Repository) {
    setRowErrors((prev) => ({ ...prev, [repository.id]: null }));
    try {
      await setRepositoryUnattendedRuns(repository.id, true);
      setConfirmingUnattendedId(null);
      refresh();
    } catch (thrown) {
      setConfirmingUnattendedId(null);
      setRowErrors((prev) => ({ ...prev, [repository.id]: toRimaiaError(thrown) }));
    }
  }

  return (
    <section className="panel">
      <h3>Repositories</h3>
      {listError && <ErrorBanner error={listError} onDismiss={() => setListError(null)} />}

      <div className="repo-add">
        {addError && <ErrorBanner error={addError} onDismiss={() => setAddError(null)} />}
        <button type="button" onClick={handleAdd} disabled={adding}>
          {adding ? "Adding…" : "Add repository"}
        </button>
      </div>

      {repositories === null && !listError && <p className="muted">Reading…</p>}
      {repositories && repositories.length === 0 && (
        <p className="muted">No repositories registered yet.</p>
      )}

      {repositories && repositories.length > 0 && (
        <ul className="repo-list">
          {repositories.map((repository) => {
            const isEditing = editingId === repository.id;
            const remote = remoteInfos[repository.id];
            const rowError = rowErrors[repository.id];
            const confirming = confirmingUnattendedId === repository.id;

            return (
              <li key={repository.id} className="repo-item">
                <h4>{repository.name}</h4>
                {rowError && (
                  <ErrorBanner
                    error={rowError}
                    onDismiss={() =>
                      setRowErrors((prev) => ({ ...prev, [repository.id]: null }))
                    }
                  />
                )}

                {isEditing && draft ? (
                  <form
                    className="repo-edit-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      saveEdit(repository.id);
                    }}
                  >
                    <label htmlFor={`repo-name-${repository.id}`}>
                      Name
                      <input
                        id={`repo-name-${repository.id}`}
                        type="text"
                        value={draft.name}
                        onChange={(event) =>
                          setDraft({ ...draft, name: event.target.value })
                        }
                      />
                    </label>
                    <label htmlFor={`repo-branch-${repository.id}`}>
                      Default branch
                      <input
                        id={`repo-branch-${repository.id}`}
                        type="text"
                        value={draft.defaultBranch}
                        onChange={(event) =>
                          setDraft({ ...draft, defaultBranch: event.target.value })
                        }
                      />
                    </label>
                    <label htmlFor={`repo-worktree-${repository.id}`}>
                      Worktree root
                      <input
                        id={`repo-worktree-${repository.id}`}
                        type="text"
                        value={draft.worktreeRoot}
                        onChange={(event) =>
                          setDraft({ ...draft, worktreeRoot: event.target.value })
                        }
                      />
                    </label>
                    <div className="repo-actions">
                      <button type="submit">Save</button>
                      <button type="button" onClick={() => cancelEdit(repository.id)}>
                        Cancel
                      </button>
                    </div>
                  </form>
                ) : (
                  <>
                    <dl className="detail-list">
                      <dt>Path</dt>
                      <dd>
                        <code>{repository.path}</code>
                      </dd>
                      <dt>Default branch</dt>
                      <dd>{repository.defaultBranch}</dd>
                      <dt>Worktree root</dt>
                      <dd>
                        <code>{repository.worktreeRoot}</code>
                      </dd>
                      <dt>Remote</dt>
                      <dd>
                        {!remote || remote.status === "loading" ? (
                          <span className="muted">Checking…</span>
                        ) : remote.status === "error" ? (
                          <span className="muted">Could not be determined.</span>
                        ) : remote.info.remoteUrl === null ? (
                          <span className="muted">No remote configured</span>
                        ) : (
                          <code>{remote.info.remoteUrl}</code>
                        )}
                      </dd>
                    </dl>
                    {remote?.status === "ready" &&
                      remote.info.remoteUrl !== null &&
                      remote.info.ghReady === false && (
                        <p className="repo-warning">
                          gh is not installed, or not authenticated for this remote — steps
                          that open a pull request will be skipped.
                        </p>
                      )}
                    <div className="repo-actions">
                      <button type="button" onClick={() => beginEdit(repository)}>
                        Edit
                      </button>
                      <button type="button" onClick={() => handleRemove(repository.id)}>
                        Remove
                      </button>
                    </div>
                  </>
                )}

                <div className="unattended-toggle">
                  <label htmlFor={`unattended-${repository.id}`}>
                    <input
                      id={`unattended-${repository.id}`}
                      type="checkbox"
                      checked={repository.allowUnattendedRuns}
                      onChange={(event) =>
                        handleUnattendedToggle(repository, event.target.checked)
                      }
                    />
                    Allow unattended agent runs
                  </label>
                </div>

                {confirming && (
                  <div
                    className="unattended-confirm"
                    role="alertdialog"
                    aria-label={`Confirm unattended runs for ${repository.name}`}
                  >
                    <p>
                      Enabling unattended runs for &ldquo;{repository.name}&rdquo; means{" "}
                      {UNATTENDED_RUNS_GRANT}
                    </p>
                    <div className="unattended-confirm-actions">
                      <button
                        type="button"
                        onClick={() => setConfirmingUnattendedId(null)}
                      >
                        Cancel
                      </button>
                      <button
                        type="button"
                        className="btn-danger"
                        onClick={() => confirmEnableUnattended(repository)}
                      >
                        Enable unattended runs
                      </button>
                    </div>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
