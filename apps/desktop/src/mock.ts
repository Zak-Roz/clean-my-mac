// Mock backend for `VITE_MOCK=1` (ARCHITECTURE.md §7): serves fixtures/sample-report.json and
// simulates everything else so every screen can be exercised in a plain browser:
// - scans emit `scan://progress` over ~2.5 s;
// - plan/execute re-validate like the core will: one item turns out blocked ("Claude is running"),
//   the Docker prune fails, the rest succeed; cleaned findings disappear from later scans;
// - history returns ~90 days of synthetic free-space points and growth rows;
// - "Open Settings" for Full Disk Access grants it, so the Trash size appears on the next check.

import type { UnlistenFn } from "@tauri-apps/api/event";
import type { Api } from "./api";
import type { CleanItemResult } from "./bindings/CleanItemResult";
import type { CleanPlan } from "./bindings/CleanPlan";
import type { CleanRequest } from "./bindings/CleanRequest";
import type { CleanResult } from "./bindings/CleanResult";
import type { Finding } from "./bindings/Finding";
import type { FreeSpacePoint } from "./bindings/FreeSpacePoint";
import type { GrowthItem } from "./bindings/GrowthItem";
import type { ScanProgress } from "./bindings/ScanProgress";
import type { ScanReport } from "./bindings/ScanReport";
import type { VolumeInfo } from "./bindings/VolumeInfo";
import { dedupedTotal, isPermanentAction } from "./lib/findings";

const GB = 1e9;
const DAY_MS = 86_400_000;

