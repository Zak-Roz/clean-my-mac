// App shell: sidebar + the current screen, the Confirm sheet, and ⌘1–3 navigation.

import { useEffect } from "react";
import { Sidebar } from "./components/layout";
import { TooltipProvider } from "./components/ui";
import { ConfirmDialog } from "./screens/ConfirmDialog";
import { Findings } from "./screens/Findings";
import { History } from "./screens/History";
import { Overview } from "./screens/Overview";
import { Settings } from "./screens/Settings";
import { AppProvider, type Screen, useApp } from "./state";

const SHORTCUTS: Record<string, Screen> = {
  "1": "overview",
  "2": "findings",
  "3": "history",
  "4": "settings",
  ",": "settings",
};

function Shell() {
  const { screen, setScreen } = useApp();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = SHORTCUTS[e.key];
      if ((e.metaKey || e.ctrlKey) && target) {
        e.preventDefault();
        setScreen(target);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setScreen]);

  return (
    <div className="flex h-full min-h-0">
      <Sidebar />
      {screen === "overview" && <Overview />}
      {screen === "findings" && <Findings />}
      {screen === "history" && <History />}
      {screen === "settings" && <Settings />}
      <ConfirmDialog />
    </div>
  );
}

export function App() {
  return (
    <AppProvider>
      <TooltipProvider>
        <Shell />
      </TooltipProvider>
    </AppProvider>
  );
}
