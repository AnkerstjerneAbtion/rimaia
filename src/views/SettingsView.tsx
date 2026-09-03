import { ConcurrencySection } from "./settings/ConcurrencySection";
import { DeveloperSection } from "./settings/DeveloperSection";
import { DoctorSection } from "./settings/DoctorSection";
import { InstructionsSection } from "./settings/InstructionsSection";
import { McpSection } from "./settings/McpSection";
import { RepositoriesSection } from "./settings/RepositoriesSection";
import { StorageSection } from "./settings/StorageSection";
import { StrategySection } from "./settings/StrategySection";

export function SettingsView() {
  return (
    <div className="view">
      <header className="view-header">
        <h2>Settings</h2>
        <p>
          The environment, repositories, storage, the MCP server, how many runs happen at
          once, and the instructions and execution strategy every run receives.
        </p>
      </header>

      {/* Each section owns its own error state and <ErrorBanner> instead of
          bubbling up to one shared banner here, so tasks 003 and 006 can add
          sections without touching this composer. */}
      {/* First on the page: a blocking check is the most urgent thing here,
          because it is the one that silently costs a whole night. */}
      <DoctorSection />
      <RepositoriesSection />
      {/* Directly under the repositories, because the per-repository cap lives
          on each row above and this is the global limit those caps sit inside.
          Read the other way round the number here means nothing: raising it
          starts nothing extra until some repository is allowed to hold two. */}
      <ConcurrencySection />
      <InstructionsSection />
      {/* Between the instructions and the MCP server on purpose: the strategy
          decides what a run is spawned with, which reads as the next thing
          after what it is told — and a planned task's proposal arrives back
          through the server described below it. */}
      <StrategySection />
      <McpSection />
      <StorageSection />
      {import.meta.env.DEV && <DeveloperSection />}
    </div>
  );
}
