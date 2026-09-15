import { useEffect, useState } from "react";

import { ON_ARCHIVE_LABELS } from "../../lib/archive";
import type { OnArchive, Repository } from "../../types";

interface OnArchiveFieldsProps {
  readonly repository: Repository;
  /** Both halves at once, because the mode and the path are one decision
   *  (ADR-0025 point 4): `"script"` with nothing to run is not one of the
   *  three states, so this form never emits it. */
  readonly onChange: (mode: OnArchive, script: string | null) => void;
}

/**
 * ADR-0025's per-repository cleanup slot.
 *
 * Radios rather than a checkbox plus a text field, because the three states
 * are exclusive in the *database* and a form that let both be filled in would
 * be describing a row that cannot exist. The user asked for "a checkbox, or my
 * own script"; this is that question with the exclusivity made visible rather
 * than enforced behind their back.
 *
 * # What is stored, and when
 *
 * Choosing a radio is an *intent*; `repository.onArchive` is what is stored.
 * They differ on purpose for the two options that cannot be applied on the
 * click alone:
 *
 * - `remove_worktree` deletes a checkout, so it gets the acknowledgement gate
 *   `worktree_auto_cleanup` already has in Storage. It is Rimaia's own
 *   removal, which is why it can also promise what it will *not* do.
 * - `script` needs a path, and the service refuses the mode without one. A
 *   radio that flipped and then errored would be showing a state the database
 *   never reached.
 *
 * `none` is the one that applies immediately: turning cleanup off narrows what
 * happens, which is the same asymmetry `StorageSection` applies to its own
 * checkbox.
 */
export function OnArchiveFields({ repository, onChange }: OnArchiveFieldsProps) {
  const stored = repository.onArchive;
  const [intent, setIntent] = useState<OnArchive>(stored);
  const [script, setScript] = useState(repository.onArchiveScript ?? "");

  // Whatever the backend kept wins, including when it kept something this
  // form did not ask for — another window, or an MCP client, is a supported
  // writer of the same row (ADR-0006).
  useEffect(() => {
    setIntent(stored);
    setScript(repository.onArchiveScript ?? "");
  }, [stored, repository.onArchiveScript]);

  const name = `on-archive-${repository.id}`;

  function choose(next: OnArchive) {
    setIntent(next);
    if (next === "none") onChange("none", null);
  }

  return (
    <div className="repo-on-archive">
      <h5>When a task here is archived</h5>

      {(["none", "remove_worktree", "script"] as const).map((option) => (
        <label key={option} htmlFor={`${name}-${option}`} className="repo-on-archive-option">
          <input
            id={`${name}-${option}`}
            type="radio"
            name={name}
            value={option}
            checked={intent === option}
            onChange={() => choose(option)}
          />
          {ON_ARCHIVE_LABELS[option]}
        </label>
      ))}

      {intent === "remove_worktree" && stored !== "remove_worktree" && (
        <div
          className="repo-on-archive-confirm"
          role="alertdialog"
          aria-label={`Confirm deleting worktrees on archive in ${repository.name}`}
        >
          <p>
            Every task you archive here loses its checkout, including any uncommitted file a
            run left in it. Rimaia never forces past a running task and never deletes a
            branch, so committed work survives.
          </p>
          <button type="button" onClick={() => onChange("remove_worktree", null)}>
            I understand — delete the worktree
          </button>
          <button type="button" onClick={() => setIntent(stored)}>
            Cancel
          </button>
        </div>
      )}

      {intent === "script" && (
        <div className="repo-on-archive-script">
          <label htmlFor={`${name}-path`}>Script to run</label>
          <input
            id={`${name}-path`}
            type="text"
            value={script}
            placeholder="/Users/you/bin/rimaia-teardown.sh"
            onChange={(event) => setScript(event.target.value)}
          />
          <button
            type="button"
            disabled={
              script.trim() === "" ||
              (stored === "script" && script.trim() === (repository.onArchiveScript ?? ""))
            }
            onClick={() => onChange("script", script.trim())}
          >
            Save script
          </button>
          <p className="muted">
            An absolute path to an executable file, not a command line — put the pipeline
            inside the script, behind its own <code>#!</code> line. It runs with the
            repository as its working directory and is given <code>RIMAIA_TASK_ID</code>,{" "}
            <code>RIMAIA_TASK_TITLE</code>, <code>RIMAIA_REPOSITORY_PATH</code>,{" "}
            <code>RIMAIA_BRANCH</code> and <code>RIMAIA_WORKTREE_PATH</code>.
          </p>
          <p className="repo-warning">
            With a script, Rimaia cleans up nothing itself and applies none of its own
            guards. Your script can delete a worktree holding uncommitted or unpushed work.
          </p>
        </div>
      )}
    </div>
  );
}
