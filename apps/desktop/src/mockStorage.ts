// Invented storage breakdown for mock mode (`VITE_MOCK=1`): a /Users/you machine whose parts add
// up to the fixture volume's used space and contain the fixture findings' paths, so the
// found-by-scan marks line up with the Overview. Found marks follow the core's rule
// (`storage::annotate`): each de-duplicated finding goes to the deepest node containing it, and a
// node whose path is a finding's path is wholly found.

import type { Finding } from "./bindings/Finding";
import type { StorageBreakdown } from "./bindings/StorageBreakdown";
import type { StorageNode } from "./bindings/StorageNode";
import type { VolumeInfo } from "./bindings/VolumeInfo";
import { dedupedTotal, isInside } from "./lib/findings";

const GB = 1e9;
const MB = 1e6;
const DAY_MS = 86_400_000;
const HOME = "/Users/you";

interface Spec {
  name: string;
  /** Allocated size; whatever the children leave is summed into "N smaller items". */
  gb: number;
  file?: boolean;
  /** Days since the newest change inside. */
  days?: number;
  /** Direct entries (defaults to the listed children plus a dozen small ones). */
  entries?: number;
  unreadable?: "protected" | "denied";
  children?: Spec[];
  label?: string;
}

const ago = (days: number) => new Date(Date.now() - days * DAY_MS).toISOString();

function node(parent: string, spec: Spec): StorageNode {
  const path = parent === "" ? spec.name : `${parent}/${spec.name}`;
  const bytes = Math.round(spec.gb * GB);
  const children = (spec.children ?? []).map((c) => node(path, c));
  const listed = children.reduce((sum, c) => sum + c.bytes, 0);
  const restBytes = bytes - listed;
  const smallEntries = spec.entries !== undefined ? spec.entries - children.length : restBytes > 0 ? 12 : 0;
  if (!spec.file && children.length > 0 && smallEntries > 0 && restBytes > 0) {
    children.push(rest(path, `${path}/@rest`, smallEntries, restBytes, spec.days ?? 30));
  }
  return {
    id: path,
    label: spec.label ?? spec.name,
    path,
    kind: spec.file ? "file" : "folder",
    bytes,
    files: spec.file ? 1 : Math.max(1, Math.round(bytes / (2 * MB))),
    entries: spec.file ? 0 : (spec.entries ?? children.length + (restBytes > 0 ? 12 : 0)),
    modified: ago(spec.days ?? 30),
    unreadable: spec.unreadable ?? null,
    foundBytes: 0,
    note: null,
    children,
  };
}

function rest(path: string, id: string, entries: number, bytes: number, days: number): StorageNode {
  return {
    id,
    label: entries === 1 ? "1 smaller item" : `${entries} smaller items`,
    path,
    kind: "rest",
    bytes,
    files: Math.max(entries, Math.round(bytes / (4 * MB))),
    entries,
    modified: ago(days),
    unreadable: null,
    foundBytes: 0,
    note: null,
    children: [],
  };
}

function group(id: string, label: string, note: string, children: StorageNode[]): StorageNode {
  const sorted = [...children].sort(
    (a, b) => Number(a.kind === "rest") - Number(b.kind === "rest") || b.bytes - a.bytes,
  );
  const flags = sorted.map((c) => c.unreadable);
  return {
    id,
    label,
    path: null,
    kind: "group",
    bytes: sorted.reduce((sum, c) => sum + c.bytes, 0),
    files: sorted.reduce((sum, c) => sum + c.files, 0),
    entries: sorted.length,
    modified: null,
    unreadable: flags.includes("protected") ? "protected" : flags.includes("denied") ? "denied" : null,
    foundBytes: 0,
    note,
    children: sorted,
  };
}

function volume(on: string, label: string, gb: number, note: string | null): StorageNode {
  return {
    id: `@macos:${on}`,
    label,
    path: on,
    kind: "volume",
    bytes: Math.round(gb * GB),
    files: 0,
    entries: 0,
    modified: null,
    unreadable: null,
    foundBytes: 0,
    note,
    children: [],
  };
}

