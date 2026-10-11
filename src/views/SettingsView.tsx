import { useHasMachine } from "../lib/capabilities";
import { useTeams } from "../lib/teams";
import { ConcurrencySection } from "./settings/ConcurrencySection";
import { DeveloperSection } from "./settings/DeveloperSection";
import { DoctorSection } from "./settings/DoctorSection";
import { InstructionsSection } from "./settings/InstructionsSection";
import { McpSection } from "./settings/McpSection";
import { RepositoriesSection } from "./settings/RepositoriesSection";
import { ReviewSection } from "./settings/ReviewSection";
import { RunnersSection } from "./settings/RunnersSection";
import { SchedulesSection } from "./settings/SchedulesSection";
import { StorageSection } from "./settings/StorageSection";
import { StrategySection } from "./settings/StrategySection";
import { TeamSection } from "./settings/TeamSection";

/**
 * One entry in the index at the top of the page and one anchor in the flow
 * below it. `id` is the fragment both ends share.
 */
interface SettingsBand {
  readonly id: string;
  readonly title: string;
  /** What the group is for, one line, in the index. */
  readonly blurb: string;
  readonly sections: readonly { readonly id: string; readonly name: string }[];
}

/**
 * Nine sections, four groups.
 *
 * The page used to be nine panels in a single scroll with nothing between
 * them, which said that a machine-wide preflight check, a per-repository
 * consent grant and a developer toggle are all the same kind of thing. They
 * are not, and by the ninth panel a reader has lost the thread of which
 * question they are answering.
 *
 * **The order is unchanged.** Every adjacency in it is load-bearing and
 * argued for below — the per-repository cap sits on a repository row and the
 * global limit has to be read after it, a schedule overrides both while its
 * window is open, the strategy is what a run is spawned with and reads as the
 * next thing after what it is told. Grouping is the smallest change that adds
 * structure without invalidating any of that: every group is a run of
 * consecutive sections, so nothing moved past anything else.
 *
 * The index is the second half of it. A jump list is not decoration on a page
 * this long: "where is the unattended-runs toggle" is a question this app
 * answers ten times during setup, and scrolling for it is the whole cost.
 */
const BANDS: readonly SettingsBand[] = [
  {
    id: "settings-environment",
    title: "Environment",
    blurb: "What this machine has to provide before anything can run.",
    sections: [{ id: "settings-doctor", name: "Doctor" }],
  },
  {
    id: "settings-work",
    title: "What runs, and when",
    blurb: "The repositories, how many at once, and the windows they run in.",
    sections: [
      { id: "settings-repositories", name: "Repositories" },
      { id: "settings-concurrency", name: "Concurrency" },
      { id: "settings-schedules", name: "Schedules" },
    ],
  },
  {
    id: "settings-brief",
    title: "What every run is told",
    blurb: "The prompt every task inherits, and what it is spawned with.",
    sections: [
      { id: "settings-instructions", name: "Instructions" },
      { id: "settings-review", name: "Review" },
      { id: "settings-strategy", name: "Strategy" },
    ],
  },
  {
    id: "settings-data",
    title: "Integrations and data",
    blurb: "The MCP server, and what Rimaia has written to this disk.",
    sections: [
      { id: "settings-mcp", name: "MCP" },
      { id: "settings-storage", name: "Storage" },
    ],
  },
];

/**
 * The team on screen and its people (task 051): first wherever there is a
 * team switcher, because who may change the settings below is decided here.
 * Solo has one team and no people to manage, so it has no such group.
 */
const TEAM_BAND: SettingsBand = {
  id: "settings-team",
  title: "Team",
  blurb: "Who is in the team on screen, and who may change it.",
  sections: [{ id: "settings-team-members", name: "Members" }],
};

/**
 * The same groups with a browser's sections (task 050 Scope 6). Concurrency,
 * Schedules, MCP, Storage, Doctor and Developer read and change this machine,
 * and a browser has none: one Runners section stands in for all six, saying
 * where each runner's settings are changed. Repositories, Instructions, Review
 * and Strategy are the team's, so they stay, each with its own gates.
 */
const BROWSER_BANDS: readonly SettingsBand[] = [
  {
    id: "settings-machines",
    title: "Machines",
    blurb: "The machines that run your tasks, and where to change their settings.",
    sections: [{ id: "settings-runners", name: "Runners" }],
  },
  {
    id: "settings-work",
    title: "What runs",
    blurb: "The repositories your team queues work for.",
    sections: [{ id: "settings-repositories", name: "Repositories" }],
  },
  BANDS[2],
];

