import "./i18n";
import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/bricolage-grotesque";
import "katex/dist/katex.min.css";
import "./styles/tailwind.css";
import "./styles/global.css";
import "./styles/onboarding.css";
import "./styles/chat.css";
import "./styles/library.css";
import "./styles/theme-picker.css";
import "./styles/plugins.css";
import "./styles/discover.css";
import "./styles/skills.css";
import "./styles/writing-tools.css";
import "./styles/alerts.css";
import App from "./App";
import { api } from "./lib/api";

// Forward uncaught UI errors into the same log file as the backend, so a crash
// in the webview leaves a trace for "Logs & diagnostics" instead of vanishing.
// Best-effort and self-guarded: logging must never itself throw.
function forwardToLog(prefix: string, detail: unknown) {
  try {
    void api.logFrontend("error", `${prefix}: ${String(detail)}`);
  } catch {
    /* logging is best-effort; ignore */
  }
}
window.addEventListener("error", (e) => forwardToLog("uncaught error", e.message || e.error));
window.addEventListener("unhandledrejection", (e) =>
  forwardToLog("unhandled promise rejection", e.reason),
);

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