async function loadFixture(): Promise<ScanReport> {
  // JSON imports widen string unions to `string`; the Rust test `sample_report_fixture_matches_model`
  // guarantees the fixture matches the ScanReport type exactly.
  const fixture = await import("../../../fixtures/sample-report.json");
  return structuredClone(fixture.default) as unknown as ScanReport;
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** Extra rule ids so the progress bar walks through a realistic catalog. */
const EXTRA_RULES = [
  "cache.pip",
  "cache.cargo-registry",
  "cache.homebrew",
  "cache.pnpm-store",
  "xcode.derived-data",
  "xcode.archives",
  "downloads.installers",
  "downloads.redundant-archives",
  "app-caches.browsers",
  "app-caches.slack",
];

/** Mock re-validation at plan/execute time. */
function revalidate(f: Finding): { blocked?: string; fails?: string } {
  if (f.ruleId === "ai.claude-desktop.vm-bundles") return { blocked: "Claude is running" };
  if (f.command?.[0] === "docker") {
    return {
      fails: "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?",
    };
  }
  return {};
}

export function createMockApi(): Api {
  const progressHandlers = new Set<(p: ScanProgress) => void>();
  const itemHandlers = new Set<(i: CleanItemResult) => void>();
  const cleaned = new Set<string>();
  const recordedScans: FreeSpacePoint[] = [];
  let lastReport: ScanReport | null = null;
  let volume: VolumeInfo | null = null;
  let fullDiskAccess = false;

  async function currentVolume(): Promise<VolumeInfo> {
    if (!volume) volume = { ...(await loadFixture()).volume };
    return { ...volume, trashBytes: fullDiskAccess ? (volume.trashBytes ?? 2.86 * GB) : null };
  }

  async function plan(req: CleanRequest): Promise<CleanPlan> {
    const report = lastReport ?? (await loadFixture());
    const items = report.findings.filter((f) => req.findingIds.includes(f.id));
    const warnings: string[] = [];
    for (const f of items) {
      const { blocked } = revalidate(f);
      if (blocked) warnings.push(`${f.label}: ${blocked} — it will be skipped. Quit it and clean again.`);
    }
    const { bytes, covered } = dedupedTotal(items);
    for (const f of covered) warnings.push(`${f.label} is inside another selected item and is covered by it.`);
    return { items, totalBytes: bytes, warnings };
  }

  const api: Api = {
    getVolume: async () => {
      await sleep(120);
      return currentVolume();
    },
    listRules: async () => [],
    startScan: async (opts) => {
      const started = Date.now();
      const report = await loadFixture();
      const findings = report.findings.filter(
        (f) =>
          !cleaned.has(f.id) &&
          (opts.ruleIds === null || opts.ruleIds.includes(f.ruleId)) &&
          (opts.categories === null || opts.categories.includes(f.category)),
      );
      const rules = [...new Set([...report.findings.map((f) => f.ruleId), ...EXTRA_RULES])];
      const emit = (p: ScanProgress) => {
        for (const h of progressHandlers) h(p);
      };
      const totalRules = rules.length;
      emit({ phase: "volume", ruleId: null, doneRules: 0, totalRules, findings: 0 });
      await sleep(300);
      for (const [i, ruleId] of rules.entries()) {
        const found = findings.filter((f) => rules.indexOf(f.ruleId) < i).length;
        emit({ phase: "rules", ruleId, doneRules: i, totalRules, findings: found });
        await sleep(110 + ((i * 37) % 90));
      }
      emit({ phase: "done", ruleId: null, doneRules: totalRules, totalRules, findings: findings.length });
      const now = new Date().toISOString();
      const vol = await currentVolume();
      lastReport = {
        ...report,
        id: now,
        startedAt: now,
        durationMs: Date.now() - started,
        volume: vol,
        fullDiskAccess,
        findings,
      };
      recordedScans.push({ at: now, freeBytes: vol.freeBytes });
      return lastReport;
    },
    lastReport: async () => lastReport,
    planClean: async (req) => {
      await sleep(350);
      return plan(req);
    },
    executeClean: async (req) => {
      const { items } = await plan(req);
      const results: CleanItemResult[] = [];
      let freedBytes = 0;
      let movedToTrashBytes = 0;
      const covered = new Set(dedupedTotal(items).covered.map((f) => f.id));
      for (const f of items) {
        if (!req.dryRun) await sleep(380);
        const { blocked, fails } = revalidate(f);
        let result: CleanItemResult;
        if (covered.has(f.id)) {
          result = { findingId: f.id, ok: true, bytes: 0, message: "Covered by parent" };
        } else if (blocked) {
          result = { findingId: f.id, ok: false, bytes: 0, message: `Skipped: ${blocked}` };
        } else if (fails && !req.dryRun) {
          result = { findingId: f.id, ok: false, bytes: 0, message: `docker system prune failed: ${fails}` };
        } else {
          result = { findingId: f.id, ok: true, bytes: f.bytes, message: null };
          if (!req.dryRun) {
            if (req.mode === "delete" || isPermanentAction(f)) freedBytes += f.bytes;
            else movedToTrashBytes += f.bytes;
          }
        }
        results.push(result);
        for (const h of itemHandlers) h(result);
      }
      if (!req.dryRun) {
        for (const r of results) if (r.ok) cleaned.add(r.findingId);
        if (lastReport) lastReport = { ...lastReport, findings: lastReport.findings.filter((f) => !cleaned.has(f.id)) };
        const vol = await currentVolume();
        volume = {
          ...vol,
          freeBytes: vol.freeBytes + freedBytes,
          availableImportantBytes:
            vol.availableImportantBytes === null ? null : vol.availableImportantBytes + freedBytes,
          trashBytes: (vol.trashBytes ?? 2.86 * GB) + movedToTrashBytes,
        };
      }
      const result: CleanResult = { dryRun: req.dryRun, mode: req.mode, items: results, freedBytes, movedToTrashBytes };
      return result;
    },
    historyGrowth: async (days) => {
      await sleep(150);
      return syntheticGrowth(days);
    },
    historyFreeSpace: async (days) => {
      await sleep(150);
      // History holds scan snapshots only: synthetic past scans ending at the fixture's volume,
      // then the scans made in this session (a clean shows up at the next scan, as in the app).
      const base = (await loadFixture()).volume.freeBytes;
      const cutoff = Date.now() - days * DAY_MS;
      return [...syntheticFreeSpace(days, base), ...recordedScans.filter((p) => Date.parse(p.at) >= cutoff)];
    },
    revealInFinder: async (path) => console.info("[mock] reveal in Finder:", path),
    openFullDiskAccessSettings: async () => {
      console.info("[mock] open Full Disk Access settings (granting it)");
      fullDiskAccess = true;
    },
    // Each registration gets its own wrapper so registering the same function twice (React
    // StrictMode effects) and unlistening once leaves the other registration intact, like Tauri.
    onScanProgress: async (handler) => {
      const wrapped = (p: ScanProgress) => handler(p);
      progressHandlers.add(wrapped);
      const stop: UnlistenFn = () => progressHandlers.delete(wrapped);
      return stop;
    },
    onCleanItem: async (handler) => {
      const wrapped = (i: CleanItemResult) => handler(i);
      itemHandlers.add(wrapped);
      const stop: UnlistenFn = () => itemHandlers.delete(wrapped);
      return stop;
    },
  };
  return api;
}

/** Deterministic noise in [-1, 1]. */
function noise(i: number): number {
  const x = Math.sin(i * 12.9898) * 43758.5453;
  return (x - Math.floor(x)) * 2 - 1;
}

/**
 * One point per day ending two hours ago at `currentFree`: a steady ~0.6 GB/day decline with two
 * cleanups (21 and 58 days ago) that freed 18 GB and 26 GB.
 */
function syntheticFreeSpace(days: number, currentFree: number): FreeSpacePoint[] {
  const points: FreeSpacePoint[] = [];
  const now = Date.now();
  const cleanups: [number, number][] = [
    [21, 18 * GB],
    [58, 26 * GB],
  ];
  for (let ago = days; ago >= 1; ago -= 1) {
    const reclaimed = cleanups.filter(([day]) => ago > day).reduce((sum, [, bytes]) => sum + bytes, 0);
    const free = currentFree + ago * 0.6 * GB + noise(ago) * 1.1 * GB - reclaimed;
    points.push({ at: new Date(now - ago * DAY_MS).toISOString(), freeBytes: Math.round(free) });
  }
  // The latest synthetic scan matches the fixture, so before any scan the chart agrees with the gauge.
  points.push({ at: new Date(now - 2 * 3_600_000).toISOString(), freeBytes: currentFree });
  return points;
}

const GROWTH: { key: string; label: string; now: number; perDay: number }[] = [
  { key: "downloads.versioned-siblings", label: "Dated copies in Downloads", now: 114.29 * GB, perDay: 1.27 * GB },
  { key: "ai.claude-desktop.vm-bundles", label: "Claude Desktop VM bundle", now: 15.67 * GB, perDay: 0.25 * GB },
  { key: "dev.git-worktrees", label: "Git worktrees", now: 6.62 * GB, perDay: 0.2 * GB },
  { key: "containers.docker", label: "Docker reclaimable data", now: 11.54 * GB, perDay: 0.16 * GB },
  { key: "cache.npm", label: "npm cache", now: 7.3 * GB, perDay: 0.08 * GB },
  { key: "cache.yarn", label: "Yarn cache", now: 4.83 * GB, perDay: -0.04 * GB },
  { key: "xcode.derived-data", label: "Xcode DerivedData", now: 0, perDay: -0.27 * GB },
  {
    key: "downloads.versioned-siblings:/Users/you/Downloads/backups/mongo_17_05_2025",
    label: "mongo_17_05_2025",
    now: 31.47 * GB,
    perDay: 1.05 * GB,
  },
  {
    key: "ai.claude-desktop.vm-bundles:/Users/you/Library/Application Support/ai-profiles/profiles/3f9c2d41-7b6e-4c1a-9e85-2d0f6a7b1c93/gui-data/vm_bundles",
    label: "vm_bundles (profile 3f9c2d41)",
    now: 8.71 * GB,
    perDay: 0.27 * GB,
  },
  {
    key: "dev.git-worktrees:/Users/you/Developer/example-app/.claude/worktrees/bugfix-123",
    label: "bugfix-123",
    now: 2.19 * GB,
    perDay: 0.1 * GB,
  },
  {
    key: "dev.git-worktrees:/Users/you/Developer/example-app/.claude/worktrees/feature-login",
    label: "feature-login",
    now: 2.68 * GB,
    perDay: 0.06 * GB,
  },
  { key: "cache.npm:/Users/you/.npm/_cacache", label: "npm cache", now: 7.3 * GB, perDay: 0.08 * GB },
  { key: "cache.yarn:/Users/you/Library/Caches/Yarn", label: "Yarn cache", now: 4.83 * GB, perDay: -0.04 * GB },
];

function syntheticGrowth(days: number): GrowthItem[] {
  return GROWTH.map(({ key, label, now, perDay }) => {
    const bytesThen = Math.max(0, Math.round(now - perDay * days));
    const bytesNow = Math.round(now);
    return { key, label, bytesThen, bytesNow, delta: bytesNow - bytesThen };
  })
    .filter((g) => g.delta !== 0)
    .sort((a, b) => b.delta - a.delta);
}