const home = (spec: Spec): StorageNode => node(HOME, { ...spec, label: `~/${spec.name}` });

function topLevel(swapBytes: number | null): StorageNode[] {
  const swap = swapBytes === null ? "" : ` Swap in use: ${(swapBytes / GB).toFixed(1)} GB.`;
  return [
    home({
      name: "Downloads",
      gb: 131.4,
      days: 1,
      entries: 217,
      children: [
        {
          name: "backups",
          gb: 116.0,
          days: 192,
          children: [
            { name: "mongo_28_01_2025", gb: 44.617, days: 301, entries: 38 },
            { name: "mongo_05_03_2025", gb: 38.204, days: 265, entries: 38 },
            { name: "mongo_17_05_2025", gb: 31.472, days: 192, entries: 41 },
          ],
          entries: 9,
        },
        { name: "Xcode_16.2.xip", gb: 3.1, file: true, days: 88 },
        { name: "family-photos-2025.zip", gb: 2.4, file: true, days: 40 },
      ],
    }),
    home({
      name: "Library",
      gb: 98.6,
      days: 0,
      unreadable: "protected",
      entries: 74,
      children: [
        {
          name: "Application Support",
          gb: 61.3,
          days: 0,
          entries: 96,
          children: [
            {
              name: "ai-profiles",
              gb: 19.4,
              days: 0,
              children: [
                { name: "migration-backup-1789380000000", gb: 9.867, days: 70, entries: 6 },
                {
                  name: "profiles",
                  gb: 9.3,
                  days: 0,
                  children: [
                    {
                      name: "3f9c2d41-7b6e-4c1a-9e85-2d0f6a7b1c93",
                      gb: 9.25,
                      days: 0,
                      children: [
                        {
                          name: "gui-data",
                          gb: 9.1,
                          days: 0,
                          children: [{ name: "vm_bundles", gb: 8.712, days: 3, entries: 2 }],
                        },
                      ],
                    },
                  ],
                },
              ],
            },
            {
              name: "com.apple.wallpaper",
              gb: 14.6,
              days: 12,
              children: [
                {
                  name: "aerials",
                  gb: 14.5,
                  days: 12,
                  children: [{ name: "videos", gb: 14.356, days: 12, entries: 22 }],
                },
              ],
            },
            { name: "Claude", gb: 7.4, days: 0, children: [{ name: "vm_bundles", gb: 6.955, days: 5, entries: 2 }] },
            { name: "Code", gb: 3.1, days: 0, entries: 31 },
            { name: "Google", gb: 2.6, days: 0, entries: 4 },
            { name: "Slack", gb: 1.4, days: 1, entries: 18 },
          ],
        },
        {
          name: "Developer",
          gb: 13.8,
          days: 2,
          children: [
            { name: "CoreSimulator", gb: 8.9, days: 9, entries: 5 },
            { name: "Xcode", gb: 4.6, days: 2, children: [{ name: "DerivedData", gb: 3.9, days: 2, entries: 14 }] },
          ],
        },
        {
          name: "Caches",
          gb: 11.2,
          days: 0,
          entries: 143,
          children: [
            { name: "Yarn", gb: 4.826, days: 6, entries: 1 },
            { name: "com.spotify.client", gb: 2.1, days: 0, entries: 3 },
            { name: "Google", gb: 1.6, days: 0, entries: 2 },
          ],
        },
        {
          name: "Containers",
          gb: 9.4,
          days: 0,
          unreadable: "protected",
          children: [{ name: "com.docker.docker", gb: 9.1, days: 0, entries: 1 }],
        },
        { name: "Mail", gb: 0.06, days: 0, unreadable: "protected", entries: 0 },
      ],
    }),
    home({
      name: "Developer",
      gb: 38.7,
      days: 0,
      entries: 23,
      children: [
        {
          name: "example-app",
          gb: 15.6,
          days: 0,
          children: [
            {
              name: ".claude",
              gb: 6.9,
              days: 0,
              children: [
                {
                  name: "worktrees",
                  gb: 6.62,
                  days: 0,
                  children: [
                    { name: "feature-login", gb: 2.683, days: 4, entries: 19 },
                    { name: "bugfix-123", gb: 2.19, days: 9, entries: 19 },
                    { name: "agent-task", gb: 1.746, days: 23, entries: 17 },
                  ],
                },
              ],
            },
            { name: "node_modules", gb: 1.9, days: 0, entries: 812 },
          ],
        },
        {
          name: "ml-experiments",
          gb: 12.4,
          days: 47,
          children: [
            { name: "data", gb: 9.8, days: 47, entries: 64 },
            { name: ".venv", gb: 2.1, days: 47, entries: 6 },
          ],
        },
        {
          name: "old-site",
          gb: 1.6,
          days: 203,
          children: [{ name: "node_modules", gb: 1.262, days: 203, entries: 640 }],
        },
      ],
    }),
    home({
      name: "Documents",
      gb: 22.4,
      days: 3,
      children: [
        { name: "Videos", gb: 14.2, days: 120, entries: 17 },
        { name: "Scans", gb: 3.6, days: 3, entries: 310 },
      ],
    }),
    home({ name: "Movies", gb: 12.1, days: 15, unreadable: "protected", entries: 9 }),
    home({
      name: "Pictures",
      gb: 6.3,
      days: 2,
      children: [{ name: "Photos Library.photoslibrary", gb: 6.2, days: 2, entries: 11 }],
    }),
    home({ name: "Music", gb: 1.2, days: 60, entries: 3 }),
    group(
      "@home-other",
      "Other in home",
      "Hidden folders in your home (~/.npm, ~/.cargo, ~/.docker, the Trash, …), loose files and small folders.",
      [
        node(HOME, {
          name: ".npm",
          label: "~/.npm",
          gb: 7.6,
          days: 1,
          children: [{ name: "_cacache", gb: 7.301, days: 1, entries: 3 }],
        }),
        node(HOME, { name: ".cargo", label: "~/.cargo", gb: 3.1, days: 4, entries: 6 }),
        node(HOME, { name: ".rustup", label: "~/.rustup", gb: 2.6, days: 30, entries: 5 }),
        node(HOME, { name: ".cache", label: "~/.cache", gb: 2.2, days: 1, entries: 14 }),
        node(HOME, { name: ".docker", label: "~/.docker", gb: 1.1, days: 0, entries: 8 }),
        node(HOME, { name: ".Trash", label: "~/.Trash", gb: 0, days: 0, unreadable: "protected", entries: 0 }),
        rest(HOME, "@home-other/@rest", 41, Math.round(3.1 * GB), 0),
      ],
    ),
    node("", {
      name: "/Applications",
      gb: 28.3,
      days: 6,
      entries: 52,
      children: [
        { name: "Xcode.app", gb: 11.8, days: 31, entries: 1 },
        { name: "Docker.app", gb: 2.0, days: 6, entries: 1 },
        { name: "Google Chrome.app", gb: 1.6, days: 2, entries: 1 },
      ],
    }),
    group(
      "@system-data",
      "System data",
      "The rest of the Data volume: /Library (shared app data, simulators), /private (logs, temporary files, system databases), /opt (Homebrew) and macOS indexes.",
      [
        node("", { name: "/System/Volumes/Data/System", gb: 6.8, days: 9, unreadable: "protected", entries: 1 }),
        node("", { name: "/private", gb: 7.4, days: 0, unreadable: "denied", entries: 7 }),
        node("", { name: "/opt", gb: 4.6, days: 6, children: [{ name: "homebrew", gb: 4.6, days: 6, entries: 18 }] }),
        node("", { name: "/Library", gb: 2.6, days: 1, unreadable: "protected", entries: 48 }),
        node("", { name: "/System/Volumes/Data/.Spotlight-V100", gb: 0, days: 0, unreadable: "protected", entries: 0 }),
        rest("/System/Volumes/Data", "@system-data/@rest", 9, Math.round(0.2 * GB), 30),
      ],
    ),
    group(
      "@users",
      "Other users & Shared",
      "Other accounts' home folders and /Users/Shared. Other accounts can only be read by an administrator.",
      [node("", { name: "/Users/Shared", gb: 0.3, days: 90, entries: 4 })],
    ),
    group(
      "@macos",
      "macOS",
      "The sealed, read-only macOS system, its boot and recovery volumes, swap and the sleep image. Sized per APFS volume; managed by macOS.",
      [
        volume("/", "System", 13.8, "The sealed, read-only macOS system volume."),
        volume(
          "/System/Volumes/VM",
          "Swap & sleep image",
          15.9,
          `Swap files and the sleep image (VM volume).${swap} A restart shrinks it.`,
        ),
        volume("/System/Volumes/Preboot", "Preboot", 9.7, "Boot files and system extensions for this macOS."),
        volume("/System/Volumes/Recovery", "Recovery", 1.5, "macOS Recovery."),
        volume("/System/Volumes/Update", "Update", 0.1, "Staged macOS updates."),
        {
          ...volume(
            "@container",
            "Other container space",
            0.2,
            "APFS volumes that are not mounted (such as Recovery) and container metadata.",
          ),
          id: "@macos/container",
          path: null,
        },
      ],
    ),
  ];
}

