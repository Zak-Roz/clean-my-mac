// Typed wrappers over the Tauri commands in src-tauri/src/commands.rs (ARCHITECTURE.md §7).
// With VITE_MOCK=1 every call is served by ./mock.ts (fixtures/sample-report.json + simulations), no Tauri needed.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AgentStatus } from "./bindings/AgentStatus";
import type { CleanItemResult } from "./bindings/CleanItemResult";
import type { CleanPlan } from "./bindings/CleanPlan";
import type { CleanRequest } from "./bindings/CleanRequest";
import type { CleanResult } from "./bindings/CleanResult";
import type { FreeSpacePoint } from "./bindings/FreeSpacePoint";
import type { GrowthItem } from "./bindings/GrowthItem";
import type { MonitorSettings } from "./bindings/MonitorSettings";
import type { RuleDef } from "./bindings/RuleDef";
import type { ScanOptions } from "./bindings/ScanOptions";
import type { ScanProgress } from "./bindings/ScanProgress";
import type { ScanReport } from "./bindings/ScanReport";
import type { VolumeInfo } from "./bindings/VolumeInfo";
import { createMockApi } from "./mock";

export const MOCK = import.meta.env.VITE_MOCK === "1";

export const SCAN_PROGRESS_EVENT = "scan://progress";
export const CLEAN_ITEM_EVENT = "clean://item";
// Scans started from the menu bar (src-tauri/src/tray.rs), and its "open History" items.
export const SCAN_STARTED_EVENT = "scan://started";
export const SCAN_FINISHED_EVENT = "scan://finished";
export const SCAN_FAILED_EVENT = "scan://failed";
export const NAVIGATE_EVENT = "app://navigate";

/** A scan the window did not start (menu bar "Scan Now"). */
export type ExternalScanEvent =
  | { kind: "started" }
  | { kind: "finished"; report: ScanReport }
  | { kind: "failed"; message: string };

export interface Api {
  getVolume(): Promise<VolumeInfo>;
  listRules(): Promise<RuleDef[]>;
  startScan(opts: ScanOptions): Promise<ScanReport>;
  lastReport(): Promise<ScanReport | null>;
  planClean(req: CleanRequest): Promise<CleanPlan>;
  executeClean(req: CleanRequest): Promise<CleanResult>;
  historyGrowth(days: number): Promise<GrowthItem[]>;
  historyFreeSpace(days: number): Promise<FreeSpacePoint[]>;
  revealInFinder(path: string): Promise<void>;
  /** Opens ~/.Trash in Finder (there is no Empty Trash action). */
  revealTrash(): Promise<void>;
  openFullDiskAccessSettings(): Promise<void>;
  /** Menu bar warning level and background-scan alert thresholds (config.toml, read-only here). */
  monitorSettings(): Promise<MonitorSettings>;
  agentStatus(): Promise<AgentStatus>;
  /** Installs the daily background scan (LaunchAgent). Only on an explicit user action. */
  agentInstall(): Promise<AgentStatus>;
  agentUninstall(): Promise<AgentStatus>;
  onExternalScan(handler: (event: ExternalScanEvent) => void): Promise<UnlistenFn>;
  onNavigate(handler: (screen: string) => void): Promise<UnlistenFn>;
  onScanProgress(handler: (progress: ScanProgress) => void): Promise<UnlistenFn>;
  onCleanItem(handler: (item: CleanItemResult) => void): Promise<UnlistenFn>;
}

const tauriApi: Api = {
  getVolume: () => invoke<VolumeInfo>("get_volume"),
  listRules: () => invoke<RuleDef[]>("list_rules"),
  startScan: (opts) => invoke<ScanReport>("start_scan", { opts }),
  lastReport: () => invoke<ScanReport | null>("last_report"),
  planClean: (req) => invoke<CleanPlan>("plan_clean", { req }),
  executeClean: (req) => invoke<CleanResult>("execute_clean", { req }),
  historyGrowth: (days) => invoke<GrowthItem[]>("history_growth", { days }),
  historyFreeSpace: (days) => invoke<FreeSpacePoint[]>("history_free_space", { days }),
  revealInFinder: (path) => invoke<void>("reveal_in_finder", { path }),
  revealTrash: () => invoke<void>("reveal_trash"),
  openFullDiskAccessSettings: () => invoke<void>("open_full_disk_access_settings"),
  monitorSettings: () => invoke<MonitorSettings>("monitor_settings"),
  agentStatus: () => invoke<AgentStatus>("agent_status"),
  agentInstall: () => invoke<AgentStatus>("agent_install"),
  agentUninstall: () => invoke<AgentStatus>("agent_uninstall"),
  onExternalScan: async (handler) => {
    const stops = await Promise.all([
      listen(SCAN_STARTED_EVENT, () => handler({ kind: "started" })),
      listen<ScanReport>(SCAN_FINISHED_EVENT, (e) => handler({ kind: "finished", report: e.payload })),
      listen<string>(SCAN_FAILED_EVENT, (e) => handler({ kind: "failed", message: e.payload })),
    ]);
    return () => {
      for (const stop of stops) stop();
    };
  },
  onNavigate: (handler) => listen<string>(NAVIGATE_EVENT, (e) => handler(e.payload)),
  onScanProgress: (handler) => listen<ScanProgress>(SCAN_PROGRESS_EVENT, (e) => handler(e.payload)),
  onCleanItem: (handler) => listen<CleanItemResult>(CLEAN_ITEM_EVENT, (e) => handler(e.payload)),
};

// Constructed only in mock mode, so production builds tree-shake the mock and the fixture away.
export const api: Api = MOCK ? createMockApi() : tauriApi;
