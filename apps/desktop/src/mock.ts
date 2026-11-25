// Mock backend for `VITE_MOCK=1` (ARCHITECTURE.md §7): serves fixtures/sample-report.json and
// simulates everything else so every screen can be exercised in a plain browser:
// - scans emit `scan://progress` over ~2.5 s;
// - plan/execute re-scan like the core does (safeguard 1): at plan time one item turns out blocked
//   ("Claude is running") and is left out with a "no longer matches the scan" warning; at execute
//   time the leftover worktree folder has become a real worktree and is skipped, the Docker prune
//   fails, the rest succeed; cleaned findings disappear from later scans;
// - history returns ~90 days of synthetic free-space points and growth rows;
// - "Open Settings" for Full Disk Access grants it, so the Trash size appears on the next check;
// - the background-scan toggle installs/uninstalls a pretend LaunchAgent (nothing touches launchd);
// - the Downloads tab scans a list of invented ~/Downloads entries (no progress events, like the
//   backend's browse scan), thumbnails are generated placeholder SVGs, Quick Look only logs;
// - Storage has no measurement at first; measuring emits `storage://progress` over ~2.5 s and
//   returns the invented breakdown of ./mockStorage.ts, found marks from the current scan.

import type { UnlistenFn } from "@tauri-apps/api/event";
import type { Api } from "./api";
import type { AgentStatus } from "./bindings/AgentStatus";
import type { CleanItemResult } from "./bindings/CleanItemResult";
import type { CleanPlan } from "./bindings/CleanPlan";
import type { CleanRequest } from "./bindings/CleanRequest";
import type { CleanResult } from "./bindings/CleanResult";
import type { Finding } from "./bindings/Finding";
import type { FreeSpacePoint } from "./bindings/FreeSpacePoint";
import type { GrowthItem } from "./bindings/GrowthItem";
import type { GrowthKind } from "./bindings/GrowthKind";
import type { ScanProgress } from "./bindings/ScanProgress";
import type { ScanReport } from "./bindings/ScanReport";
import type { StorageBreakdown } from "./bindings/StorageBreakdown";
import type { StorageProgress } from "./bindings/StorageProgress";
import type { VolumeInfo } from "./bindings/VolumeInfo";
import { DOWNLOADS_RULE, entryKind, extensionOf } from "./lib/downloads";
import { dedupedTotal, isPermanentAction } from "./lib/findings";
import { mockBreakdown, mockChildren, reannotate } from "./mockStorage";

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