/** The deepest node containing `target` (looking through groups; never volumes): [id, exact]. */
function locate(nodes: StorageNode[], target: string): [string, boolean] | null {
  let best: StorageNode | null = null;
  const consider = (list: StorageNode[]) => {
    for (const n of list) {
      if (n.kind === "volume" || n.kind === "unmeasured") continue;
      if (n.path === null) {
        consider(n.children);
        continue;
      }
      if (!(target === n.path || isInside(target, n.path))) continue;
      const better =
        best === null ||
        n.path.length > (best.path ?? "").length ||
        (n.path.length === (best.path ?? "").length && best.kind === "rest" && n.kind !== "rest");
      if (better) best = n;
    }
  };
  consider(nodes);
  const found = best as StorageNode | null;
  if (found === null) return null;
  if (found.kind === "rest") return [found.id, false];
  if (found.path === target) return [found.id, true];
  return locate(found.children, target) ?? [found.id, false];
}

/** Sets `foundBytes` like `storage::annotate`; returns the located part. */
export function annotateMock(nodes: StorageNode[], findings: Finding[]): number {
  const assigned = new Map<string, number>();
  const covered = new Set<string>();
  const nested = new Set(dedupedTotal(findings).covered);
  const items = findings.filter((f) => f.path !== null && !nested.has(f));
  for (const f of items) {
    const hit = locate(nodes, f.path as string);
    if (!hit) continue;
    if (hit[1]) covered.add(hit[0]);
    else assigned.set(hit[0], (assigned.get(hit[0]) ?? 0) + f.bytes);
  }
  const cover = (n: StorageNode) => {
    n.foundBytes = n.bytes;
    n.children.forEach(cover);
  };
  const finalize = (n: StorageNode): number => {
    if (covered.has(n.id)) {
      cover(n);
      return n.bytes;
    }
    const below = n.children.reduce((sum, c) => sum + finalize(c), 0);
    n.foundBytes = Math.min(n.bytes, below + (assigned.get(n.id) ?? 0));
    return n.foundBytes;
  };
  return nodes.reduce((sum, n) => sum + finalize(n), 0);
}

