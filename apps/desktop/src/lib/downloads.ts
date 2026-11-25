// Pure logic for the Downloads tab: kinds, "opened" state and age, sorting, filtering, range
// selection and the badges the main scan adds. The entries are `downloads.browse` findings
// (ARCHITECTURE.md §4: one per top-level item of ~/Downloads, with Spotlight metadata in `meta`).

import type { Finding } from "../bindings/Finding";
import type { ScanReport } from "../bindings/ScanReport";
import { isInside, isSelectable } from "./findings";

/** The browse-only rule the Downloads tab scans by id. */
export const DOWNLOADS_RULE = "downloads.browse";

const DAY_MS = 86_400_000;

export type EntryKind = "image" | "video" | "audio" | "document" | "archive" | "installer" | "folder" | "other";

export const KIND_LABEL: Record<EntryKind, string> = {
  image: "Image",
  video: "Video",
  audio: "Audio",
  document: "Document",
  archive: "Archive",
  installer: "Installer",
  folder: "Folder",
  other: "Other",
};

export type KindFilter = EntryKind | "all";

export const KIND_FILTERS: { value: KindFilter; label: string }[] = [
  { value: "all", label: "All kinds" },
  { value: "image", label: "Images" },
  { value: "video", label: "Video" },
  { value: "audio", label: "Audio" },
  { value: "document", label: "Documents & PDF" },
  { value: "archive", label: "Archives" },
  { value: "installer", label: "Installers (dmg, pkg, iso)" },
  { value: "folder", label: "Folders" },
  { value: "other", label: "Other" },
];

const EXTENSIONS: Record<EntryKind, string[]> = {
  image: [
    "jpg",
    "jpeg",
    "png",
    "gif",
    "heic",
    "heif",
    "webp",
    "avif",
    "tif",
    "tiff",
    "bmp",
    "svg",
    "psd",
    "ico",
    "icns",
    "raw",
    "cr2",
    "cr3",
    "nef",
    "arw",
    "dng",
  ],
  video: ["mp4", "mov", "m4v", "avi", "mkv", "webm", "wmv", "flv", "mpg", "mpeg", "3gp"],
  audio: ["mp3", "m4a", "wav", "aif", "aiff", "flac", "ogg", "oga", "aac", "opus", "wma"],
  document: [
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "key",
    "pages",
    "numbers",
    "odt",
    "ods",
    "odp",
    "txt",
    "rtf",
    "rtfd",
    "md",
    "csv",
    "tsv",
    "json",
    "xml",
    "html",
    "htm",
    "epub",
    "log",
    "yaml",
    "yml",
  ],
  archive: ["zip", "tar", "gz", "tgz", "bz2", "tbz", "xz", "txz", "7z", "rar", "zst", "lz4"],
  installer: ["dmg", "pkg", "mpkg", "iso", "xip", "app"],
  folder: [],
  other: [],
};

const KIND_BY_EXTENSION = new Map<string, EntryKind>(
  (Object.entries(EXTENSIONS) as [EntryKind, string[]][]).flatMap(([kind, exts]) =>
    exts.map((ext): [string, EntryKind] => [ext, kind]),
  ),
);

/** Uniform Type Identifier prefixes (Spotlight `kMDItemContentType`), checked in order. */
const KIND_BY_UTI: [string, EntryKind][] = [
  ["com.apple.disk-image", "installer"],
  ["com.apple.installer-package", "installer"],
  ["public.iso-image", "installer"],
  ["com.apple.xip-archive", "installer"],
  ["com.apple.application", "installer"],
  ["public.image", "image"],
  ["public.jpeg", "image"],
  ["public.png", "image"],
  ["public.heic", "image"],
  ["public.camera-raw", "image"],
  ["public.movie", "video"],
  ["public.video", "video"],
  ["public.mpeg-4", "video"],
  ["com.apple.quicktime", "video"],
  ["public.audio", "audio"],
  ["public.mp3", "audio"],
  ["com.apple.m4a", "audio"],
  ["com.adobe.pdf", "document"],
  ["public.plain-text", "document"],
  ["public.text", "document"],
  ["public.rtf", "document"],
  ["org.openxmlformats.", "document"],
  ["com.microsoft.word", "document"],
  ["com.microsoft.excel", "document"],
  ["com.microsoft.powerpoint", "document"],
  ["com.apple.iwork", "document"],
  ["com.apple.keynote", "document"],
  ["public.zip-archive", "archive"],
  ["public.archive", "archive"],
  ["public.tar-archive", "archive"],
  ["org.gnu.gnu-zip", "archive"],
  ["org.7-zip", "archive"],
  ["com.rarlab", "archive"],
  ["public.folder", "folder"],
];

