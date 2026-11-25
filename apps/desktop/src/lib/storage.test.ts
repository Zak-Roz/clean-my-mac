import { describe, expect, it } from "vitest";
import type { Finding } from "../bindings/Finding";
import type { ScanReport } from "../bindings/ScanReport";
import type { StorageBreakdown } from "../bindings/StorageBreakdown";
import type { StorageNode } from "../bindings/StorageNode";
import {
  accounting,
  barShare,
  canOpen,
  childrenOf,
  findingsIn,
  findingsQuery,
  hasProtected,
  isFolderRest,
  isStale,
  type Loaded,
  needsListing,
  progressFraction,
  ROOT_ID,
  resolveTrail,
  rootNode,
  trail,
} from "./storage";

function node(over: Partial<StorageNode> & Pick<StorageNode, "id">): StorageNode {
  return {
    label: over.id,
    path: over.id.startsWith("@") ? null : over.id,
    kind: "folder",
    bytes: 0,
    files: 0,
    entries: 0,
    modified: null,
    unreadable: null,
    foundBytes: 0,
    note: null,
    children: [],
    ...over,
  };
}

const caches = node({
  id: "/Users/you/Library/Caches",
  bytes: 12e9,
  entries: 40,
  foundBytes: 4.8e9,
  children: [
    node({ id: "/Users/you/Library/Caches/Yarn", bytes: 4.8e9, entries: 3, foundBytes: 4.8e9 }),
    node({
      id: "/Users/you/Library/Caches/@rest",
      kind: "rest",
      path: "/Users/you/Library/Caches",
      bytes: 7.2e9,
      entries: 39,
    }),
  ],
});
const library = node({
  id: "/Users/you/Library",
  label: "~/Library",
  bytes: 20e9,
  entries: 60,
  foundBytes: 4.8e9,
  children: [caches],
});
const homeOther = node({
  id: "@home-other",
  kind: "group",
  bytes: 3e9,
  children: [
    node({ id: "/Users/you/.npm", bytes: 2.5e9, entries: 2 }),
    node({ id: "@home-other/@rest", kind: "rest", path: "/Users/you", bytes: 0.5e9, entries: 12 }),
  ],
});
const macos = node({
  id: "@macos",
  kind: "group",
  bytes: 30e9,
  children: [node({ id: "@macos:/", kind: "volume", path: "/", bytes: 30e9 })],
});
const unmeasured = node({ id: "@unmeasured", kind: "unmeasured", bytes: 7e9 });

function breakdown(over: Partial<StorageBreakdown> = {}): StorageBreakdown {
  return {
    measuredAt: "2026-11-25T10:00:00Z",
    durationMs: 25_000,
    scanId: null,
    totalBytes: 100e9,
    freeBytes: 40e9,
    usedBytes: 60e9,
    walkedBytes: 23e9,
    volumesBytes: 30e9,
    unmeasuredBytes: 7e9,
    overcountBytes: 0,
    fullDiskAccess: false,
    localSnapshots: [],
    foundBytes: 6e9,
    foundLocatedBytes: 4.8e9,
    nodes: [macos, library, unmeasured, homeOther],
    errors: [],
    ...over,
  };
}

const none: Loaded = new Map();

function finding(path: string | null): Finding {
  return { id: `rule:${path}`, path, label: path ?? "docker" } as Finding;
}

describe("rootNode", () => {
  it("puts the top level under one node that sums to the used space", () => {
    const root = rootNode(breakdown());
    expect(root.id).toBe(ROOT_ID);
    expect(root.bytes).toBe(60e9);
    expect(root.foundBytes).toBe(4.8e9);
    expect(root.children.map((c) => c.id)).toEqual(["@macos", "/Users/you/Library", "@unmeasured", "@home-other"]);
  });
});

describe("drilling", () => {
  it("opens groups, folders with entries and a folder's smaller items, nothing else", () => {
    expect(canOpen(macos)).toBe(true);
    expect(canOpen(library)).toBe(true);
    expect(canOpen(caches.children[1] as StorageNode)).toBe(true);
    expect(canOpen(homeOther.children[1] as StorageNode)).toBe(false); // a group's rest is not one folder
    expect(canOpen(unmeasured)).toBe(false);
    expect(canOpen(macos.children[0] as StorageNode)).toBe(false);
    expect(canOpen(node({ id: "/empty", entries: 0 }))).toBe(false);
    expect(canOpen(node({ id: "/f", kind: "file", bytes: 1 }))).toBe(false);
  });

  it("knows which folders need a live listing", () => {
    const yarn = caches.children[0] as StorageNode;
    expect(needsListing(yarn, none)).toBe(true);
    expect(needsListing(caches, none)).toBe(false);
    const loaded: Loaded = new Map([[yarn.id, { ...yarn, children: [node({ id: `${yarn.id}/v6`, bytes: 4.8e9 })] }]]);
    expect(needsListing(yarn, loaded)).toBe(false);
    expect(childrenOf(yarn, loaded).map((c) => c.id)).toEqual(["/Users/you/Library/Caches/Yarn/v6"]);
    expect(isFolderRest(caches.children[1] as StorageNode)).toBe(true);
    expect(isFolderRest(homeOther.children[1] as StorageNode)).toBe(false);
  });

  it("builds the breadcrumb trail through kept and loaded levels", () => {
    const root = rootNode(breakdown());
    expect(trail(root, "/Users/you/Library/Caches/Yarn", none)?.map((n) => n.id)).toEqual([
      ROOT_ID,
      "/Users/you/Library",
      "/Users/you/Library/Caches",
      "/Users/you/Library/Caches/Yarn",
    ]);
    expect(trail(root, "/Users/you/.npm", none)?.map((n) => n.id)).toEqual([ROOT_ID, "@home-other", "/Users/you/.npm"]);
    expect(trail(root, "/Users/you/Library/Caches/Yarn/v6", none)).toBeNull();
    const yarn = caches.children[0] as StorageNode;
    const loaded: Loaded = new Map([[yarn.id, { ...yarn, children: [node({ id: `${yarn.id}/v6` })] }]]);
    expect(trail(root, "/Users/you/Library/Caches/Yarn/v6", loaded)?.length).toBe(5);
  });

  it("falls back to the deepest level that still exists after a re-measure", () => {
    const root = rootNode(breakdown());
    const ids = [ROOT_ID, "/Users/you/Library", "/Users/you/Library/Caches", "/Users/you/Library/Caches/Gone"];
    expect(resolveTrail(root, ids, none).map((n) => n.id)).toEqual([
      ROOT_ID,
      "/Users/you/Library",
      "/Users/you/Library/Caches",
    ]);
    expect(resolveTrail(root, ["/nowhere"], none).map((n) => n.id)).toEqual([ROOT_ID]);
  });
});

