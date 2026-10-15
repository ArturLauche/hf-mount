import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { isTauri } from "./lib/api";
import "./styles.css";

if (isTauri) {
  // A desktop app, not a web page: no browser context menu and no reloads
  // that would drop in-flight state. Text fields keep their own menu.
  document.addEventListener("contextmenu", (e) => {
    const t = e.target as HTMLElement | null;
    if (!t?.closest("input, textarea, .selectable")) e.preventDefault();
  });
  document.addEventListener("keydown", (e) => {
    const k = e.key.toLowerCase();
    if (k === "f5" || ((e.ctrlKey || e.metaKey) && (k === "r" || k === "p"))) e.preventDefault();
  });
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