/** Lower-case extension of a file name ("Report.PDF" → "pdf"); "" when there is none. */
export function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1 ? name.slice(dot + 1).toLowerCase() : "";
}

/** What an entry is: by extension first (deterministic), then by its UTI, then folder/other. */
export function entryKind(f: Finding): EntryKind {
  const byExt = KIND_BY_EXTENSION.get(extensionOf(f.label));
  if (byExt) return byExt;
  const uti = f.meta?.contentType ?? "";
  const byUti = KIND_BY_UTI.find(([prefix]) => uti.startsWith(prefix));
  if (byUti) return byUti[1];
  return f.isDir ? "folder" : "other";
}

export type OpenedState = "opened" | "never" | "unknown";

/**
 * "never" only when Spotlight knows the item and has no last-used date; without a Spotlight
 * record (unindexed volume, symlink) the open date is "unknown".
 */
export function openedState(f: Finding): OpenedState {
  if (f.meta?.lastOpened) return "opened";
  return f.meta?.indexed ? "never" : "unknown";
}

function time(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  return Number.isNaN(t) ? null : t;
}

/**
 * Newest sign of use: opened, added, or anything inside modified (`lastUsed` from the core is
 * already the newest of the three); `null` when no date is known.
 */
export function lastActivity(f: Finding): number | null {
  const candidates = [time(f.lastUsed), time(f.meta?.lastOpened), time(f.meta?.dateAdded)].filter(
    (t): t is number => t !== null,
  );
  return candidates.length > 0 ? Math.max(...candidates) : null;
}

export type AgeFilter = "any" | 30 | 90 | 365;

export const AGE_FILTERS: { value: AgeFilter; label: string }[] = [
  { value: "any", label: "Any time" },
  { value: 30, label: "30 days" },
  { value: 90, label: "90 days" },
  { value: 365, label: "1 year" },
];

/** Not opened, added or modified for `days` days. Items without any date are left out. */
export function untouchedFor(f: Finding, days: number, now: number): boolean {
  const last = lastActivity(f);
  return last !== null && last <= now - days * DAY_MS;
}

/** "example.com" from the first where-from URL; `null` for none or for blob/data URLs. */
export function sourceDomain(f: Finding): string | null {
  const url = f.meta?.whereFrom;
  if (!url) return null;
  try {
    const { hostname, protocol } = new URL(url);
    if (!hostname || !/^(https?|ftp):$/.test(protocol)) return null;
    return hostname.replace(/^www\./, "");
  } catch {
    return null;
  }
}

export type SortKey = "size" | "added" | "opened" | "name";

export const SORTS: { value: SortKey; label: string; hint: string }[] = [
  { value: "size", label: "Size", hint: "Largest first" },
  { value: "added", label: "Date added", hint: "Oldest first" },
  { value: "opened", label: "Last opened", hint: "Never opened first, then oldest" },
  { value: "name", label: "Name", hint: "A to Z" },
];

const collator = new Intl.Collator("en", { numeric: true, sensitivity: "base" });

function byName(a: Finding, b: Finding): number {
  return collator.compare(a.label, b.label);
}

/** Ascending by `t`, unknown (`null`) last. */
function byTime(a: number | null, b: number | null): number {
  if (a === null || b === null) return a === null ? (b === null ? 0 : 1) : -1;
  return a - b;
}

const OPENED_RANK: Record<OpenedState, number> = { never: 0, unknown: 1, opened: 2 };

/**
 * A sorted copy. Size: largest first. Date added: oldest first. Last opened: never-opened first
 * (oldest added first among them), then unknown (least recently used first), then the oldest
 * opened. Name: natural A–Z. Ties fall back to the name.
 */
