// App-wide state: volume, last scan report, live scan progress and the current selection, plus the
// Downloads tab's own scan and selection (kept apart from the main report, never in its totals).
// Screens read it through `useApp()`; everything that talks to the backend goes through `api`.

import { createContext, type ReactNode, useCallback, useContext, useEffect, useMemo, useState } from "react";
import { api } from "./api";
import type { CleanResult } from "./bindings/CleanResult";
import type { ScanProgress } from "./bindings/ScanProgress";
import type { ScanReport } from "./bindings/ScanReport";
import type { VolumeInfo } from "./bindings/VolumeInfo";
import { DOWNLOADS_RULE } from "./lib/downloads";
import { defaultSelection, isSelectable } from "./lib/findings";
import { errorMessage } from "./lib/format";

export type Screen = "overview" | "findings" | "downloads" | "history" | "settings";

const SCREENS: readonly string[] = ["overview", "findings", "downloads", "history", "settings"] satisfies Screen[];

function isScreen(value: string): value is Screen {
  return SCREENS.includes(value);
}

interface AppState {
  screen: Screen;
  setScreen: (screen: Screen) => void;

  volume: VolumeInfo | null;
  volumeError: string | null;
  refreshVolume: () => Promise<void>;

  report: ScanReport | null;
  scanning: boolean;
  progress: ScanProgress | null;
  scanError: string | null;
  startScan: () => Promise<void>;

  selection: ReadonlySet<string>;
  setSelected: (ids: string[], selected: boolean) => void;
  clearSelection: () => void;

  /** The Downloads tab's scan (`downloads.browse` only): its own report, never the last scan. */
  downloads: ScanReport | null;
  downloadsScanning: boolean;
  downloadsError: string | null;
  scanDownloads: () => Promise<void>;
  downloadsSelection: ReadonlySet<string>;
  setDownloadsSelection: (update: (prev: ReadonlySet<string>) => ReadonlySet<string>) => void;

  /** The finding ids the Confirm sheet is showing; `null` while it is closed. */
  confirmIds: string[] | null;
  confirmOpen: boolean;
  /** `true` opens the sheet for the Findings selection; `false` closes it. */
  setConfirmOpen: (open: boolean) => void;
  /** Opens the sheet for these ids (the Downloads tab). */
  openConfirm: (ids: string[]) => void;
  /** Called by the Confirm dialog once a real clean finished. */
  applyCleanResult: (result: CleanResult) => Promise<void>;

  /** Bumped after every scan/clean so history views refetch. */
  dataVersion: number;
}

const AppContext = createContext<AppState | null>(null);

