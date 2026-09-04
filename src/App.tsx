import { useEffect, useState } from "react";

import { DoctorBanner } from "./components/DoctorBanner";
import { Sidebar } from "./components/Sidebar";
import { useDoctor } from "./hooks/useDoctor";
import { getAppInfo } from "./lib/commands";
import { BoardView } from "./views/BoardView";
import { RunsView } from "./views/RunsView";
import { SettingsView } from "./views/SettingsView";
import { WelcomeView } from "./views/WelcomeView";
import type { View } from "./types";
import "./styles.css";

function App() {
  // Route state, not a router. Four views with no URLs, no nesting and no deep
  // links to preserve — a router library would be all cost (task 001).
  //
  // `null` is "we do not yet know where to start": the opening view depends on
  // `onboardingDismissed`, and defaulting to the board would flash it before
  // the welcome screen replaced it on a first run (seam-contract D22).
  const [view, setView] = useState<View | null>(null);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const { report, dismiss } = useDoctor();

  useEffect(() => {
    getAppInfo().then(
      (info) => {
        setView(info.onboardingDismissed ? "board" : "welcome");
        setAppVersion(info.appVersion);
      },
      // A failed read is not a reason to withhold the app. The board is the
      // safe answer: the welcome screen is skippable, and showing it to a
      // returning user would be worse than not showing it to a new one.
      () => setView("board"),
    );
  }, []);

  if (view === null) return <div className="app" />;

  return (
    <div className="app">
      <Sidebar current={view} onNavigate={setView} version={appVersion} />
      <main className="content">
        {/* Above every view, not only Settings: a queue that will not start
            tonight is worth interrupting the board for now. Suppressed on the
            welcome screen, which reports the same checks per step. */}
        {view !== "welcome" && (
          <DoctorBanner
            report={report}
            onOpenSettings={() => setView("settings")}
            onDismiss={(result) => void dismiss(result)}
          />
        )}
        {view === "board" && <BoardView />}
        {view === "runs" && <RunsView />}
        {view === "settings" && <SettingsView />}
        {view === "welcome" && <WelcomeView onFinish={() => setView("board")} />}
      </main>
    </div>
  );
}

export default App;
