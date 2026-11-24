// Pure finding logic: labels, selection rules, nested-path de-duplication (ARCHITECTURE.md §10),
// grouping for the Findings list. No React here so it stays easy to test.

import type { ActionKind } from "../bindings/ActionKind";
import type { Category } from "../bindings/Category";
import type { CleanMode } from "../bindings/CleanMode";
import type { Finding } from "../bindings/Finding";
import type { Risk } from "../bindings/Risk";
import { basename, tildify } from "./format";

export const RISK_META: Record<Risk, { label: string; hint: string }> = {
  safe: { label: "Safe", hint: "Regenerates by itself. Preselected." },
  review: { label: "Review", hint: "Probably not needed. Check it, then tick it." },
  manual: { label: "Manual", hint: "Your data. Never preselected and never included in Select All." },
};

export const RISKS: Risk[] = ["safe", "review", "manual"];

export const CATEGORY_LABEL: Record<Category, string> = {
  downloads: "Downloads",
  "dev-artifacts": "Project build artifacts",
  worktrees: "Git worktrees",
  "package-caches": "Package caches",
  "ai-tools": "AI tools",
  xcode: "Xcode",
  containers: "Containers",
  "app-caches": "App caches",
  system: "System",
  "large-files": "Large files",
};

/** Whether a finding can be ticked at all. */
export function isSelectable(f: Finding): boolean {
  return f.blockers.length === 0 && f.action !== "info-only";
}

/** "Select all" and category/group checkboxes never pick manual items. */
export function isBulkSelectable(f: Finding): boolean {
  return isSelectable(f) && f.risk !== "manual";
}

export function defaultSelection(findings: Finding[]): Set<string> {
  return new Set(findings.filter((f) => f.selectedByDefault && isSelectable(f)).map((f) => f.id));
}

/** True when `child` is strictly inside directory `parent`. */
export function isInside(child: string, parent: string): boolean {
  const p = parent.endsWith("/") ? parent : `${parent}/`;
  return child.startsWith(p);
}

export interface Total {
  bytes: number;
  /** Findings left out of `bytes` because an ancestor (or the same path) is in the set. */
  covered: Finding[];
}

/**
 * Sums `bytes`, counting a path once even when a nested finding is also in the set
 * (e.g. a stale node_modules inside a selected worktree). Path-less findings always count.
 */
export function dedupedTotal(findings: Finding[]): Total {
  const paths = findings.filter((f) => f.path !== null).map((f) => f.path as string);
  const seen = new Set<string>();
  const covered: Finding[] = [];
  let bytes = 0;
  for (const f of findings) {
    if (f.path !== null) {
      const path = f.path;
      const nested = paths.some((p) => isInside(path, p));
      if (nested || seen.has(path)) {
        covered.push(f);
        continue;
      }
      seen.add(path);
    }
    bytes += f.bytes;
  }
  return { bytes, covered };
}

export function matchesQuery(f: Finding, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [f.label, f.path ?? "", f.reason, f.ruleId, f.group ?? "", (f.command ?? []).join(" ")].some((s) =>
    s.toLowerCase().includes(q),
  );
}

export type Block = { kind: "single"; finding: Finding } | { kind: "set"; key: string; findings: Finding[] };

export interface CategorySection {
  category: Category;
  findings: Finding[];
  blocks: Block[];
  total: number;
}

/**
 * Category sections (largest first) whose findings are kept in size order, with members of
 * the same `group` gathered into one block at the position of the largest member.
 */
export function groupFindings(findings: Finding[]): CategorySection[] {
  const byCategory = new Map<Category, Finding[]>();
  for (const f of findings) {
    const list = byCategory.get(f.category) ?? [];
    list.push(f);
    byCategory.set(f.category, list);
  }
  const sections: CategorySection[] = [];
  for (const [category, list] of byCategory) {
    const sorted = [...list].sort((a, b) => b.bytes - a.bytes);
    const blocks: Block[] = [];
    const sets = new Map<string, Finding[]>();
    for (const f of sorted) {
      if (f.group === null) {
        blocks.push({ kind: "single", finding: f });
        continue;
      }
      const existing = sets.get(f.group);
      if (existing) {
        existing.push(f);
      } else {
        const members = [f];
        sets.set(f.group, members);
        blocks.push({ kind: "set", key: f.group, findings: members });
      }
    }
    // A "set" of one is just a row.
    const flattened = blocks.map(
      (b): Block =>
        b.kind === "set" && b.findings.length === 1 ? { kind: "single", finding: b.findings[0] as Finding } : b,
    );
    sections.push({ category, findings: sorted, blocks: flattened, total: dedupedTotal(sorted).bytes });
  }
  return sections.sort((a, b) => b.total - a.total);
}

/** A group key is a repo path for worktrees or a relative glob for versioned sets. */
export function setTitle(key: string): { title: string; subtitle: string | null } {
  if (key.startsWith("/")) return { title: basename(key), subtitle: tildify(key) };
  return { title: key, subtitle: null };
}

/** What will physically happen to an item when cleaned in `mode`. */
export function actionDescription(f: Finding, mode: CleanMode): string {
  switch (f.action) {
    case "trash":
    case "delete":
      return mode === "trash" ? "Move to Trash" : "Delete permanently";
    case "git-worktree-remove":
      return mode === "trash" ? "Move to Trash with its git admin data" : "git worktree remove (permanent)";
    case "command":
      return `Runs ${(f.command ?? []).join(" ")}`;
    case "info-only":
      return "Nothing to do automatically";
  }
}

/** Past tense of `actionDescription`, for a successful result. */
export function doneDescription(f: Finding, mode: CleanMode): string {
  switch (f.action) {
    case "trash":
    case "delete":
      return mode === "trash" ? "Moved to Trash" : "Deleted";
    case "git-worktree-remove":
      return mode === "trash" ? "Moved to Trash with its git admin data" : "Removed with git worktree remove";
    case "command":
      return `Ran ${(f.command ?? []).join(" ")}`;
    case "info-only":
      return "Nothing done";
  }
}

/** Short chip for findings whose action is not a plain Trash move. */
export function actionChip(action: ActionKind): string | null {
  switch (action) {
    case "git-worktree-remove":
      return "git worktree";
    case "command":
      return "command";
    case "info-only":
      return "info only";
    case "trash":
    case "delete":
      return null;
  }
}

/**
 * Items whose action frees space immediately whatever the chosen mode: commands run their own tool
 * (docker, xcrun). Git worktrees follow the mode like plain paths.
 */
export function isPermanentAction(f: Finding): boolean {
  return f.action === "command";
}

/** Logical size is worth showing when the file is sparse or compressed (Docker.raw: 68.7 GB vs 11.5 GB). */
export function showLogical(f: Finding): boolean {
  return f.logicalBytes > f.bytes * 1.5 && f.logicalBytes - f.bytes > 1e9;
}
