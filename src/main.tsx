import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBanner } from "./components/ErrorBanner";
import { ClientCapabilitiesProvider } from "./lib/capabilities";
import { toRimaiaError } from "./lib/commands";
import { bootstrapClient } from "./lib/client";

const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

// `App` mounts only once the transports are chosen. When they cannot be,
// there is nothing behind the banner to dismiss it onto.
bootstrapClient().then(
  (capabilities) =>
    root.render(
      <React.StrictMode>
        <ClientCapabilitiesProvider capabilities={capabilities}>
          <App />
        </ClientCapabilitiesProvider>
      </React.StrictMode>,
    ),
  (thrown) => root.render(<ErrorBanner error={toRimaiaError(thrown)} />),
);