describe("barShare", () => {
  it("is the part of the parent, with the found part inside it", () => {
    expect(barShare(caches, 20e9)).toEqual({ total: 0.6, found: 0.24 });
    expect(barShare(caches, 0)).toEqual({ total: 0, found: 0 });
    expect(barShare({ ...caches, foundBytes: 99e9 }, 20e9).found).toBe(0.6);
  });
});

describe("isStale", () => {
  const report = { startedAt: "2026-11-25T11:00:00Z" } as ScanReport;
  it("measures again when a scan finished after the measurement", () => {
    expect(isStale(null, null)).toBe(true);
    expect(isStale(breakdown(), null)).toBe(false);
    expect(isStale(breakdown(), report)).toBe(true);
    expect(isStale(breakdown({ measuredAt: "2026-11-25T11:05:00Z" }), report)).toBe(false);
  });
});

describe("progressFraction", () => {
  it("never shows done before it is", () => {
    expect(progressFraction(null)).toBeNull();
    expect(progressFraction({ phase: "volumes", walkedBytes: 0, expectedBytes: 0 })).toBeNull();
    expect(progressFraction({ phase: "walking", walkedBytes: 50, expectedBytes: 100 })).toBe(0.5);
    expect(progressFraction({ phase: "walking", walkedBytes: 120, expectedBytes: 100 })).toBe(0.99);
    expect(progressFraction({ phase: "done", walkedBytes: 90, expectedBytes: 100 })).toBe(1);
    expect(progressFraction({ phase: "walking", walkedBytes: 5, expectedBytes: 0 })).toBeNull();
  });
});

describe("findingsIn", () => {
  const findings = [
    finding("/Users/you/Library/Caches/Yarn"),
    finding("/Users/you/Library/Caches"), // contains Yarn
    finding("/Users/you/.npm/_cacache"),
    finding("/Users/you/Library-old"), // a sibling with the same prefix, not inside
    finding(null),
  ];

  it("matches findings inside, equal to or containing the node", () => {
    const yarn = caches.children[0] as StorageNode;
    expect(findingsIn(yarn, findings).map((f) => f.path)).toEqual([
      "/Users/you/Library/Caches/Yarn",
      "/Users/you/Library/Caches",
    ]);
    expect(findingsIn(library, findings).map((f) => f.path)).toEqual([
      "/Users/you/Library/Caches/Yarn",
      "/Users/you/Library/Caches",
    ]);
  });

  it("looks through groups and ignores volumes and rests", () => {
    expect(findingsIn(homeOther, findings).map((f) => f.path)).toEqual(["/Users/you/.npm/_cacache"]);
    expect(findingsIn(macos, findings)).toEqual([]);
    expect(findingsIn(caches.children[1] as StorageNode, findings)).toEqual([]);
  });

  it("searches Findings for the one finding's path, else for the node's path", () => {
    const one = [finding("/Users/you/.npm/_cacache")];
    expect(findingsQuery(node({ id: "/Users/you/.npm" }), one)).toBe("/Users/you/.npm/_cacache");
    expect(findingsQuery(library, findings.slice(0, 2))).toBe("/Users/you/Library");
    expect(findingsQuery(homeOther, [])).toBe("");
  });
});

describe("accounting", () => {
  it("says what the parts add up to and how much was not measured", () => {
    expect(accounting(breakdown())).toEqual({ parts: 60e9, difference: 0, unmeasuredShare: 7e9 / 60e9 });
    expect(accounting(breakdown({ usedBytes: 0, nodes: [] }))).toEqual({ parts: 0, difference: 0, unmeasuredShare: 0 });
  });
});

describe("hasProtected", () => {
  it("finds a protected folder anywhere in the tree", () => {
    expect(hasProtected(breakdown().nodes)).toBe(false);
    const locked = {
      ...caches,
      children: [{ ...(caches.children[0] as StorageNode), unreadable: "protected" as const }],
    };
    expect(hasProtected([{ ...library, children: [locked] }])).toBe(true);
    expect(hasProtected([{ ...library, unreadable: "denied" }])).toBe(false);
  });
});
