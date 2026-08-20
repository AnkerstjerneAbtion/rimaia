import { useState } from "react";

import { Sidebar } from "./components/Sidebar";
import { BoardView } from "./views/BoardView";
import { RunsView } from "./views/RunsView";
import { SettingsView } from "./views/SettingsView";
import type { View } from "./types";
import "./styles.css";

function App() {
  // Route state, not a router. Three views with no URLs, no nesting and no deep
  // links to preserve — a router library would be all cost (task 001).
  const [view, setView] = useState<View>("board");

  return (
    <div className="app">
      <Sidebar current={view} onNavigate={setView} />
      <main className="content">
        {view === "board" && <BoardView />}
        {view === "runs" && <RunsView />}
        {view === "settings" && <SettingsView />}
      </main>
    </div>
  );
}

export default App;
