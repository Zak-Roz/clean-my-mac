import { describe, expect, it } from "vitest";
import type { FileMeta } from "../bindings/FileMeta";
import type { Finding } from "../bindings/Finding";
import type { ScanReport } from "../bindings/ScanReport";
import {
  DOWNLOADS_RULE,
  entryKind,
  extensionOf,
  filterEntries,
  lastActivity,
  mainScanFlags,
  openedState,
  rangeIds,
  selectionSummary,
  sortEntries,
  sourceDomain,
  untouchedFor,
} from "./downloads";

const NOW = Date.parse("2026-11-25T12:00:00Z");
const DAY = 86_400_000;
const daysAgo = (n: number) => new Date(NOW - n * DAY).toISOString();

function meta(over: Partial<FileMeta> = {}): FileMeta {
  return {
    dateAdded: null,
    lastOpened: null,
    indexed: true,
    created: null,
    modified: null,
    contentType: null,
    whereFrom: null,
    ...over,
  };
}

function entry(label: string, over: Partial<Finding> = {}, m: Partial<FileMeta> = {}): Finding {
  const path = `/Users/you/Downloads/${label}`;
  return {
    id: `${DOWNLOADS_RULE}:${path}`,
    ruleId: DOWNLOADS_RULE,
    category: "downloads",
    risk: "manual",
    label,
    path,
    bytes: 1000,
    logicalBytes: 1000,
    files: 1,
    isDir: false,
    lastUsed: null,
    reason: "",
    action: "trash",
    command: null,
    selectedByDefault: false,
    blockers: [],
    group: null,
    meta: meta(m),
    ...over,
  };
}

const labels = (list: Finding[]) => list.map((f) => f.label);

describe("kinds", () => {
  it("reads the extension case-insensitively", () => {
    expect(extensionOf("Report.PDF")).toBe("pdf");
    expect(extensionOf("archive.tar.gz")).toBe("gz");
    expect(extensionOf(".hidden")).toBe("");
    expect(extensionOf("README")).toBe("");
    expect(extensionOf("trailing.")).toBe("");
  });

  it("uses the extension first, then the UTI, then folder/other", () => {
    expect(entryKind(entry("IMG_0001.HEIC"))).toBe("image");
    expect(entryKind(entry("clip.mov"))).toBe("video");
    expect(entryKind(entry("song.flac"))).toBe("audio");
    expect(entryKind(entry("invoice.pdf"))).toBe("document");
    expect(entryKind(entry("data.csv"))).toBe("document");
    expect(entryKind(entry("src.tar.gz"))).toBe("archive");
    expect(entryKind(entry("Setup.dmg"))).toBe("installer");
    expect(entryKind(entry("Thing.pkg", { isDir: true }))).toBe("installer");
    expect(entryKind(entry("Tool.app", { isDir: true }))).toBe("installer");
    expect(entryKind(entry("photo", {}, { contentType: "public.jpeg" }))).toBe("image");
    expect(entryKind(entry("disk", {}, { contentType: "com.apple.disk-image-udif" }))).toBe("installer");
    expect(entryKind(entry("project.v2", { isDir: true }, { contentType: "public.folder" }))).toBe("folder");
    expect(entryKind(entry("stuff", { isDir: true }, { contentType: null }))).toBe("folder");
    expect(entryKind(entry("dump.sql"))).toBe("other");
    expect(entryKind(entry("noext", {}, { contentType: "public.data" }))).toBe("other");
  });
});

