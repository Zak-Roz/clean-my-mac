// Pure helpers for clean results: outcome counts and the reasons the core reports when an item
// changed since the scan (ARCHITECTURE.md §10, safeguard 1: the backend re-scans before planning
// and before cleaning, and leaves alone whatever no longer matches what the user picked).

import type { CleanItemResult } from "../bindings/CleanItemResult";
import type { CleanResult } from "../bindings/CleanResult";

/** The core's reason (`actions::NO_LONGER_MATCHES`) for an item a fresh scan no longer confirms. */
export const NO_LONGER_MATCHES = "no longer matches the scan";

/** The core's note when an action happened but its audit-log entry could not be written. */
export const AUDIT_LOG_FAILED = "audit log write failed";

export interface OutcomeCounts {
  done: number;
  skipped: number;
  failed: number;
  /** Skipped because they changed since the scan (a subset of `skipped`). */
  stale: number;
  /** Done, but not recorded in the audit log. */
  unaudited: number;
}

/** Left alone because it changed since the scan the user picked it from. */
export function isStale(item: CleanItemResult): boolean {
  return item.outcome === "skipped" && (item.message ?? "").includes(NO_LONGER_MATCHES);
}

export function isUnaudited(item: CleanItemResult): boolean {
  return (item.message ?? "").includes(AUDIT_LOG_FAILED);
}

export function outcomeCounts(result: CleanResult): OutcomeCounts {
  const counts: OutcomeCounts = { done: 0, skipped: 0, failed: 0, stale: 0, unaudited: 0 };
  for (const item of result.items) {
    counts[item.outcome] += 1;
    if (isStale(item)) counts.stale += 1;
    if (isUnaudited(item)) counts.unaudited += 1;
  }
  return counts;
}

/** Plan warnings about selected items the plan left out because they changed since the scan. */
export function staleWarnings(warnings: string[]): string[] {
  return warnings.filter((w) => w.includes(NO_LONGER_MATCHES));
}

/** Item message without the core's "skipped: " prefix (the status icon already says it). */
export function itemMessage(item: CleanItemResult): string | null {
  if (item.message === null) return null;
  return item.outcome === "skipped" ? item.message.replace(/^skipped: /, "") : item.message;
}