/** The whole mock breakdown for `volume`, with found marks for `findings`. */
export function mockBreakdown(volume: VolumeInfo, findings: Finding[], measuredAt: string): StorageBreakdown {
  const used = volume.totalBytes - volume.freeBytes;
  const nodes = topLevel(volume.swapUsedBytes);
  const volumesBytes = nodes.find((n) => n.id === "@macos")?.bytes ?? 0;
  const walked = nodes.reduce((sum, n) => sum + n.bytes, 0) - volumesBytes;
  const unmeasured = Math.max(0, used - walked - volumesBytes);
  nodes.push({
    id: "@unmeasured",
    label: "Not measured",
    path: null,
    kind: "unmeasured",
    bytes: unmeasured,
    files: 0,
    entries: 0,
    modified: null,
    unreadable: null,
    foundBytes: 0,
    note: "Used space no readable file accounts for: 2 local Time Machine snapshots, folders that need Full Disk Access, folders only macOS or an administrator can read, file-system metadata.",
    children: [],
  });
  nodes.sort((a, b) => b.bytes - a.bytes);
  const located = annotateMock(nodes, findings);
  const found = Math.min(used, dedupedTotal(findings).bytes);
  return {
    measuredAt,
    durationMs: 24_300,
    scanId: null,
    totalBytes: volume.totalBytes,
    freeBytes: volume.freeBytes,
    usedBytes: used,
    walkedBytes: walked,
    volumesBytes,
    unmeasuredBytes: unmeasured,
    overcountBytes: 0,
    fullDiskAccess: false,
    localSnapshots: ["com.apple.TimeMachine.2026-11-24-091500.local", "com.apple.TimeMachine.2026-11-25-091500.local"],
    foundBytes: found,
    foundLocatedBytes: Math.min(found, located),
    nodes,
    errors: [
      { path: `${HOME}/Library/Mail`, message: "operation not permitted (needs Full Disk Access?)" },
      { path: "/private/var/db/diagnostics", message: "permission denied" },
    ],
  };
}