/** Mock fresh scan at plan/execute time: what changed since the stored report. */
function revalidate(f: Finding): { blocked?: string; changed?: string; fails?: string } {
  if (f.ruleId === "ai.claude-desktop.vm-bundles") return { blocked: "Claude is running" };
  // The leftover worktree folder became a real worktree between the plan and the clean.
  if (f.ruleId === "dev.git-worktrees" && f.action === "trash") {
    return { changed: "its action changed from trash to git-worktree-remove" };
  }
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
  const storageHandlers = new Set<(p: StorageProgress) => void>();
  let storage: StorageBreakdown | null = null;
  const cleaned = new Set<string>();
  const recordedScans: FreeSpacePoint[] = [];
  let lastReport: ScanReport | null = null;
  let downloadsReport: ScanReport | null = null;
  let quickLookPath: string | null = null;
  let volume: VolumeInfo | null = null;
  let fullDiskAccess = false;
  let agentInstalled = false;

  function agentStatus(): AgentStatus {
    const dataDir = "/Users/you/Library/Application Support/clean-my-mac";
    return {
      installed: agentInstalled,
      loaded: agentInstalled,
      plistPath: "/Users/you/Library/LaunchAgents/com.zak-roz.clean-my-mac.scan.plist",
      program: agentInstalled ? "/Users/you/.cargo/bin/cmm" : null,
      programOk: agentInstalled,
      hour: agentInstalled ? 10 : null,
      minute: agentInstalled ? 0 : null,
      logPath: `${dataDir}/agent.log`,
      lastExitCode: null,
      lastLogLine: null,
      resolvedCmm: "/Users/you/.cargo/bin/cmm",
    };
  }

  async function currentVolume(): Promise<VolumeInfo> {
    if (!volume) volume = { ...(await loadFixture()).volume };
    return { ...volume, trashBytes: fullDiskAccess ? (volume.trashBytes ?? 2.86 * GB) : null };
  }

  async function plan(req: CleanRequest): Promise<CleanPlan> {
    const report = lastReport ?? (await loadFixture());
    const pool = [...report.findings, ...(downloadsReport?.findings ?? [])];
    const selected = pool.filter((f) => req.findingIds.includes(f.id));
    const warnings: string[] = [];
    const items: Finding[] = [];
    for (const f of selected) {
      const { blocked } = revalidate(f);
      if (blocked) warnings.push(`Skipped ${f.label}: no longer matches the scan (now blocked: ${blocked})`);
      else items.push(f);
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
      if (opts.ruleIds?.length === 1 && opts.ruleIds[0] === DOWNLOADS_RULE) {
        // The backend keeps a Downloads scan apart: no progress events, not the last report.
        await sleep(700);
        const now = new Date().toISOString();
        downloadsReport = {
          ...(await loadFixture()),
          id: now,
          startedAt: now,
          durationMs: 640,
          findings: mockDownloads().filter((f) => !cleaned.has(f.id)),
          errors: [],
        };
        return downloadsReport;
      }
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
        const { changed, fails } = revalidate(f);
        let result: CleanItemResult;
        if (covered.has(f.id)) {
          result = { findingId: f.id, outcome: "done", bytes: 0, message: "Covered by parent" };
        } else if (changed) {
          result = {
            findingId: f.id,
            outcome: "skipped",
            bytes: 0,
            message: `skipped: no longer matches the scan (${changed})`,
          };
        } else if (fails && !req.dryRun) {
          result = { findingId: f.id, outcome: "failed", bytes: 0, message: `docker system prune failed: ${fails}` };
        } else {
          result = { findingId: f.id, outcome: "done", bytes: f.bytes, message: null };
          if (!req.dryRun) {
            if (req.mode === "delete" || isPermanentAction(f)) freedBytes += f.bytes;
            else movedToTrashBytes += f.bytes;
          }
        }
        results.push(result);
        for (const h of itemHandlers) h(result);
      }
      if (!req.dryRun) {
        for (const r of results) if (r.outcome === "done") cleaned.add(r.findingId);
        if (lastReport) lastReport = { ...lastReport, findings: lastReport.findings.filter((f) => !cleaned.has(f.id)) };
        if (downloadsReport) {
          downloadsReport = {
            ...downloadsReport,
            findings: downloadsReport.findings.filter((f) => !cleaned.has(f.id)),
          };
        }
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
    revealTrash: async () => console.info("[mock] show Trash in Finder"),
    openFullDiskAccessSettings: async () => {
      console.info("[mock] open Full Disk Access settings (granting it)");
      fullDiskAccess = true;
    },
    monitorSettings: async () => ({
      lowSpaceThresholdBytes: 50 * GB,
      growthAlertBytes: 5 * GB,
      growthAlertDays: 7,
      notifications: true,
      configPath: "/Users/you/Library/Application Support/clean-my-mac/config.toml",
    }),
    agentStatus: async () => agentStatus(),
    agentInstall: async () => {
      await sleep(400);
      agentInstalled = true;
      return agentStatus();
    },
    agentUninstall: async () => {
      await sleep(300);
      agentInstalled = false;
      return agentStatus();
    },
    thumbnail: async (path, size) => {
      await sleep(80 + (hash(path) % 220));
      return new Blob([placeholderThumbnail(path, size)], { type: "image/svg+xml" });
    },
    quickLook: async (path) => {
      quickLookPath = quickLookPath === path ? null : path;
      console.info(quickLookPath ? "[mock] Quick Look:" : "[mock] Quick Look closed:", path);
      return quickLookPath !== null;
    },
    // No menu bar in the browser.
    onExternalScan: async () => () => {},
    onNavigate: async () => () => {},
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
    storageCached: async () => {
      await sleep(80);
      return storage && reannotate(storage, lastReport?.findings ?? [], lastReport?.id ?? null);
    },
    measureStorage: async () => {
      const vol = await currentVolume();
      const expectedBytes = vol.totalBytes - vol.freeBytes - 41 * GB;
      const emit = (p: StorageProgress) => {
        for (const h of storageHandlers) h(p);
      };
      emit({ phase: "volumes", walkedBytes: 0, expectedBytes: 0 });
      await sleep(250);
      const steps = 10;
      for (let i = 0; i <= steps; i += 1) {
        emit({ phase: "walking", walkedBytes: Math.round((expectedBytes * 0.93 * i) / steps), expectedBytes });
        await sleep(220);
      }
      emit({ phase: "done", walkedBytes: Math.round(expectedBytes * 0.93), expectedBytes });
      storage = mockBreakdown(vol, [], new Date().toISOString());
      return reannotate(storage, lastReport?.findings ?? [], lastReport?.id ?? null);
    },
    storageChildren: async (path) => {
      await sleep(300);
      return mockChildren(storage, path, lastReport?.findings ?? []);
    },
    onStorageProgress: async (handler) => {
      const wrapped = (p: StorageProgress) => handler(p);
      storageHandlers.add(wrapped);
      const stop: UnlistenFn = () => storageHandlers.delete(wrapped);
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

const GROWTH: { kind: GrowthKind; key: string; label: string; now: number; perDay: number }[] = [
  {
    kind: "rule",
    key: "downloads.versioned-siblings",
    label: "Dated copies in Downloads",
    now: 114.29 * GB,
    perDay: 1.27 * GB,
  },
  {
    kind: "rule",
    key: "ai.claude-desktop.vm-bundles",
    label: "Claude Desktop VM bundle",
    now: 15.67 * GB,
    perDay: 0.25 * GB,
  },
  { kind: "rule", key: "dev.git-worktrees", label: "Git worktrees", now: 6.62 * GB, perDay: 0.2 * GB },
  { kind: "rule", key: "containers.docker", label: "Docker reclaimable data", now: 11.54 * GB, perDay: 0.16 * GB },
  { kind: "rule", key: "cache.npm", label: "npm cache", now: 7.3 * GB, perDay: 0.08 * GB },
  { kind: "rule", key: "cache.yarn", label: "Yarn cache", now: 4.83 * GB, perDay: -0.04 * GB },
  { kind: "rule", key: "xcode.derived-data", label: "Xcode DerivedData", now: 0, perDay: -0.27 * GB },
  {
    kind: "item",
    key: "downloads.versioned-siblings:/Users/you/Downloads/backups/mongo_17_05_2025",
    label: "mongo_17_05_2025",
    now: 31.47 * GB,
    perDay: 1.05 * GB,
  },
  {
    kind: "item",
    key: "ai.claude-desktop.vm-bundles:/Users/you/Library/Application Support/ai-profiles/profiles/3f9c2d41-7b6e-4c1a-9e85-2d0f6a7b1c93/gui-data/vm_bundles",
    label: "vm_bundles (profile 3f9c2d41)",
    now: 8.71 * GB,
    perDay: 0.27 * GB,
  },
  {
    kind: "item",
    key: "dev.git-worktrees:/Users/you/Developer/example-app/.claude/worktrees/bugfix-123",
    label: "bugfix-123",
    now: 2.19 * GB,
    perDay: 0.1 * GB,
  },
  {
    kind: "item",
    key: "dev.git-worktrees:/Users/you/Developer/example-app/.claude/worktrees/feature-login",
    label: "feature-login",
    now: 2.68 * GB,
    perDay: 0.06 * GB,
  },
  { kind: "item", key: "cache.npm:/Users/you/.npm/_cacache", label: "npm cache", now: 7.3 * GB, perDay: 0.08 * GB },
  {
    kind: "item",
    key: "cache.yarn:/Users/you/Library/Caches/Yarn",
    label: "Yarn cache",
    now: 4.83 * GB,
    perDay: -0.04 * GB,
  },
];

function syntheticGrowth(days: number): GrowthItem[] {
  return GROWTH.map(({ kind, key, label, now, perDay }) => {
    const bytesThen = Math.max(0, Math.round(now - perDay * days));
    const bytesNow = Math.round(now);
    return { kind, key, label, bytesThen, bytesNow, delta: bytesNow - bytesThen };
  })
    .filter((g) => g.delta !== 0)
    .sort((a, b) => b.delta - a.delta);
}

// ---------------------------------------------------------------------------------------------
// Downloads tab: invented entries of a ~/Downloads folder (dates relative to now)

const MB = 1e6;

interface MockEntry {
  name: string;
  bytes: number;
  added: number;
  /** Days since last opened; `null` = never opened; `undefined` = not indexed (unknown). */
  opened?: number | null;
  dir?: boolean;
  files?: number;
  from?: string;
  type?: string;
  blocked?: string;
}

const MOCK_DOWNLOADS: MockEntry[] = [
  { name: "backups", bytes: 151.2 * GB, added: 410, opened: 160, dir: true, files: 1290, type: "public.folder" },
  {
    name: "vacation-2025.mov",
    bytes: 1.84 * GB,
    added: 402,
    opened: null,
    from: "https://share.example.net/v/9f2",
    type: "com.apple.quicktime-movie",
  },
  {
    name: "Ubuntu-24.04-desktop-arm64.iso",
    bytes: 3.21 * GB,
    added: 520,
    opened: null,
    from: "https://releases.example.org/24.04/ubuntu.iso",
    type: "public.iso-image",
  },
  {
    name: "Figma-126.4.dmg",
    bytes: 512 * MB,
    added: 45,
    opened: null,
    from: "https://desktop.example.com/mac/Figma.dmg",
    type: "com.apple.disk-image-udif",
  },
  {
    name: "design-assets.zip",
    bytes: 340 * MB,
    added: 61,
    opened: 60,
    from: "https://files.example.com/d/assets.zip",
    type: "public.zip-archive",
  },
  { name: "design-assets", bytes: 612 * MB, added: 60, opened: 12, dir: true, files: 214, type: "public.folder" },
  {
    name: "node-v22.11.0.pkg",
    bytes: 85.4 * MB,
    added: 300,
    opened: null,
    from: "https://nodejs.example.org/dist/node.pkg",
    type: "com.apple.installer-package-archive",
  },
  {
    name: "Tool.app",
    bytes: 210 * MB,
    added: 20,
    opened: 19,
    dir: true,
    files: 812,
    type: "com.apple.application-bundle",
  },
  {
    name: "podcast-episode-142.mp3",
    bytes: 64.2 * MB,
    added: 15,
    opened: null,
    from: "https://cdn.example.fm/ep142.mp3",
    type: "public.mp3",
  },
  {
    name: "dataset-2026-q3.csv",
    bytes: 45.8 * MB,
    added: 33,
    opened: 33,
    from: "https://data.example.com/export",
    type: "public.comma-separated-values-text",
  },
  {
    name: "Quarterly-Report-2025.pdf",
    bytes: 2.4 * MB,
    added: 120,
    opened: 98,
    from: "https://drive.example.com/file/q",
    type: "com.adobe.pdf",
  },
  { name: "IMG_4821.HEIC", bytes: 3.1 * MB, added: 200, opened: 199, type: "public.heic" },
  { name: "Screenshot 2026-10-02 at 14.31.07.png", bytes: 1.2 * MB, added: 54, opened: null, type: "public.png" },
  {
    name: "invoice-0042.pdf",
    bytes: 0.18 * MB,
    added: 3,
    opened: 2,
    from: "https://billing.example.com/inv/42",
    type: "com.adobe.pdf",
  },
  { name: "fonts", bytes: 12.6 * MB, added: 95, dir: true, files: 48, type: "public.folder" },
  { name: "notes.md", bytes: 4096, added: 1, opened: null, type: "net.daringfireball.markdown" },
  {
    name: "big-export.zip.crdownload",
    bytes: 120 * MB,
    added: 0,
    opened: null,
    from: "https://files.example.com/big-export.zip",
    type: "public.data",
    blocked: "Download not finished: let it complete or cancel it in the browser first",
  },
];

function mockDownloads(): Finding[] {
  const now = Date.now();
  const ago = (days: number) => new Date(now - days * DAY_MS - 3_600_000).toISOString();
  return MOCK_DOWNLOADS.map((e) => {
    const path = `/Users/you/Downloads/${e.name}`;
    const indexed = e.opened !== undefined;
    const lastOpened = typeof e.opened === "number" ? ago(e.opened) : null;
    const used = Math.min(e.added, typeof e.opened === "number" ? e.opened : e.added);
    const addedText = e.added === 0 ? "today" : e.added === 1 ? "1 day ago" : `${e.added} days ago`;
    const openedText =
      typeof e.opened === "number"
        ? ` · opened ${e.opened === 0 ? "today" : e.opened === 1 ? "1 day ago" : `${e.opened} days ago`}`
        : indexed
          ? " · never opened"
          : "";
    return {
      id: `${DOWNLOADS_RULE}:${path}`,
      ruleId: DOWNLOADS_RULE,
      category: "downloads",
      risk: "manual",
      label: e.name,
      path,
      bytes: Math.round(e.bytes),
      logicalBytes: Math.round(e.bytes),
      files: e.files ?? 1,
      isDir: e.dir ?? false,
      lastUsed: ago(used),
      reason: `Added ${addedText}${openedText}`,
      action: "trash",
      command: null,
      selectedByDefault: false,
      blockers: e.blocked ? [e.blocked] : [],
      group: null,
      meta: {
        dateAdded: ago(e.added),
        lastOpened,
        indexed,
        created: ago(e.added),
        modified: ago(e.added + 2),
        contentType: e.type ?? null,
        whereFrom: e.from ?? null,
      },
    } satisfies Finding;
  });
}

function hash(text: string): number {
  let h = 0;
  for (const ch of text) h = (h * 31 + (ch.codePointAt(0) ?? 0)) >>> 0;
  return h;
}

/** A placeholder "thumbnail": a tinted card with the extension, or a little landscape for images. */
function placeholderThumbnail(path: string, size: number): string {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const hue = hash(name) % 360;
  const s = size * 2;
  const fake = { label: name, isDir: !extensionOf(name), meta: null } as unknown as Finding;
  if (entryKind(fake) === "image" || entryKind(fake) === "video") {
    return `<svg xmlns="http://www.w3.org/2000/svg" width="${s}" height="${s}" viewBox="0 0 64 64">
<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="hsl(${hue} 70% 72%)"/><stop offset="1" stop-color="hsl(${(hue + 40) % 360} 60% 52%)"/></linearGradient></defs>
<rect width="64" height="64" rx="6" fill="url(#g)"/><circle cx="46" cy="18" r="7" fill="hsl(48 95% 80%)"/>
<path d="M0 50 L20 30 L34 44 L44 36 L64 54 L64 64 L0 64 Z" fill="hsl(${(hue + 120) % 360} 35% 34%)"/></svg>`;
  }
  const ext = (extensionOf(name) || "•").toUpperCase().slice(0, 4);
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${s}" height="${s}" viewBox="0 0 64 64">
<path d="M14 4h26l12 12v42a2 2 0 0 1-2 2H14a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Z" fill="hsl(${hue} 30% 96%)" stroke="hsl(${hue} 20% 70%)"/>
<path d="M40 4v12h12" fill="none" stroke="hsl(${hue} 20% 70%)"/>
<rect x="12" y="36" width="40" height="14" rx="2" fill="hsl(${hue} 55% 48%)"/>
<text x="32" y="46.5" text-anchor="middle" font-family="-apple-system, sans-serif" font-size="9" font-weight="700" fill="#fff">${ext}</text></svg>`;
}
