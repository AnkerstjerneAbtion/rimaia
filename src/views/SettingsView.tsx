import { DeveloperSection } from "./settings/DeveloperSection";
import { StorageSection } from "./settings/StorageSection";

export function SettingsView() {
  return (
    <div className="view">
      <header className="view-header">
        <h2>Settings</h2>
        <p>Repositories, base instructions and run behaviour arrive in task 006.</p>
      </header>

      {/* Each section owns its own error state and <ErrorBanner> instead of
          bubbling up to one shared banner here, so tasks 003 and 006 can add
          sections without touching this composer. */}
      <StorageSection />
      {import.meta.env.DEV && <DeveloperSection />}
    </div>
  );
}
