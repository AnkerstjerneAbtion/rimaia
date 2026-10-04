import { useEffect, useState } from "react";

import { DoctorBanner } from "./components/DoctorBanner";
import { Sidebar } from "./components/Sidebar";
import { useDoctor } from "./hooks/useDoctor";
import { useTasks } from "./hooks/useTasks";
import { getAppInfo, getReviewDigest } from "./lib/commands";
import { openingView, reviewQueue } from "./lib/review";
import { BoardView } from "./views/BoardView";
import { ReviewView } from "./views/ReviewView";
import { AnalyticsView } from "./views/AnalyticsView";
import { RunsView } from "./views/RunsView";
import { SettingsView } from "./views/SettingsView";
import { WelcomeView } from "./views/WelcomeView";
import type { View } from "./types";
import "./styles.css";

function App() {
  // Route state, not a router. Five views with no URLs, no nesting and no deep
  // links to preserve — a router library would be all cost (task 001).
  //
  // `null` is "we do not yet know where to start": the opening view depends on
  // `onboardingDismissed`, and defaulting to the board would flash it before
  // the welcome screen replaced it on a first run (seam-contract D22).
  const [view, setView] = useState<View | null>(null);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const { report, dismiss } = useDoctor();
  // The sidebar's count of what is waiting for a verdict, off the board's own
  // read so it agrees with the queue the review view walks.
  const { state: board } = useTasks(null);
  const reviewCount = reviewQueue(board.tasks).length;

  useEffect(() => {
    getAppInfo().then(
      async (info) => {
        setAppVersion(info.appVersion);
        // A digest is only worth reading once the welcome screen is out of the
        // way; a failed read opens the board, for the same reason a failed
        // `get_app_info` does below.
        const digest = info.onboardingDismissed
          ? await getReviewDigest().catch(() => null)
          : null;
        setView(openingView(info.onboardingDismissed, digest));
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
      <Sidebar
        current={view}
        onNavigate={setView}
        version={appVersion}
        reviewCount={reviewCount}
      />
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
        {view === "review" && <ReviewView />}
        {view === "runs" && <RunsView />}
        {view === "analytics" && <AnalyticsView />}
        {view === "settings" && <SettingsView />}
        {view === "welcome" && <WelcomeView onFinish={() => setView("board")} />}
      </main>
    </div>
  );
}

export default App;
