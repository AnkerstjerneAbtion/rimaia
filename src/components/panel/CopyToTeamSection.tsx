import { useState } from "react";

import { copyTaskToTeam, listCopyTargets, toRimaiaError } from "../../lib/commands";
import { useTeams } from "../../lib/teams";
import type { CopyTarget, RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

/**
 * **Copy to team…** (task 051 Scope 12, ADR-0029 point 5). A task never moves
 * between teams; a copy is a new task in a repository of the chosen team,
 * carrying the title, the plan and the extra instructions and nothing else.
 *
 * Shown only to a person in more than one team, which is never solo.
 */
export function CopyToTeamSection({ taskId }: { readonly taskId: string }) {
  const teams = useTeams();
  if (!teams || teams.teams.length < 2) return null;
  return <CopyToTeam taskId={taskId} />;
}

function CopyToTeam({ taskId }: { readonly taskId: string }) {
  const [targets, setTargets] = useState<CopyTarget[] | null>(null);
  const [teamId, setTeamId] = useState("");
  const [repositoryId, setRepositoryId] = useState("");
  const [copying, setCopying] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  async function open() {
    setError(null);
    setCopied(null);
    try {
      const listed = await listCopyTargets();
      setTargets(listed);
      setTeamId("");
      setRepositoryId("");
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }

  async function copy() {
    if (!repositoryId || copying) return;
    setCopying(true);
    setError(null);
    try {
      await copyTaskToTeam(taskId, repositoryId);
      const team = targets?.find((target) => target.teamId === teamId);
      setCopied(team?.teamName ?? "the team");
      setTargets(null);
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setCopying(false);
    }
  }

  const chosen = targets?.find((target) => target.teamId === teamId) ?? null;

  return (
    <section className="task-detail-section task-copy" aria-label="Copy to team">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {copied && (
        <p className="muted" role="status">
          {`Copied to ${copied}, as a new task in Not ready.`}
        </p>
      )}
      {targets === null ? (
        <button type="button" onClick={open}>
          Copy to team…
        </button>
      ) : (
        <div className="task-copy-form">
          <h4 className="task-copy-heading">Copy to another team</h4>
          <p className="muted">
            The copy brings the plan and nothing else: no runs, links, dependencies or assignee.
            It starts in Not ready.
          </p>
          <label className="task-copy-field">
            Team
            <select
              value={teamId}
              onChange={(event) => {
                setTeamId(event.target.value);
                setRepositoryId("");
              }}
            >
              <option value="">Choose a team</option>
              {targets.map((target) => (
                <option key={target.teamId} value={target.teamId}>
                  {target.teamName}
                </option>
              ))}
            </select>
          </label>
          <label className="task-copy-field">
            Repository
            <select
              value={repositoryId}
              disabled={chosen === null}
              onChange={(event) => setRepositoryId(event.target.value)}
            >
              <option value="">
                {chosen && chosen.repositories.length === 0
                  ? "This team has no repository"
                  : "Choose a repository"}
              </option>
              {(chosen?.repositories ?? []).map((repository) => (
                <option key={repository.id} value={repository.id}>
                  {repository.name}
                </option>
              ))}
            </select>
          </label>
          <div className="task-copy-actions">
            <button
              type="button"
              className="btn-primary"
              disabled={!repositoryId || copying}
              onClick={copy}
            >
              {copying ? "Copying…" : "Copy task"}
            </button>
            <button type="button" onClick={() => setTargets(null)} disabled={copying}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