/** `b` with found marks for `findings` (what `storage_cached` does for the current report). */
export function reannotate(b: StorageBreakdown, findings: Finding[], scanId: string | null): StorageBreakdown {
  const copy = structuredClone(b);
  const located = annotateMock(copy.nodes, findings);
  const found = Math.min(copy.usedBytes, dedupedTotal(findings).bytes);
  return { ...copy, scanId, foundBytes: found, foundLocatedBytes: Math.min(found, located) };
}

function findNode(nodes: StorageNode[], path: string): StorageNode | null {
  for (const n of nodes) {
    if (n.kind !== "rest" && n.path === path) return n;
    const below = findNode(n.children, path);
    if (below) return below;
  }
  return null;
}

/** A full listing of one folder: its kept children plus invented entries for the smaller ones. */
export function mockChildren(b: StorageBreakdown | null, path: string, findings: Finding[]): StorageNode {
  const folder = (b && findNode(b.nodes, path)) ?? node("", { name: path, gb: 0.5, entries: 6 });
  const kept = folder.children.filter((c) => c.kind !== "rest").map((c) => ({ ...c, children: [] }));
  const restBytes = folder.bytes - kept.reduce((sum, c) => sum + c.bytes, 0);
  const count = Math.max(0, Math.min(40, folder.entries - kept.length));
  const ext = /mongo_/.test(path) ? "bson" : "dat";
  const invented: StorageNode[] = [];
  let weightSum = 0;
  for (let i = 0; i < count; i += 1) weightSum += 1 / (i + 1);
  for (let i = 0; i < count; i += 1) {
    const bytes = Math.round((restBytes * (1 / (i + 1))) / weightSum);
    invented.push({
      ...node(path, {
        name: `${/mongo_/.test(path) ? "collection" : "item"}-${String(i + 1).padStart(2, "0")}.${ext}`,
        gb: bytes / GB,
        file: true,
        days: 10 + i,
      }),
    });
  }
  const listed: StorageNode = { ...folder, children: [...kept, ...invented].sort((x, y) => y.bytes - x.bytes) };
  annotateMock([listed], findings);
  return listed;
}
