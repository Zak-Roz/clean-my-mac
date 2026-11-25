// App shell: sidebar + the current screen, the Confirm sheet, and ⌘1–6 navigation.

import { useEffect } from "react";
import { NAV, Sidebar } from "./components/layout";
import { TooltipProvider } from "./components/ui";
import { ConfirmDialog } from "./screens/ConfirmDialog";
import { Downloads } from "./screens/Downloads";
import { Findings } from "./screens/Findings";
import { History } from "./screens/History";
import { Overview } from "./screens/Overview";
import { Settings } from "./screens/Settings";
import { Storage } from "./screens/Storage";
import { AppProvider, type Screen, useApp } from "./state";

// ⌘1–⌘6 follow the sidebar (Overview, Findings, Downloads, Storage, History, Settings); ⌘, opens Settings.
const SHORTCUTS: Record<string, Screen> = {
  ...Object.fromEntries(NAV.map(({ key, screen }) => [key, screen])),
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
      {screen === "downloads" && <Downloads />}
      {screen === "storage" && <Storage />}
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
