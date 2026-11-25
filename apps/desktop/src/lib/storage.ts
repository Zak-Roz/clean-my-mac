// Pure Storage-view logic (ARCHITECTURE.md §11): the drill-down tree with live-loaded folder
// listings layered on top, breadcrumbs, bar shares, staleness and the link to matching findings.
// No React here so it stays easy to test.

import type { Finding } from "../bindings/Finding";
import type { ScanReport } from "../bindings/ScanReport";
import type { StorageBreakdown } from "../bindings/StorageBreakdown";
import type { StorageNode } from "../bindings/StorageNode";
import type { StorageProgress } from "../bindings/StorageProgress";
import { isInside } from "./findings";

/** Id of the synthetic node above the top level ("Used space"). */
export const ROOT_ID = "@root";

/** Folder listings loaded on demand (`storageChildren`), by the id of the node they replace the children of. */
export type Loaded = ReadonlyMap<string, StorageNode>;

/** The top level as one node, so drilling and breadcrumbs treat it like any other level. */
export function rootNode(b: StorageBreakdown): StorageNode {
  return {
    id: ROOT_ID,
    label: "Used space",
    path: null,
    kind: "group",
    bytes: b.usedBytes,
    files: b.nodes.reduce((sum, n) => sum + n.files, 0),
    entries: b.nodes.length,
    modified: null,
    unreadable: null,
    foundBytes: b.nodes.reduce((sum, n) => sum + n.foundBytes, 0),
    note: null,
    children: b.nodes,
  };
}

/** The children to show for `node`: a live listing when one was loaded, else the kept ones. */
export function childrenOf(node: StorageNode, loaded: Loaded): StorageNode[] {
  return loaded.get(node.id)?.children ?? node.children;
}

/** A `rest` row of a folder (not of a group): opening it lists the whole folder. */
export function isFolderRest(node: StorageNode): boolean {
  return node.kind === "rest" && node.path !== null && !node.id.startsWith("@") && node.entries > 0;
}

/** Folders whose entries were not kept that deep: they need a live listing before they can be shown. */
export function needsListing(node: StorageNode, loaded: Loaded): boolean {
  return node.kind === "folder" && node.entries > 0 && childrenOf(node, loaded).length === 0;
}

/** Whether clicking the row goes one level deeper. */
export function canOpen(node: StorageNode): boolean {
  switch (node.kind) {
    case "group":
      return node.children.length > 0;
    case "folder":
      return node.entries > 0;
    case "rest":
      return isFolderRest(node);
    default:
      return false;
  }
}

/**
 * Nodes from the root to `id` (both included), following live listings; null when `id` is not
 * in the tree (any more). Opening a folder's `rest` row shows the folder itself, fully listed.
 */
export function trail(root: StorageNode, id: string, loaded: Loaded): StorageNode[] | null {
  if (root.id === id) return [root];
  for (const child of childrenOf(root, loaded)) {
    const below = trail(child, id, loaded);
    if (below) return [root, ...below];
  }
  return null;
}

/** The deepest still-existing prefix of a remembered trail of ids (after a re-measure). */
export function resolveTrail(root: StorageNode, ids: readonly string[], loaded: Loaded): StorageNode[] {
  let best: StorageNode[] = [root];
  for (let i = ids.length; i > 0; i -= 1) {
    const found = trail(root, ids[i - 1] as string, loaded);
    if (found) {
      best = found;
      break;
    }
  }
  return best;
}

/** Fraction of the parent a row's bar covers, and the found part of it (both 0–1). */
export function barShare(node: StorageNode, parentBytes: number): { total: number; found: number } {
  if (parentBytes <= 0) return { total: 0, found: 0 };
  const total = Math.min(1, node.bytes / parentBytes);
  const found = Math.min(total, node.foundBytes / parentBytes);
  return { total, found };
}

/** The measurement predates the last scan (or there is none): measure again ("cached per scan"). */
export function isStale(b: StorageBreakdown | null, report: ScanReport | null): boolean {
  if (!b) return true;
  if (!report) return false;
  return Date.parse(b.measuredAt) < Date.parse(report.startedAt);
}

/** 0–1 while walking (never 1 before it is done), null when the expected size is unknown. */
export function progressFraction(p: StorageProgress | null): number | null {
  if (!p || p.phase === "volumes" || p.expectedBytes <= 0) return null;
  if (p.phase === "done") return 1;
  return Math.min(0.99, p.walkedBytes / p.expectedBytes);
}

/** Paths a node stands for: its own, or for a group every child's (recursively, without rests). */
function pathsOf(node: StorageNode): string[] {
  if (node.kind === "rest" || node.kind === "volume" || node.kind === "unmeasured") return [];
  if (node.path !== null) return [node.path];
  return node.children.flatMap(pathsOf);
}

/** Findings that lie in the node or contain it (a node inside a found folder is wholly found). */
export function findingsIn(node: StorageNode, findings: readonly Finding[]): Finding[] {
  const paths = pathsOf(node);
  return findings.filter((f) => {
    const p = f.path;
    if (p === null) return false;
    return paths.some((q) => p === q || isInside(p, q) || isInside(q, p));
  });
}

/** What to search for in Findings to show the findings of a node. */
export function findingsQuery(node: StorageNode, matching: readonly Finding[]): string {
  const only = matching.length === 1 ? matching[0] : undefined;
  if (only?.path) return only.path;
  return node.path ?? "";
}

/** Totals of the top level: what the parts add up to, and how far that is from the used space. */
export function accounting(b: StorageBreakdown): { parts: number; difference: number; unmeasuredShare: number } {
  const parts = b.nodes.reduce((sum, n) => sum + n.bytes, 0);
  return {
    parts,
    difference: parts - b.usedBytes,
    unmeasuredShare: b.usedBytes > 0 ? b.unmeasuredBytes / b.usedBytes : 0,
  };
}

/** Any node in the tree flagged as protected by macOS (Full Disk Access can help). */
export function hasProtected(nodes: readonly StorageNode[]): boolean {
  return nodes.some((n) => n.unreadable === "protected" || hasProtected(n.children));
}
