import { useState } from "react";

import {
  addTaskLink,
  removeTaskLink,
  reorderTaskLink,
  toRimaiaError,
  updateTaskLink,
} from "../../lib/commands";
import type { RimaiaError, TaskLink } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

/**
 * A link's label when the user leaves it blank at add time — the URL's own
 * host, almost always more legible than the raw URL and requires nothing
 * from a "paste a link" flow. Falls back to the raw string for a URL
 * `new URL` can't parse rather than throwing on input the backend does not
 * itself validate the shape of.
 */
export function defaultLabelFromUrl(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

interface LinksEditorProps {
  readonly taskId: string;
  /** Server order (by `position`) — `get_task` already sorts these. */
  readonly links: readonly TaskLink[];
  readonly loading: boolean;
  /** Refetches the owning task's detail. Add/remove/reorder all change
   *  positions server-side, and this component holds no position math of
   *  its own to reconcile a local copy against. */
  readonly onChanged: () => void;
}

export function LinksEditor({ taskId, links, loading, onChanged }: LinksEditorProps) {
  const [error, setError] = useState<RimaiaError | null>(null);
  const [newLabel, setNewLabel] = useState("");
  const [newUrl, setNewUrl] = useState("");
  const [adding, setAdding] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState({ label: "", url: "" });
  const [busyId, setBusyId] = useState<string | null>(null);

  function handleAdd() {
    const url = newUrl.trim();
    if (!url) return;
    const label = newLabel.trim() || defaultLabelFromUrl(url);
    setAdding(true);
    setError(null);
    addTaskLink(taskId, { label, url }).then(
      () => {
        setNewLabel("");
        setNewUrl("");
        setAdding(false);
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setAdding(false);
      },
    );
  }

  function beginEdit(link: TaskLink) {
    setEditingId(link.id);
    setEditDraft({ label: link.label, url: link.url });
  }

  function saveEdit(id: string) {
    setBusyId(id);
    setError(null);
    updateTaskLink(id, { label: editDraft.label.trim(), url: editDraft.url.trim() }).then(
      () => {
        setEditingId(null);
        setBusyId(null);
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setBusyId(null);
      },
    );
  }

  function handleRemove(id: string) {
    setBusyId(id);
    setError(null);
    removeTaskLink(id).then(
      () => {
        setBusyId(null);
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setBusyId(null);
      },
    );
  }

  function move(linkId: string, beforeId: string | null, afterId: string | null) {
    setBusyId(linkId);
    setError(null);
    reorderTaskLink(linkId, beforeId, afterId).then(
      () => {
        setBusyId(null);
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setBusyId(null);
      },
    );
  }

  // Neighbour ids for "swap with the adjacent link" — the same shape as
  // `move_task`'s `beforeId`/`afterId`. `reorder_task_link` owns the
  // resulting position (seam-contract D1's rule for tasks applies just as
  // much here); this only ever names the two links either side of the slot.
  function moveUp(index: number) {
    if (index === 0) return;
    const beforeId = links[index - 2]?.id ?? null;
    const afterId = links[index - 1].id;
    move(links[index].id, beforeId, afterId);
  }

  function moveDown(index: number) {
    if (index === links.length - 1) return;
    const beforeId = links[index + 1].id;
    const afterId = links[index + 2]?.id ?? null;
    move(links[index].id, beforeId, afterId);
  }

  return (
    <section className="task-detail-section">
      <h4>Links</h4>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      {loading && <p className="muted">Loading…</p>}
      {!loading && links.length === 0 && <p className="muted">No links yet.</p>}

      {!loading && links.length > 0 && (
        <ul className="links-list">
          {links.map((link, index) =>
            editingId === link.id ? (
              <li key={link.id} className="link-item">
                <input
                  type="text"
                  aria-label="Link label"
                  value={editDraft.label}
                  onChange={(event) => setEditDraft({ ...editDraft, label: event.target.value })}
                />
                <input
                  type="text"
                  aria-label="Link URL"
                  value={editDraft.url}
                  onChange={(event) => setEditDraft({ ...editDraft, url: event.target.value })}
                />
                <div className="link-item-actions">
                  <button
                    type="button"
                    onClick={() => saveEdit(link.id)}
                    disabled={busyId === link.id}
                  >
                    Save
                  </button>
                  <button
                    type="button"
                    onClick={() => setEditingId(null)}
                    disabled={busyId === link.id}
                  >
                    Cancel
                  </button>
                </div>
              </li>
            ) : (
              <li key={link.id} className="link-item">
                <a href={link.url} target="_blank" rel="noreferrer">
                  {link.label}
                </a>
                <div className="link-item-actions">
                  <button
                    type="button"
                    onClick={() => moveUp(index)}
                    disabled={index === 0 || busyId === link.id}
                    aria-label={`Move "${link.label}" up`}
                  >
                    ↑
                  </button>
                  <button
                    type="button"
                    onClick={() => moveDown(index)}
                    disabled={index === links.length - 1 || busyId === link.id}
                    aria-label={`Move "${link.label}" down`}
                  >
                    ↓
                  </button>
                  <button
                    type="button"
                    onClick={() => beginEdit(link)}
                    disabled={busyId === link.id}
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    onClick={() => handleRemove(link.id)}
                    disabled={busyId === link.id}
                  >
                    Remove
                  </button>
                </div>
              </li>
            ),
          )}
        </ul>
      )}

      <div className="link-add-form">
        <input
          type="text"
          placeholder="Label (defaults to the URL's host)"
          aria-label="New link label"
          value={newLabel}
          onChange={(event) => setNewLabel(event.target.value)}
        />
        <input
          type="text"
          placeholder="https://…"
          aria-label="New link URL"
          value={newUrl}
          onChange={(event) => setNewUrl(event.target.value)}
        />
        <button type="button" onClick={handleAdd} disabled={adding || newUrl.trim() === ""}>
          {adding ? "Adding…" : "Add link"}
        </button>
      </div>
    </section>
  );
}