describe("opened state and age", () => {
  it("keeps never-opened distinct from unknown", () => {
    expect(openedState(entry("a", {}, { lastOpened: daysAgo(3) }))).toBe("opened");
    expect(openedState(entry("b", {}, { indexed: true }))).toBe("never");
    expect(openedState(entry("c", {}, { indexed: false }))).toBe("unknown");
    expect(openedState(entry("d", { meta: null }))).toBe("unknown");
  });

  it("last activity is the newest of used, opened and added", () => {
    const f = entry("a", { lastUsed: daysAgo(40) }, { lastOpened: daysAgo(10), dateAdded: daysAgo(100) });
    expect(lastActivity(f)).toBe(NOW - 10 * DAY);
    expect(lastActivity(entry("b", { meta: null }))).toBeNull();
  });

  it("untouched-for counts whole windows and skips items without dates", () => {
    const f = entry("a", {}, { dateAdded: daysAgo(31) });
    expect(untouchedFor(f, 30, NOW)).toBe(true);
    expect(untouchedFor(f, 90, NOW)).toBe(false);
    const opened = entry("b", {}, { dateAdded: daysAgo(400), lastOpened: daysAgo(5) });
    expect(untouchedFor(opened, 30, NOW)).toBe(false);
    expect(untouchedFor(entry("c", { meta: null }), 30, NOW)).toBe(false);
    // A file modified inside the window counts as used even if never opened.
    const edited = entry("d", { lastUsed: daysAgo(2) }, { dateAdded: daysAgo(200) });
    expect(untouchedFor(edited, 30, NOW)).toBe(false);
  });
});

describe("source domain", () => {
  it("is the host of the first where-from URL", () => {
    expect(sourceDomain(entry("a", {}, { whereFrom: "https://www.example.com/files/a.pdf?sig=x" }))).toBe(
      "example.com",
    );
    expect(sourceDomain(entry("b", {}, { whereFrom: "https://dl.example.org/b" }))).toBe("dl.example.org");
    expect(sourceDomain(entry("c", {}, { whereFrom: "blob:https://example.com/123" }))).toBeNull();
    expect(sourceDomain(entry("d", {}, { whereFrom: "not a url" }))).toBeNull();
    expect(sourceDomain(entry("e"))).toBeNull();
  });
});

describe("sorting", () => {
  const big = entry("big.zip", { bytes: 9000 }, { dateAdded: daysAgo(5), lastOpened: daysAgo(1) });
  const old = entry("old.pdf", { bytes: 10 }, { dateAdded: daysAgo(300), lastOpened: daysAgo(200) });
  const never = entry("never.dmg", { bytes: 500 }, { dateAdded: daysAgo(50) });
  const neverOlder = entry("never-older.iso", { bytes: 500 }, { dateAdded: daysAgo(80) });
  const unknown = entry(
    "file 10.txt",
    { bytes: 500, lastUsed: daysAgo(20) },
    { indexed: false, dateAdded: daysAgo(20) },
  );
  const noDate = entry("file 9.txt", { bytes: 500, meta: null });
  const all = [big, old, never, neverOlder, unknown, noDate];

  it("size: largest first, ties by name", () => {
    expect(labels(sortEntries(all, "size"))).toEqual([
      "big.zip",
      "file 9.txt",
      "file 10.txt",
      "never-older.iso",
      "never.dmg",
      "old.pdf",
    ]);
  });

  it("date added: oldest first, unknown last", () => {
    expect(labels(sortEntries(all, "added"))).toEqual([
      "old.pdf",
      "never-older.iso",
      "never.dmg",
      "file 10.txt",
      "big.zip",
      "file 9.txt",
    ]);
  });

  it("last opened: never (oldest added first), unknown, then oldest opened", () => {
    expect(labels(sortEntries(all, "opened"))).toEqual([
      "never-older.iso",
      "never.dmg",
      "file 10.txt",
      "file 9.txt",
      "old.pdf",
      "big.zip",
    ]);
  });

  it("name: natural order", () => {
    expect(labels(sortEntries(all, "name"))).toEqual([
      "big.zip",
      "file 9.txt",
      "file 10.txt",
      "never-older.iso",
      "never.dmg",
      "old.pdf",
    ]);
  });

  it("does not mutate the input", () => {
    const copy = [...all];
    sortEntries(all, "size");
    expect(all).toEqual(copy);
  });
});

