import { describe, expect, it } from "vitest";
import type { Finding } from "../bindings/Finding";
import type { GrowthItem } from "../bindings/GrowthItem";
import { growthSubtitle, splitGrowth } from "./growth";

function growth(over: Partial<GrowthItem>): GrowthItem {
  return { kind: "item", key: "k", label: "l", bytesThen: 0, bytesNow: 1, delta: 1, ...over };
}

const npm = { id: "cache.npm:/Users/me/.npm/_cacache", path: "/Users/me/.npm/_cacache" } as Finding;

describe("splitGrowth", () => {
  it("splits by kind, never by the key's shape", () => {
    const rows = [
      growth({ kind: "rule", key: "cache.npm" }),
      growth({ kind: "item", key: npm.id }),
      // A rule key with a colon would have been misfiled by the old `key.includes(":")` check.
      growth({ kind: "rule", key: "odd:rule" }),
    ];
    const { byRule, byItem } = splitGrowth(rows);
    expect(byRule.map((g) => g.key)).toEqual(["cache.npm", "odd:rule"]);
    expect(byItem.map((g) => g.key)).toEqual([npm.id]);
  });
});

describe("growthSubtitle", () => {
  it("shows the path of an item that is still in the report", () => {
    expect(growthSubtitle(growth({ key: npm.id }), [npm])).toEqual({
      text: "~/.npm/_cacache",
      title: "/Users/me/.npm/_cacache",
    });
  });

  it("falls back to the key for rules and for findings that are gone", () => {
    expect(growthSubtitle(growth({ kind: "rule", key: "cache.npm" }), [npm]).text).toBe("cache.npm");
    expect(growthSubtitle(growth({ key: "dev.nm:/gone" }), [npm]).text).toBe("dev.nm:/gone");
    expect(growthSubtitle(growth({ key: npm.id }), undefined).text).toBe(npm.id);
  });
});