export function SettingsView() {
  // Doctor, concurrency, schedules, MCP and storage: this machine's, so they
  // exist only where there is one (task 050 Scope 6).
  const hasMachine = useHasMachine();
  const hasTeams = useTeams() !== null;
  const bands = [...(hasTeams ? [TEAM_BAND] : []), ...(hasMachine ? BANDS : BROWSER_BANDS)];
  if (!hasMachine) return <BrowserSettingsView bands={bands} hasTeams={hasTeams} />;
  return (
    <div className="view settings-view">
      <header className="view-header">
        <h2>Settings</h2>
        <p>
          The environment, repositories, storage, the MCP server, how many runs happen at
          once and when they happen, and the instructions and execution strategy every run
          receives.
        </p>
      </header>

      {/* Sticky, so it is still reachable from the bottom of Storage. */}
      <nav className="settings-index" aria-label="Settings sections">
        {bands.map((band) => (
          <div key={band.id} className="settings-index-group">
            <a className="settings-index-label" href={`#${band.id}`}>
              {band.title}
            </a>
            <ul>
              {band.sections.map((section) => (
                <li key={section.id}>
                  <a href={`#${section.id}`}>{section.name}</a>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </nav>

      {/* Each section owns its own error state and <ErrorBanner> instead of
          bubbling up to one shared banner here, so tasks 003 and 006 can add
          sections without touching this composer. */}
      {hasTeams && (
        <SettingsBandView band={TEAM_BAND}>
          <div className="settings-slot" id="settings-team-members">
            <TeamSection />
          </div>
        </SettingsBandView>
      )}

      <SettingsBandView band={BANDS[0]}>
        {/* First on the page: a blocking check is the most urgent thing here,
            because it is the one that silently costs a whole night. */}
        <div className="settings-slot" id="settings-doctor">
          <DoctorSection />
        </div>
      </SettingsBandView>

      <SettingsBandView band={BANDS[1]}>
        <div className="settings-slot" id="settings-repositories">
          <RepositoriesSection />
        </div>
        {/* Directly under the repositories, because the per-repository cap lives
            on each row above and this is the global limit those caps sit inside.
            Read the other way round the number here means nothing: raising it
            starts nothing extra until some repository is allowed to hold two. */}
        <div className="settings-slot" id="settings-concurrency">
          <ConcurrencySection />
        </div>
        {/* Directly under the concurrency limit, because the two answer adjacent
            halves of one question — how much runs at once, and when it runs at
            all — and because a schedule carries its own mode and limit that
            override the ones above while its window is open. Read the other way
            round, a schedule's "several at once" would look like it contradicted
            the setting rather than superseding it for one night. */}
        <div className="settings-slot" id="settings-schedules">
          <SchedulesSection />
        </div>
      </SettingsBandView>

      <SettingsBandView band={BANDS[2]}>
        <div className="settings-slot" id="settings-instructions">
          <InstructionsSection />
        </div>
        {/* Review sits with the instructions it extends: what a review run is
            told is one more thing a run receives besides its own plan, and its
            loop multiplies what every run costs, so it is read next to the
            strategy that sets what each run is spawned with. */}
        <div className="settings-slot" id="settings-review">
          <ReviewSection />
        </div>
        {/* Between the instructions and the MCP server on purpose: the strategy
            decides what a run is spawned with, which reads as the next thing
            after what it is told — and a planned task's proposal arrives back
            through the server described below it. */}
        <div className="settings-slot" id="settings-strategy">
          <StrategySection />
        </div>
      </SettingsBandView>

      <SettingsBandView band={BANDS[3]}>
        <div className="settings-slot" id="settings-mcp">
          <McpSection />
        </div>
        <div className="settings-slot" id="settings-storage">
          <StorageSection />
        </div>
        {/* Not in the index: it does not exist in a release build, and an index
            entry that is there for developers only is a promise the shipped app
            cannot keep. */}
        {import.meta.env.DEV && <DeveloperSection />}
      </SettingsBandView>
    </div>
  );
}

function BrowserSettingsView({
  bands,
  hasTeams,
}: {
  readonly bands: readonly SettingsBand[];
  readonly hasTeams: boolean;
}) {
  const [machines, work, brief] = hasTeams ? bands.slice(1) : bands;
  return (
    <div className="view settings-view">
      <header className="view-header">
        <h2>Settings</h2>
        <p>
          The team and its people, its repositories, the instructions and execution strategy
          every run receives, and the runners that carry them out.
        </p>
      </header>

      <nav className="settings-index" aria-label="Settings sections">
        {bands.map((band) => (
          <div key={band.id} className="settings-index-group">
            <a className="settings-index-label" href={`#${band.id}`}>
              {band.title}
            </a>
            <ul>
              {band.sections.map((section) => (
                <li key={section.id}>
                  <a href={`#${section.id}`}>{section.name}</a>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </nav>

      {hasTeams && (
        <SettingsBandView band={TEAM_BAND}>
          <div className="settings-slot" id="settings-team-members">
            <TeamSection />
          </div>
        </SettingsBandView>
      )}
      <SettingsBandView band={machines}>
        <div className="settings-slot" id="settings-runners">
          <RunnersSection />
        </div>
      </SettingsBandView>
      <SettingsBandView band={work}>
        <div className="settings-slot" id="settings-repositories">
          <RepositoriesSection />
        </div>
      </SettingsBandView>
      <SettingsBandView band={brief}>
        <div className="settings-slot" id="settings-instructions">
          <InstructionsSection />
        </div>
        <div className="settings-slot" id="settings-review">
          <ReviewSection />
        </div>
        <div className="settings-slot" id="settings-strategy">
          <StrategySection />
        </div>
      </SettingsBandView>
    </div>
  );
}

function SettingsBandView({
  band,
  children,
}: {
  readonly band: SettingsBand;
  readonly children: React.ReactNode;
}) {
  return (
    <section className="settings-band" id={band.id} aria-labelledby={`${band.id}-label`}>
      <div className="settings-band-head">
        <h3 className="settings-band-label" id={`${band.id}-label`}>
          {band.title}
        </h3>
        <p className="settings-band-blurb">{band.blurb}</p>
      </div>
      {children}
    </section>
  );
}