export function AppProvider({ children }: { children: ReactNode }) {
  const [screen, setScreen] = useState<Screen>("overview");
  const [volume, setVolume] = useState<VolumeInfo | null>(null);
  const [volumeError, setVolumeError] = useState<string | null>(null);
  const [report, setReport] = useState<ScanReport | null>(null);
  const [scanning, setScanning] = useState(false);
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [scanError, setScanError] = useState<string | null>(null);
  const [selection, setSelection] = useState<ReadonlySet<string>>(new Set());
  const [downloads, setDownloads] = useState<ScanReport | null>(null);
  const [downloadsScanning, setDownloadsScanning] = useState(false);
  const [downloadsError, setDownloadsError] = useState<string | null>(null);
  const [downloadsSelection, setDownloadsSelectionState] = useState<ReadonlySet<string>>(new Set());
  const [confirmIds, setConfirmIds] = useState<string[] | null>(null);
  const [dataVersion, setDataVersion] = useState(0);

  const refreshVolume = useCallback(async () => {
    try {
      setVolume(await api.getVolume());
      setVolumeError(null);
    } catch (e) {
      setVolumeError(errorMessage(e));
    }
  }, []);

  useEffect(() => {
    refreshVolume();
    // A report survives a webview reload in the Tauri state; pick it up.
    api.lastReport().then(
      (r) => {
        if (r) {
          setReport(r);
          setSelection(defaultSelection(r.findings));
        }
      },
      () => {},
    );
    const unlisten = api.onScanProgress(setProgress);
    // "Scan Now" in the menu bar runs the same scan in the backend; mirror it here.
    const unlistenExternal = api.onExternalScan((event) => {
      if (event.kind === "started") {
        setScanning(true);
        setScanError(null);
        setProgress(null);
        return;
      }
      setScanning(false);
      setProgress(null);
      if (event.kind === "failed") {
        setScanError(event.message);
        return;
      }
      setReport(event.report);
      setVolume(event.report.volume);
      setSelection(defaultSelection(event.report.findings));
      setDataVersion((v) => v + 1);
    });
    // Growth lines in the menu bar open History.
    const unlistenNavigate = api.onNavigate((target) => {
      if (isScreen(target)) setScreen(target);
    });
    // Coming back from System Settings (Full Disk Access) or Finder (emptied Trash): re-read.
    const onFocus = () => refreshVolume();
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("focus", onFocus);
      for (const stop of [unlisten, unlistenExternal, unlistenNavigate]) stop.then((fn) => fn());
    };
  }, [refreshVolume]);

  const startScan = useCallback(async () => {
    setScanning(true);
    setScanError(null);
    setProgress(null);
    try {
      const result = await api.startScan({ ruleIds: null, categories: null });
      setReport(result);
      setVolume(result.volume);
      setSelection(defaultSelection(result.findings));
      setDataVersion((v) => v + 1);
    } catch (e) {
      setScanError(errorMessage(e));
    } finally {
      setScanning(false);
      setProgress(null);
    }
  }, []);

  const scanDownloads = useCallback(async () => {
    setDownloadsScanning(true);
    setDownloadsError(null);
    try {
      const result = await api.startScan({ ruleIds: [DOWNLOADS_RULE], categories: null });
      setDownloads(result);
      // Keep the picks that are still there.
      const ids = new Set(result.findings.map((f) => f.id));
      setDownloadsSelectionState((prev) => new Set([...prev].filter((id) => ids.has(id))));
    } catch (e) {
      setDownloadsError(errorMessage(e));
    } finally {
      setDownloadsScanning(false);
    }
  }, []);

  const setDownloadsSelection = useCallback(
    (update: (prev: ReadonlySet<string>) => ReadonlySet<string>) => setDownloadsSelectionState(update),
    [],
  );

  const setConfirmOpen = useCallback(
    (open: boolean) =>
      setConfirmIds(
        open ? (report?.findings ?? []).filter((f) => selection.has(f.id) && isSelectable(f)).map((f) => f.id) : null,
      ),
    [report, selection],
  );

  const openConfirm = useCallback((ids: string[]) => setConfirmIds(ids), []);

  const setSelected = useCallback((ids: string[], selected: boolean) => {
    setSelection((prev) => {
      const next = new Set(prev);
      for (const id of ids) {
        if (selected) next.add(id);
        else next.delete(id);
      }
      return next;
    });
  }, []);

  const clearSelection = useCallback(() => setSelection(new Set()), []);

  const applyCleanResult = useCallback(
    async (result: CleanResult) => {
      if (result.dryRun) return;
      const done = new Set(result.items.filter((i) => i.outcome === "done").map((i) => i.findingId));
      setSelection((prev) => new Set([...prev].filter((id) => !done.has(id))));
      setDownloadsSelectionState((prev) => new Set([...prev].filter((id) => !done.has(id))));
      setDownloads((prev) => (prev ? { ...prev, findings: prev.findings.filter((f) => !done.has(f.id)) } : prev));
      // The backend drops cleaned findings from its stored report; mirror that.
      let next: ScanReport | null = null;
      try {
        next = await api.lastReport();
      } catch {
        next = null;
      }
      setReport((prev) => next ?? (prev ? { ...prev, findings: prev.findings.filter((f) => !done.has(f.id)) } : prev));
      await refreshVolume();
      setDataVersion((v) => v + 1);
    },
    [refreshVolume],
  );

  const value = useMemo<AppState>(
    () => ({
      screen,
      setScreen,
      volume,
      volumeError,
      refreshVolume,
      report,
      scanning,
      progress,
      scanError,
      startScan,
      selection,
      setSelected,
      clearSelection,
      downloads,
      downloadsScanning,
      downloadsError,
      scanDownloads,
      downloadsSelection,
      setDownloadsSelection,
      confirmIds,
      confirmOpen: confirmIds !== null,
      setConfirmOpen,
      openConfirm,
      applyCleanResult,
      dataVersion,
    }),
    [
      screen,
      volume,
      volumeError,
      refreshVolume,
      report,
      scanning,
      progress,
      scanError,
      startScan,
      selection,
      setSelected,
      clearSelection,
      downloads,
      downloadsScanning,
      downloadsError,
      scanDownloads,
      downloadsSelection,
      setDownloadsSelection,
      confirmIds,
      setConfirmOpen,
      openConfirm,
      applyCleanResult,
      dataVersion,
    ],
  );

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useApp(): AppState {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useApp outside AppProvider");
  return ctx;
}

/** Loads something async whenever `deps` change; keeps the previous data while refetching. */
export function useAsync<T>(load: () => Promise<T>, deps: unknown[]) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // biome-ignore-start lint/correctness/useExhaustiveDependencies: callers pass the dependencies of `load`
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    load().then(
      (d) => {
        if (cancelled) return;
        setData(d);
        setError(null);
        setLoading(false);
      },
      (e) => {
        if (cancelled) return;
        setError(errorMessage(e));
        setLoading(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, deps);
  // biome-ignore-end lint/correctness/useExhaustiveDependencies: see above
  return { data, error, loading };
}
