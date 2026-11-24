// Typed wrappers over the Tauri commands in src-tauri/src/commands.rs (ARCHITECTURE.md §7).
// With VITE_MOCK=1 every call is served from fixtures/sample-report.json, no Tauri needed.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CleanItemResult } from "./bindings/CleanItemResult";
import type { CleanPlan } from "./bindings/CleanPlan";
import type { CleanRequest } from "./bindings/CleanRequest";
import type { CleanResult } from "./bindings/CleanResult";
import type { FreeSpacePoint } from "./bindings/FreeSpacePoint";
import type { GrowthItem } from "./bindings/GrowthItem";
import type { RuleDef } from "./bindings/RuleDef";
import type { ScanOptions } from "./bindings/ScanOptions";
import type { ScanProgress } from "./bindings/ScanProgress";
import type { ScanReport } from "./bindings/ScanReport";
import type { VolumeInfo } from "./bindings/VolumeInfo";

export const MOCK = import.meta.env.VITE_MOCK === "1";

export const SCAN_PROGRESS_EVENT = "scan://progress";
export const CLEAN_ITEM_EVENT = "clean://item";

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
  openFullDiskAccessSettings(): Promise<void>;
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
  openFullDiskAccessSettings: () => invoke<void>("open_full_disk_access_settings"),
  onScanProgress: (handler) => listen<ScanProgress>(SCAN_PROGRESS_EVENT, (e) => handler(e.payload)),
  onCleanItem: (handler) => listen<CleanItemResult>(CLEAN_ITEM_EVENT, (e) => handler(e.payload)),
};

async function loadFixture(): Promise<ScanReport> {
  // JSON imports widen string unions to `string`; the Rust test `sample_report_fixture_matches_model`
  // guarantees the fixture matches the ScanReport type exactly.
  const fixture = await import("../../../fixtures/sample-report.json");
  return fixture.default as unknown as ScanReport;
}

let mockLastReport: ScanReport | null = null;

const mockApi: Api = {
  getVolume: async () => (await loadFixture()).volume,
  listRules: async () => [],
  startScan: async (opts) => {
    const report = await loadFixture();
    const findings = report.findings.filter(
      (f) =>
        (opts.ruleIds === null || opts.ruleIds.includes(f.ruleId)) &&
        (opts.categories === null || opts.categories.includes(f.category)),
    );
    mockLastReport = { ...report, findings };
    return mockLastReport;
  },
  lastReport: async () => mockLastReport,
  planClean: async (req) => {
    const report = mockLastReport ?? (await loadFixture());
    const items = report.findings.filter((f) => req.findingIds.includes(f.id));
    return {
      items,
      totalBytes: items.reduce((sum, f) => sum + f.bytes, 0),
      warnings: items.flatMap((f) => f.blockers.map((b) => `${f.label}: ${b}`)),
    };
  },
  executeClean: async (req) => {
    const plan = await mockApi.planClean(req);
    const items = plan.items.map((f) => ({ findingId: f.id, ok: true, bytes: f.bytes, message: null }));
    const done = req.dryRun ? 0 : plan.totalBytes;
    return {
      dryRun: req.dryRun,
      mode: req.mode,
      items,
      freedBytes: req.mode === "delete" ? done : 0,
      movedToTrashBytes: req.mode === "trash" ? done : 0,
    };
  },
  historyGrowth: async () => [],
  historyFreeSpace: async () => [],
  revealInFinder: async (path) => console.info("[mock] reveal in Finder:", path),
  openFullDiskAccessSettings: async () => console.info("[mock] open Full Disk Access settings"),
  onScanProgress: async () => () => {},
  onCleanItem: async () => () => {},
};

export const api: Api = MOCK ? mockApi : tauriApi;
