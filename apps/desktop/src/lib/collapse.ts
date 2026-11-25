// Which category sections and sets of the Findings list are folded. Pure logic: the screen keeps a
// `CollapseState` in React state and mirrors `overrides` to localStorage through the helpers below.
// Folding only hides rows; it never changes what is selected or visible to Select All and totals.

import type { Category } from "../bindings/Category";
import type { Finding } from "../bindings/Finding";
import { type CategorySection, isSelectable } from "./findings";

/** Sets with more items than this start collapsed. */
export const SET_COLLAPSE_THRESHOLD = 3;

/** How many remembered choices are kept; the least recently changed ones go first. */
export const MAX_REMEMBERED = 200;

export const COLLAPSE_STORAGE_KEY = "cmm.findings.collapsed";

export const categoryFoldId = (category: Category) => `category:${category}`;
export const setFoldId = (key: string) => `set:${key}`;

/** Something that can be folded, with its folded state when the user never chose one. */
export interface Foldable {
  id: string;
  defaultCollapsed: boolean;
}

export function defaultSetCollapsed(size: number): boolean {
  return size > SET_COLLAPSE_THRESHOLD;
}

/** Every category and set in `sections`, in display order. Categories start expanded. */
export function foldables(sections: CategorySection[]): Foldable[] {
  return sections.flatMap((s) => [
    { id: categoryFoldId(s.category), defaultCollapsed: false },
    ...s.blocks.flatMap((b) =>
      b.kind === "set" ? [{ id: setFoldId(b.key), defaultCollapsed: defaultSetCollapsed(b.findings.length) }] : [],
    ),
  ]);
}

/** The user's explicit choices, by fold id (`true` = collapsed). Persisted. */
export type Overrides = Readonly<Record<string, boolean>>;

export interface CollapseState {
  overrides: Overrides;
  /**
   * What the user folded during the current search. While a query is active everything starts
   * expanded, so search never hides a match; this resets whenever the query changes.
   */
  search: { query: string; collapsed: ReadonlySet<string> } | null;
}

export const EMPTY_COLLAPSE: CollapseState = { overrides: {}, search: null };

/** Same normalization as `matchesQuery`: a blank query is no search. */
function searchKey(query: string): string {
  return query.trim().toLowerCase();
}

export function isCollapsed(state: CollapseState, query: string, item: Foldable): boolean {
  const q = searchKey(query);
  if (q) return state.search?.query === q && state.search.collapsed.has(item.id);
  return state.overrides[item.id] ?? item.defaultCollapsed;
}

/**
 * Folds or unfolds `ids`. Without a search this is remembered; during a search it only lasts
 * until the query changes, and the remembered choices are left alone.
 */
export function setCollapsed(state: CollapseState, query: string, ids: string[], collapsed: boolean): CollapseState {
  const q = searchKey(query);
  if (q) {
    const next = new Set(state.search?.query === q ? state.search.collapsed : []);
    for (const id of ids) {
      if (collapsed) next.add(id);
      else next.delete(id);
    }
    return { ...state, search: { query: q, collapsed: next } };
  }
  return { ...state, overrides: remember(state.overrides, ids, collapsed) };
}

/** Records the choice as the most recent one and drops the oldest beyond `MAX_REMEMBERED`. */
function remember(overrides: Overrides, ids: string[], collapsed: boolean): Overrides {
  const entries = Object.entries(overrides).filter(([id]) => !ids.includes(id));
  for (const id of ids) entries.push([id, collapsed]);
  return Object.fromEntries(entries.slice(-MAX_REMEMBERED));
}

/** Reads remembered choices; anything unreadable counts as none. */
export function parseOverrides(raw: string | null): Overrides {
  if (!raw) return {};
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value !== "object" || value === null || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value).filter((e): e is [string, boolean] => typeof e[1] === "boolean"));
  } catch {
    return {};
  }
}

type KeyValueStore = Pick<Storage, "getItem" | "setItem">;

/** `localStorage`, or null where the page has none (private mode, blocked storage, tests). */
export function browserStorage(): KeyValueStore | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

export function loadOverrides(storage: KeyValueStore | null): Overrides {
  try {
    return parseOverrides(storage?.getItem(COLLAPSE_STORAGE_KEY) ?? null);
  } catch {
    return {};
  }
}

export function saveOverrides(storage: KeyValueStore | null, overrides: Overrides): void {
  try {
    storage?.setItem(COLLAPSE_STORAGE_KEY, JSON.stringify(overrides));
  } catch {
    // Storage full or blocked: the choice still applies until the app closes.
  }
}

/** A short note on what a folded set or section hides, such as "3 with unpushed commits". */
export interface FoldHint {
  text: string;
  /** Distinct blocker messages behind a generic "blocked" hint, for a tooltip. */
  reasons: string[];
}

const BLOCKER_KINDS: { match: RegExp; label: string }[] = [
  { match: /^unpushed commits/i, label: "with unpushed commits" },
  { match: /^uncommitted changes/i, label: "with uncommitted changes" },
];

/**
 * Counts blocked findings by the kind of blocker. A finding counts once per known kind it has;
 * findings blocked only for other reasons are counted together as "blocked".
 */
export function blockerHints(findings: Finding[]): FoldHint[] {
  const counts = new Map<string, number>();
  let other = 0;
  const reasons = new Set<string>();
  for (const f of findings) {
    if (f.blockers.length === 0) continue;
    const kinds = new Set<string>();
    for (const b of f.blockers) {
      const kind = BLOCKER_KINDS.find((k) => k.match.test(b));
      if (kind) kinds.add(kind.label);
    }
    if (kinds.size === 0) {
      other += 1;
      for (const b of f.blockers) reasons.add(b);
    }
    for (const k of kinds) counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  const hints: FoldHint[] = BLOCKER_KINDS.filter((k) => counts.has(k.label)).map((k) => ({
    text: `${counts.get(k.label)} ${k.label}`,
    reasons: [],
  }));
  if (other > 0) hints.push({ text: `${other} blocked`, reasons: [...reasons] });
  return hints;
}

/** What a folded set or section shows in its header: how many of its items are ticked, and what is blocked. */
export function foldSummary(
  findings: Finding[],
  selection: ReadonlySet<string>,
): { selected: number; hints: FoldHint[] } {
  return {
    selected: findings.filter((f) => selection.has(f.id) && isSelectable(f)).length,
    hints: blockerHints(findings),
  };
}
