import React from "react";
import ReactDOM from "react-dom/client";

import App from "../App";
import { setCommandTransport } from "../lib/commands";
import { setEventTransport } from "../lib/events";
import { buildScenario, isScenarioName, SCENARIO_NAMES } from "./fixtures/seed";
import { createFixtureTransports } from "./fixtures/transport";

// Fixture mode answers for a backend that does not exist. It must never be
// reachable from a build a user runs: `vite build` takes `index.html` alone, so
// this file is not in the production graph, and this throws as a second line
// of defence if something ever pulls it in.
if (!import.meta.env.DEV) {
  throw new Error("fixture mode is a dev-server entry and cannot run in a production build");
}

const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);
const requested = new URLSearchParams(window.location.search).get("scenario") ?? "busy";

if (isScenarioName(requested)) {
  const transports = createFixtureTransports(buildScenario(requested));
  setCommandTransport(transports.command);
  setEventTransport(transports.event);
  root.render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
} else {
  root.render(
    <div style={{ padding: "2rem", fontFamily: "system-ui" }}>
      <h1>Unknown fixture scenario “{requested}”</h1>
      <p>Valid scenarios:</p>
      <ul>
        {SCENARIO_NAMES.map((name) => (
          <li key={name}>
            <a href={`?scenario=${name}`}>{name}</a>
          </li>
        ))}
      </ul>
    </div>,
  );
}
