// Growth rows from the history database (contract v2): `kind` says whether a row sums a rule or
// tracks one finding, so keys are never parsed.

import type { Finding } from "../bindings/Finding";
import type { GrowthItem } from "../bindings/GrowthItem";
import { tildify } from "./format";

export function splitGrowth(items: readonly GrowthItem[]): { byRule: GrowthItem[]; byItem: GrowthItem[] } {
  return {
    byRule: items.filter((g) => g.kind === "rule"),
    byItem: items.filter((g) => g.kind === "item"),
  };
}

/**
 * Second line of a growth row: for an item still in the current report, its path (with `~`);
 * otherwise the key as-is (rule id, or the id of a finding that is gone).
 */
export function growthSubtitle(
  g: GrowthItem,
  findings: readonly Finding[] | undefined,
): { text: string; title: string } {
  if (g.kind === "item") {
    const path = findings?.find((f) => f.id === g.key)?.path;
    if (path) return { text: tildify(path), title: path };
  }
  return { text: g.key, title: g.key };
}
