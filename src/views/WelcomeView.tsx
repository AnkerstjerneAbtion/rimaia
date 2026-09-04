import { useCallback, useEffect, useState } from "react";

import { DoctorResultList } from "../components/DoctorResultList";
import { ErrorBanner } from "../components/ErrorBanner";
import { McpAddCommand } from "../components/McpAddCommand";
import { RepositoryAddForm } from "../components/RepositoryAddForm";
import { useDoctor } from "../hooks/useDoctor";
import { dismissOnboarding, getBaseInstructions, listRepositories, toRimaiaError } from "../lib/commands";
import { resultsFor, statusLabel, worstStatus } from "../lib/doctor";
import { InstructionsSection } from "./settings/InstructionsSection";
import type { DoctorCheck, DoctorStatus, Repository, RimaiaError } from "../types";

/**
 * The first-run walkthrough (task 018): register a repository → enable
 * unattended runs → set base instructions → add the MCP server.
 *
 * Two rules shape it. **A step is done when the thing is true, not when a
 * button was clicked** — every heading below reads live state (a registered
 * repository, an `allowUnattendedRuns` flag, non-empty instructions, a
 * listening server), so quitting halfway and coming back resumes honestly, and
 * a user who set something up before ever opening this screen sees it already
 * satisfied. And **each step embeds the real control** rather than a
 * description of where to find one, except where the real control carries
 * policy text that is not allowed to be restated (see step two).
 */
interface Step {
  readonly title: string;
  readonly blurb: string;
  /** The doctor rows that belong to this step, if any. */
  readonly checks: readonly DoctorCheck[];
  /** Live state, not a click: whether this step's outcome already holds. */
  readonly satisfied: boolean;
  readonly body: React.ReactNode;
}

function StepBadge({ done, status }: { done: boolean; status: DoctorStatus | null }) {
  if (!done) return <span className="welcome-step-badge">To do</span>;
  return (
    <span className={`welcome-step-badge welcome-step-badge-${status ?? "pass"}`}>
      {status && status !== "pass" ? statusLabel(status) : "Done"}
    </span>
  );
}

export function WelcomeView({ onFinish }: { onFinish: () => void }) {
  const { report, running, rerun } = useDoctor();
  const [repositories, setRepositories] = useState<Repository[]>([]);
  const [instructions, setInstructions] = useState("");
  const [error, setError] = useState<RimaiaError | null>(null);
  const [dismissing, setDismissing] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [repos, base] = await Promise.all([listRepositories(), getBaseInstructions()]);
      setRepositories(repos);
      setInstructions(base);
      setError(null);
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function handleFinish() {
    setDismissing(true);
    try {
      await dismissOnboarding();
      onFinish();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
      setDismissing(false);
    }
  }

  const unattended = repositories.filter((repository) => repository.allowUnattendedRuns);

  const steps: Step[] = [
    {
      title: "Register a repository",
      blurb:
        "Rimaia runs each task in a git worktree of a repository you register. Nothing is written inside the repository itself.",
      checks: ["repository_path"],
      satisfied: repositories.length > 0,
      body: <RepositoryAddForm onRegistered={() => void refresh()} />,
    },
    {
      title: "Enable unattended runs",
      blurb:
        "An overnight run cannot stop to ask permission, so each repository opts in explicitly. The grant is broad and Settings states it in full before you agree — which is why this step sends you there rather than repeating it here (ADR-0012).",
      checks: [],
      satisfied: unattended.length > 0,
      body:
        repositories.length === 0 ? (
          <p className="muted">Register a repository first.</p>
        ) : unattended.length > 0 ? (
          <p className="muted">
            Enabled for {unattended.map((repository) => repository.name).join(", ")}.
          </p>
        ) : (
          <p className="muted">
            Settings → Repositories has the toggle, beside the repository it applies to.
          </p>
        ),
    },
    {
      title: "Set base instructions",
      blurb:
        "Prepended to every run's prompt: how to branch, when to commit, whether to open a pull request. This is where you tell every task what “done” looks like.",
      checks: [],
      satisfied: instructions.trim().length > 0,
      body: <InstructionsSection />,
    },
    {
      title: "Add the MCP server",
      blurb:
        "Lets a Claude Code session hand a finished plan straight to Rimaia's board instead of implementing it there and then.",
      checks: ["mcp_port"],
      satisfied: true,
      body: <McpAddCommand />,
    },
  ];

  return (
    <div className="view welcome-view">
      <header className="view-header">
        <h2>Welcome to Rimaia</h2>
        <p>
          Four things to set up once. You can leave at any point and finish from Settings — this
          screen reads your actual configuration, so nothing here has to be done in order.
        </p>
      </header>

      {/* How far in you are, countable rather than estimated: there are exactly
          four things and each is either true or not. It reads live state like
          every heading below it, so somebody who set two of these up before
          ever opening this screen arrives two segments in. */}
      <div className="welcome-progress">
        <span className="welcome-progress-meter" aria-hidden="true">
          {steps.map((step) => (
            <span
              key={step.title}
              className="welcome-progress-segment"
              data-done={step.satisfied}
            />
          ))}
        </span>
        <span className="welcome-progress-count">
          {steps.filter((step) => step.satisfied).length} of {steps.length} set up
        </span>
      </div>

      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      <ol className="welcome-steps">
        {steps.map((step, index) => {
          const rows = resultsFor(report, step.checks);
          return (
            <li
              key={step.title}
              className={
                step.satisfied ? "welcome-step welcome-step-done panel" : "welcome-step panel"
              }
            >
              <h3>
                <span className="welcome-step-number">{index + 1}</span>
                {step.title}
                <StepBadge done={step.satisfied} status={worstStatus(rows)} />
              </h3>
              <p className="muted">{step.blurb}</p>
              {step.body}
              {/* Only the problems: a welcome screen is not the place to
                  enumerate passing checks, and Settings → Environment already
                  shows the whole report. */}
              <DoctorResultList results={rows.filter((row) => row.status !== "pass")} />
            </li>
          );
        })}
      </ol>

      <div className="welcome-actions">
        <button type="button" onClick={() => void rerun()} disabled={running}>
          {running ? "Checking…" : "Re-check environment"}
        </button>
        {/* Always enabled. An unfinished setup is a reason to warn, not to trap
            someone on a screen they have understood and want to leave; the
            doctor banner follows them to every other view anyway, and the queue
            still refuses to start on its own terms. */}
        <button type="button" onClick={() => void handleFinish()} disabled={dismissing}>
          {dismissing ? "Finishing…" : "Go to the board"}
        </button>
      </div>
    </div>
  );
}
