import { describe, expect, it } from "vitest";
import type { CleanItemResult } from "../bindings/CleanItemResult";
import type { CleanResult } from "../bindings/CleanResult";
import { isStale, isUnaudited, itemMessage, outcomeCounts, staleWarnings } from "./clean";

function item(outcome: CleanItemResult["outcome"], message: string | null): CleanItemResult {
  return { findingId: `t:${outcome}:${message}`, outcome, bytes: 0, message };
}

const stale = item(
  "skipped",
  "skipped: no longer matches the scan (its action changed from trash to git-worktree-remove)",
);
const blocked = item("skipped", "skipped: worktree has uncommitted changes (2 path(s))");
const unaudited = item("done", "moved to Trash (audit log write failed: no such table: actions)");

describe("clean results", () => {
  it("tells items that changed since the scan from other skips", () => {
    expect(isStale(stale)).toBe(true);
    expect(isStale(blocked)).toBe(false);
    expect(isStale(item("failed", "no longer matches the scan"))).toBe(false);
    expect(isStale(item("skipped", null))).toBe(false);
  });

  it("counts outcomes, stale skips and unaudited actions", () => {
    const result: CleanResult = {
      dryRun: false,
      mode: "trash",
      items: [stale, blocked, unaudited, item("done", null), item("failed", "`docker system prune -f` failed")],
      freedBytes: 0,
      movedToTrashBytes: 0,
    };
    expect(outcomeCounts(result)).toEqual({ done: 2, skipped: 2, failed: 1, stale: 1, unaudited: 1 });
    expect(isUnaudited(unaudited)).toBe(true);
    expect(isUnaudited(blocked)).toBe(false);
  });

  it("picks the plan warnings about changed items", () => {
    const warnings = [
      "Skipped agent-task: no longer matches the scan (a new scan no longer finds it)",
      "Moving to Trash frees no disk space until the Trash is emptied (1.0 GB in 2 item(s)).",
      "Skipped vm_bundles: blocked: Claude is running",
    ];
    expect(staleWarnings(warnings)).toEqual([warnings[0]]);
  });

  it("drops the core's skipped prefix for display", () => {
    expect(itemMessage(blocked)).toBe("worktree has uncommitted changes (2 path(s))");
    expect(itemMessage(unaudited)).toBe(unaudited.message);
    expect(itemMessage(item("done", null))).toBeNull();
  });
});