export function sortEntries(entries: Finding[], key: SortKey): Finding[] {
  const compare = (a: Finding, b: Finding): number => {
    switch (key) {
      case "size":
        return b.bytes - a.bytes;
      case "added":
        return byTime(time(a.meta?.dateAdded), time(b.meta?.dateAdded));
      case "opened": {
        const state = openedState(a);
        return OPENED_RANK[state] - OPENED_RANK[openedState(b)] || byOpened(a, b, state);
      }
      case "name":
        return 0;
    }
  };
  return [...entries].sort((a, b) => compare(a, b) || byName(a, b));
}

/** Within one opened state. */
function byOpened(a: Finding, b: Finding, state: OpenedState): number {
  switch (state) {
    case "never":
      return byTime(time(a.meta?.dateAdded), time(b.meta?.dateAdded));
    case "unknown":
      return byTime(lastActivity(a), lastActivity(b));
    case "opened":
      return byTime(time(a.meta?.lastOpened), time(b.meta?.lastOpened));
  }
}

export interface EntryFilter {
  kind: KindFilter;
  age: AgeFilter;
  query: string;
}

export function matchesEntry(f: Finding, filter: EntryFilter, now: number): boolean {
  if (filter.kind !== "all" && entryKind(f) !== filter.kind) return false;
  if (filter.age !== "any" && !untouchedFor(f, filter.age, now)) return false;
  const q = filter.query.trim().toLowerCase();
  if (!q) return true;
  return [f.label, sourceDomain(f) ?? "", KIND_LABEL[entryKind(f)]].some((s) => s.toLowerCase().includes(q));
}

export function filterEntries(entries: Finding[], filter: EntryFilter, now: number): Finding[] {
  return entries.filter((f) => matchesEntry(f, filter, now));
}

/** Ids from `anchor` to `target` inclusive, in list order (either direction); just `target` without an anchor in the list. */
export function rangeIds(orderedIds: string[], anchor: string | null, target: string): string[] {
  const end = orderedIds.indexOf(target);
  if (end < 0) return [];
  const start = anchor === null ? -1 : orderedIds.indexOf(anchor);
  if (start < 0) return [target];
  const [from, to] = start <= end ? [start, end] : [end, start];
  return orderedIds.slice(from, to + 1);
}

/** Selected, selectable entries and their total size (top-level entries never nest). */
export function selectionSummary(entries: Finding[], selection: ReadonlySet<string>) {
  const selected = entries.filter((f) => selection.has(f.id) && isSelectable(f));
  return { selected, bytes: selected.reduce((sum, f) => sum + f.bytes, 0) };
}

/** Main-scan rules whose findings are worth a badge on a Downloads entry. */
const FLAG_RULES: Record<string, { exact: string; inside: string }> = {
  "downloads.installers": { exact: "Old installer", inside: "Old installers inside" },
  "downloads.redundant-archives": { exact: "Already extracted", inside: "Extracted archives inside" },
  "downloads.versioned-siblings": { exact: "Older version", inside: "Older versions inside" },
};

export interface EntryFlag {
  label: string;
  /** The main finding's reason (tooltip). */
  reason: string;
}

/**
 * Badges from the main scan: an entry flagged itself by an installer / extracted-archive / older
 * version rule ("Old installer"), or a folder holding such findings ("Older versions inside").
 * Keyed by entry path; one badge per rule.
 */
export function mainScanFlags(entries: Finding[], main: ScanReport | null): Map<string, EntryFlag[]> {
  const out = new Map<string, EntryFlag[]>();
  if (!main) return out;
  const flagged = main.findings.filter((f) => f.path !== null && f.ruleId in FLAG_RULES);
  for (const entry of entries) {
    const path = entry.path;
    if (!path) continue;
    const flags: EntryFlag[] = [];
    for (const [ruleId, labels] of Object.entries(FLAG_RULES)) {
      const ofRule = flagged.filter((f) => f.ruleId === ruleId);
      const exact = ofRule.find((f) => f.path === path);
      if (exact) {
        flags.push({ label: labels.exact, reason: exact.reason });
        continue;
      }
      const inside = ofRule.filter((f) => isInside(f.path as string, path));
      if (inside.length > 0) {
        const first = inside[0] as Finding;
        const more = inside.length > 1 ? ` (and ${inside.length - 1} more)` : "";
        flags.push({ label: labels.inside, reason: `${first.label}: ${first.reason}${more}` });
      }
    }
    if (flags.length > 0) out.set(path, flags);
  }
  return out;
}
