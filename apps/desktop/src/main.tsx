import React from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import StartupFailureScreen, { type StartupFailure } from "./StartupFailureScreen";
import V2Concepts, { v2ConceptFromSearch } from "./V2Concepts";
import V2R2, { v2r2FromSearch } from "./V2R2";
import { initLocale } from "./i18n";
import "./index.css";

// I18N-1: locale (host OS → persisted preference) and the root dir/lang.
initLocale();

// RIG-ONLY (never ship): F9 stutter drill, dev builds behind an env gate.
if (import.meta.env.DEV && import.meta.env.VITE_STUTTER_DRILL === "1") {
  void import("./stutterDrill").then((m) => m.installDrill());
}

const concept = v2ConceptFromSearch(window.location.search);
const v2r2Screen = v2r2FromSearch(window.location.search);

async function startupFailure(): Promise<StartupFailure | null> {
  try {
    return await invoke<StartupFailure | null>("startup_failure");
  } catch {
    return null;
  }
}

void startupFailure().then((failure) => {
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <ErrorBoundary>
        {failure != null ? (
          <StartupFailureScreen failure={failure} />
        ) : v2r2Screen ? (
          <V2R2 screen={v2r2Screen} />
        ) : concept ? (
          <V2Concepts concept={concept} />
        ) : (
          <App />
        )}
      </ErrorBoundary>
    </React.StrictMode>,
  );
});
