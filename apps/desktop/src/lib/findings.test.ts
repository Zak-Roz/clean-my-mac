import { describe, expect, it } from "vitest";
import fixture from "../../../../fixtures/sample-report.json";
import type { Finding } from "../bindings/Finding";
import type { ScanReport } from "../bindings/ScanReport";
import {
  actionDescription,
  dedupedTotal,
  defaultSelection,
  doneDescription,
  groupFindings,
  isBulkSelectable,
  isInside,
  isPermanentAction,
  isSelectable,
  matchesQuery,
} from "./findings";
import { formatBytes, formatDelta, tildify } from "./format";

const report = fixture as unknown as ScanReport;

function finding(over: Partial<Finding>): Finding {
  return {
    id: `t:${over.path ?? over.label}`,
    ruleId: "t",
    category: "dev-artifacts",
    risk: "review",
    label: "x",
    path: null,
    bytes: 0,
    logicalBytes: 0,
    files: 0,
    isDir: false,
    lastUsed: null,
    reason: "",
    action: "trash",
    command: null,
    selectedByDefault: false,
    blockers: [],
    group: null,
    ...over,
  };
}

describe("isInside", () => {
  it("matches strict descendants only", () => {
    expect(isInside("/a/b/c", "/a/b")).toBe(true);
    expect(isInside("/a/b", "/a/b")).toBe(false);
    expect(isInside("/a/bc", "/a/b")).toBe(false);
    expect(isInside("/a/b/c", "/a/b/")).toBe(true);
  });
});

describe("dedupedTotal", () => {
  it("counts a nested path once, under its selected ancestor", () => {
    const worktree = finding({ path: "/r/.claude/worktrees/w1", bytes: 100 });
    const nested = finding({ path: "/r/.claude/worktrees/w1/node_modules", bytes: 40 });
    const other = finding({ path: "/r/other", bytes: 7 });
    const { bytes, covered } = dedupedTotal([nested, worktree, other]);
    expect(bytes).toBe(107);
    expect(covered).toEqual([nested]);
  });

  it("counts duplicate paths once and path-less findings always", () => {
    const a = finding({ label: "a", path: "/x", bytes: 5 });
    const b = finding({ label: "b", path: "/x", bytes: 5 });
    const cmd = finding({ label: "docker", bytes: 13 });
    expect(dedupedTotal([a, b, cmd]).bytes).toBe(18);
  });

  it("does not treat sibling prefixes as nesting", () => {
    const a = finding({ path: "/d/mongo_1", bytes: 1 });
    const b = finding({ path: "/d/mongo_10", bytes: 2 });
    expect(dedupedTotal([a, b]).bytes).toBe(3);
  });
});

describe("selection rules", () => {
  it("never preselects or bulk-selects manual, blocked or info-only items", () => {
    const manual = finding({ label: "m", risk: "manual" });
    const blocked = finding({ label: "b", blockers: ["Claude is running"] });
    const info = finding({ label: "i", action: "info-only" });
    expect(isBulkSelectable(manual)).toBe(false);
    expect(isSelectable(manual)).toBe(true);
    expect(isSelectable(blocked)).toBe(false);
    expect(isSelectable(info)).toBe(false);
  });

  it("preselects exactly the fixture's safe, unblocked items", () => {
    const ids = defaultSelection(report.findings);
    expect([...ids].sort()).toEqual(
      report.findings
        .filter((f) => f.selectedByDefault)
        .map((f) => f.id)
        .sort(),
    );
    for (const f of report.findings.filter((f) => ids.has(f.id))) expect(f.risk).toBe("safe");
  });
});

describe("groupFindings", () => {
  it("orders categories by size and gathers a group into one block", () => {
    const sections = groupFindings(report.findings);
    const totals = sections.map((s) => s.total);
    expect([...totals].sort((a, b) => b - a)).toEqual(totals);

    const worktrees = sections.find((s) => s.category === "worktrees");
    expect(worktrees?.blocks).toHaveLength(1);
    const block = worktrees?.blocks[0];
    expect(block?.kind).toBe("set");
    if (block?.kind === "set") expect(block.findings.map((f) => f.label)).toContain("agent-task");
  });

  it("covers every finding exactly once", () => {
    const ids = groupFindings(report.findings).flatMap((s) =>
      s.blocks.flatMap((b) => (b.kind === "single" ? [b.finding.id] : b.findings.map((f) => f.id))),
    );
    expect(ids.sort()).toEqual(report.findings.map((f) => f.id).sort());
  });
});

describe("matchesQuery", () => {
  it("searches label, path, reason, rule and command", () => {
    const docker = report.findings.find((f) => f.ruleId === "containers.docker") as Finding;
    expect(matchesQuery(docker, "prune")).toBe(true);
    expect(matchesQuery(docker, "DOCKER")).toBe(true);
    expect(matchesQuery(docker, "yarn")).toBe(false);
    expect(matchesQuery(docker, "  ")).toBe(true);
  });
});

describe("format", () => {
  it("uses decimal units like Finder", () => {
    expect(formatBytes(494_384_795_648)).toBe("494 GB");
    expect(formatBytes(47_861_239_808)).toBe("47.9 GB");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatDelta(-4_700_000_000)).toBe("−4.7 GB");
    expect(formatDelta(1_200_000)).toBe("+1.2 MB");
  });

  it("shortens the home directory", () => {
    expect(tildify("/Users/you/Library/Caches/Yarn")).toBe("~/Library/Caches/Yarn");
    expect(tildify("/Library/Caches")).toBe("/Library/Caches");
  });
});

describe("worktree actions follow the clean mode", () => {
  const wt = finding({ path: "/r/.claude/worktrees/w1", action: "git-worktree-remove" });
  const docker = finding({ label: "docker", action: "command", command: ["docker", "system", "prune", "-f"] });

  it("describes Trash and Delete differently", () => {
    expect(actionDescription(wt, "trash")).toBe("Move to Trash with its git admin data");
    expect(actionDescription(wt, "delete")).toBe("git worktree remove (permanent)");
    expect(doneDescription(wt, "trash")).toBe("Moved to Trash with its git admin data");
    expect(doneDescription(wt, "delete")).toBe("Removed with git worktree remove");
  });

  it("only commands free space whatever the mode", () => {
    expect(isPermanentAction(wt)).toBe(false);
    expect(isPermanentAction(docker)).toBe(true);
  });
});
