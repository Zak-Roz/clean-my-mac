import { describe, expect, it } from "vitest";
import fixture from "../../../../fixtures/sample-report.json";
import type { Finding } from "../bindings/Finding";
import type { ScanReport } from "../bindings/ScanReport";
import {
  blockerHints,
  COLLAPSE_STORAGE_KEY,
  type CollapseState,
  categoryFoldId,
  defaultSetCollapsed,
  EMPTY_COLLAPSE,
  type Foldable,
  foldables,
  foldSummary,
  isCollapsed,
  loadOverrides,
  MAX_REMEMBERED,
  parseOverrides,
  SET_COLLAPSE_THRESHOLD,
  saveOverrides,
  setCollapsed,
  setFoldId,
} from "./collapse";
import { groupFindings, matchesQuery } from "./findings";

const report = fixture as unknown as ScanReport;

function finding(over: Partial<Finding>): Finding {
  return {
    id: `t:${over.label}`,
    ruleId: "t",
    category: "worktrees",
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

const bigSet: Foldable = { id: setFoldId("/r/big"), defaultCollapsed: true };
const smallSet: Foldable = { id: setFoldId("/r/small"), defaultCollapsed: false };
const category: Foldable = { id: categoryFoldId("worktrees"), defaultCollapsed: false };

class MemoryStorage {
  data = new Map<string, string>();
  getItem(key: string) {
    return this.data.get(key) ?? null;
  }
  setItem(key: string, value: string) {
    this.data.set(key, value);
  }
}

describe("default fold state", () => {
  it("collapses only sets larger than the threshold", () => {
    expect(defaultSetCollapsed(SET_COLLAPSE_THRESHOLD)).toBe(false);
    expect(defaultSetCollapsed(SET_COLLAPSE_THRESHOLD + 1)).toBe(true);
  });

  it("starts categories expanded and the fixture's big worktree set collapsed", () => {
    const items = foldables(groupFindings(report.findings));
    for (const item of items.filter((i) => i.id.startsWith("category:"))) expect(item.defaultCollapsed).toBe(false);
    expect(items).toContainEqual({ id: setFoldId("/Users/you/Developer/example-app"), defaultCollapsed: true });
    expect(items).toContainEqual({ id: setFoldId("Downloads/backups/mongo_*"), defaultCollapsed: false });
  });

  it("follows the default until the user chooses", () => {
    expect(isCollapsed(EMPTY_COLLAPSE, "", bigSet)).toBe(true);
    expect(isCollapsed(EMPTY_COLLAPSE, "", smallSet)).toBe(false);
    const state = setCollapsed(EMPTY_COLLAPSE, "", [bigSet.id], false);
    expect(isCollapsed(state, "", bigSet)).toBe(false);
    expect(isCollapsed(setCollapsed(state, "", [smallSet.id], true), "", smallSet)).toBe(true);
  });
});

describe("search", () => {
  const folded = setCollapsed(EMPTY_COLLAPSE, "", [category.id, smallSet.id], true);

  it("expands everything while a query is active, then restores the remembered state", () => {
    for (const item of [bigSet, smallSet, category]) expect(isCollapsed(folded, "feat", item)).toBe(false);
    expect(isCollapsed(folded, "   ", category)).toBe(true);
    expect(isCollapsed(folded, "", bigSet)).toBe(true);
  });

  it("folds during a search without touching remembered choices, until the query changes", () => {
    const during = setCollapsed(folded, "Feat", [bigSet.id], true);
    expect(during.overrides).toEqual(folded.overrides);
    expect(isCollapsed(during, "feat ", bigSet)).toBe(true);
    expect(isCollapsed(during, "feature", bigSet)).toBe(false);
    expect(isCollapsed(during, "", smallSet)).toBe(true);
  });

  it("shows every match: a set holding a matching row renders expanded", () => {
    const sections = groupFindings(report.findings.filter((f) => matchesQuery(f, "bugfix")));
    const state = setCollapsed(EMPTY_COLLAPSE, "", [setFoldId("/Users/you/Developer/example-app")], true);
    const items = foldables(sections);
    expect(items.length).toBeGreaterThan(0);
    for (const item of items) expect(isCollapsed(state, "bugfix", item)).toBe(false);
  });
});

describe("remembered choices", () => {
  it("round-trips through storage", () => {
    const storage = new MemoryStorage();
    const state: CollapseState = setCollapsed(EMPTY_COLLAPSE, "", [bigSet.id], false);
    saveOverrides(storage, state.overrides);
    expect(loadOverrides(storage)).toEqual({ [bigSet.id]: false });
  });

  it("works without storage and survives one that throws", () => {
    expect(loadOverrides(null)).toEqual({});
    saveOverrides(null, { a: true });
    const broken = {
      getItem: () => {
        throw new Error("SecurityError");
      },
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
    };
    expect(loadOverrides(broken)).toEqual({});
    expect(() => saveOverrides(broken, { a: true })).not.toThrow();
  });

  it("ignores malformed stored values", () => {
    expect(parseOverrides(null)).toEqual({});
    expect(parseOverrides("{not json")).toEqual({});
    expect(parseOverrides("[true]")).toEqual({});
    expect(parseOverrides("null")).toEqual({});
    expect(parseOverrides('{"a":true,"b":"yes","c":false}')).toEqual({ a: true, c: false });
    const storage = new MemoryStorage();
    storage.setItem(COLLAPSE_STORAGE_KEY, "garbage");
    expect(loadOverrides(storage)).toEqual({});
  });

  it("keeps only the most recent choices", () => {
    let state = EMPTY_COLLAPSE;
    for (let i = 0; i < MAX_REMEMBERED + 5; i++) state = setCollapsed(state, "", [`set:${i}`], true);
    state = setCollapsed(state, "", ["set:5"], false);
    const ids = Object.keys(state.overrides);
    expect(ids).toHaveLength(MAX_REMEMBERED);
    expect(ids).not.toContain("set:4");
    expect(ids.at(-1)).toBe("set:5");
    expect(state.overrides["set:5"]).toBe(false);
  });
});

describe("blockerHints", () => {
  it("counts findings per blocker kind and the rest as blocked", () => {
    const hints = blockerHints([
      finding({ label: "a", blockers: ["Unpushed commits (3 not on any remote branch)"] }),
      finding({ label: "b", blockers: ["Unpushed commits (6 not on any remote branch)"] }),
      finding({
        label: "c",
        blockers: ["Uncommitted changes in worktree (2 files)", "Unpushed commits (1 not on any remote branch)"],
      }),
      finding({ label: "d", blockers: ["Claude is running"] }),
      finding({ label: "e", blockers: ["Claude is running"] }),
      finding({ label: "f" }),
    ]);
    expect(hints).toEqual([
      { text: "3 with unpushed commits", reasons: [] },
      { text: "1 with uncommitted changes", reasons: [] },
      { text: "2 blocked", reasons: ["Claude is running"] },
    ]);
  });

  it("says nothing when nothing is blocked", () => {
    expect(blockerHints([finding({ label: "a" }), finding({ label: "b", risk: "manual" })])).toEqual([]);
  });
});

describe("foldSummary", () => {
  it("counts ticked items that can still be cleaned", () => {
    const a = finding({ label: "a" });
    const b = finding({ label: "b", blockers: ["Unpushed commits (2 not on any remote branch)"] });
    const c = finding({ label: "c" });
    const summary = foldSummary([a, b, c], new Set([a.id, b.id, "elsewhere"]));
    expect(summary.selected).toBe(1);
    expect(summary.hints.map((h) => h.text)).toEqual(["1 with unpushed commits"]);
  });
});
