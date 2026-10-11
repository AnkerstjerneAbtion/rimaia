import React from "react";
import ReactDOM from "react-dom/client";

import App from "../App";
import { ClientCapabilitiesProvider } from "../lib/capabilities";
import {
  browserLocalTransport,
  setCommandTransport,
  setCommandTransports,
} from "../lib/commands";
import { setEventTransport, setEventTransports } from "../lib/events";
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
  const scenario = buildScenario(requested);
  const transports = createFixtureTransports(scenario);
  if (scenario.capabilities.mode === "browser") {
    // A browser scenario refuses what a browser refuses, so a local command
    // the interface still sends shows in the picture as the banner it would be.
    setCommandTransports({ board: transports.command, local: browserLocalTransport });
    setEventTransports({ board: transports.event, local: null });
  } else {
    setCommandTransport(transports.command);
    setEventTransport(transports.event);
  }
  root.render(
    <React.StrictMode>
      <ClientCapabilitiesProvider capabilities={scenario.capabilities}>
        <App />
      </ClientCapabilitiesProvider>
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