describe("filtering", () => {
  const photo = entry("beach.jpg", {}, { dateAdded: daysAgo(400), whereFrom: "https://photos.example.com/x" });
  const pdf = entry("invoice.pdf", {}, { dateAdded: daysAgo(40) });
  const fresh = entry("Setup.dmg", {}, { dateAdded: daysAgo(1) });
  const folder = entry("project", { isDir: true }, { dateAdded: daysAgo(100), contentType: "public.folder" });
  const all = [photo, pdf, fresh, folder];

  it("by kind, age and search together", () => {
    expect(labels(filterEntries(all, { kind: "all", age: "any", query: "" }, NOW))).toEqual(labels(all));
    expect(labels(filterEntries(all, { kind: "image", age: "any", query: "" }, NOW))).toEqual(["beach.jpg"]);
    expect(labels(filterEntries(all, { kind: "folder", age: "any", query: "" }, NOW))).toEqual(["project"]);
    expect(labels(filterEntries(all, { kind: "all", age: 30, query: "" }, NOW))).toEqual([
      "beach.jpg",
      "invoice.pdf",
      "project",
    ]);
    expect(labels(filterEntries(all, { kind: "all", age: 90, query: "" }, NOW))).toEqual(["beach.jpg", "project"]);
    expect(labels(filterEntries(all, { kind: "all", age: 365, query: "" }, NOW))).toEqual(["beach.jpg"]);
    expect(labels(filterEntries(all, { kind: "all", age: "any", query: "INVOICE" }, NOW))).toEqual(["invoice.pdf"]);
    expect(labels(filterEntries(all, { kind: "all", age: "any", query: "photos.example" }, NOW))).toEqual([
      "beach.jpg",
    ]);
    expect(labels(filterEntries(all, { kind: "all", age: "any", query: "installer" }, NOW))).toEqual(["Setup.dmg"]);
    expect(filterEntries(all, { kind: "document", age: 365, query: "" }, NOW)).toEqual([]);
  });
});

describe("selection", () => {
  const ids = ["a", "b", "c", "d", "e"];

  it("ranges go either way and include both ends", () => {
    expect(rangeIds(ids, "b", "d")).toEqual(["b", "c", "d"]);
    expect(rangeIds(ids, "d", "b")).toEqual(["b", "c", "d"]);
    expect(rangeIds(ids, "c", "c")).toEqual(["c"]);
    expect(rangeIds(ids, null, "c")).toEqual(["c"]);
    // An anchor that the filter has hidden since.
    expect(rangeIds(ids, "gone", "c")).toEqual(["c"]);
    expect(rangeIds(ids, "a", "gone")).toEqual([]);
  });

  it("totals only selectable entries", () => {
    const a = entry("a.zip", { bytes: 100 });
    const b = entry("b.zip", { bytes: 250 });
    const blocked = entry("c.crdownload", { bytes: 999, blockers: ["Download not finished"] });
    const { selected, bytes } = selectionSummary([a, b, blocked], new Set([a.id, blocked.id]));
    expect(labels(selected)).toEqual(["a.zip"]);
    expect(bytes).toBe(100);
  });
});

describe("main scan badges", () => {
  function mainFinding(ruleId: string, path: string, reason: string): Finding {
    return { ...entry("x"), id: `${ruleId}:${path}`, ruleId, path, label: path.split("/").pop() ?? "", reason };
  }

  it("flags entries and folders holding flagged items", () => {
    const dmg = entry("Setup.dmg");
    const backups = entry("backups", { isDir: true });
    const zip = entry("tool.zip");
    const plain = entry("notes.txt");
    const main = {
      findings: [
        mainFinding("downloads.installers", dmg.path as string, "Installer older than 7 days"),
        mainFinding("downloads.versioned-siblings", "/Users/you/Downloads/backups/db_01", "Older copy"),
        mainFinding("downloads.versioned-siblings", "/Users/you/Downloads/backups/db_02", "Older copy"),
        mainFinding("downloads.redundant-archives", zip.path as string, "Extracted next to it"),
        mainFinding("downloads.old-files", plain.path as string, "Not opened for 40 days"),
        mainFinding("cache.npm", "/Users/you/Downloads/backups-but-not-inside", "x"),
      ],
    } as unknown as ScanReport;
    const flags = mainScanFlags([dmg, backups, zip, plain], main);
    expect(flags.get(dmg.path as string)?.map((f) => f.label)).toEqual(["Old installer"]);
    expect(flags.get(zip.path as string)?.map((f) => f.label)).toEqual(["Already extracted"]);
    const inside = flags.get(backups.path as string);
    expect(inside?.map((f) => f.label)).toEqual(["Older versions inside"]);
    expect(inside?.[0]?.reason).toBe("db_01: Older copy (and 1 more)");
    expect(flags.has(plain.path as string)).toBe(false);
    expect(mainScanFlags([dmg], null).size).toBe(0);
  });
});
