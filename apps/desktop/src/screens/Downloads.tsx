// Downloads tab: everything at the top of ~/Downloads (the browse-only `downloads.browse` scan),
// with QuickLook thumbnails, sorting by size / date added / last opened / name, filters by kind,
// age and text, multi-select and the shared Confirm sheet (Move to Trash by default).
// Space or the eye button opens Quick Look for the focused row.

import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { Finding } from "../bindings/Finding";
import type { ScanReport } from "../bindings/ScanReport";
import {
  DocumentIcon,
  DownloadIcon,
  EyeIcon,
  FolderStackIcon,
  RevealIcon,
  SearchIcon,
  TrashIcon,
  WarningIcon,
} from "../components/icons";
import { ScreenFrame } from "../components/layout";
import { Button, Callout, Checkbox, Chip, cx, EmptyState, IconButton, Segmented, Spinner, Tip } from "../components/ui";
import {
  AGE_FILTERS,
  type AgeFilter,
  type EntryFlag,
  type EntryKind,
  entryKind,
  filterEntries,
  KIND_FILTERS,
  KIND_LABEL,
  type KindFilter,
  mainScanFlags,
  openedState,
  rangeIds,
  SORTS,
  type SortKey,
  selectionSummary,
  sortEntries,
  sourceDomain,
} from "../lib/downloads";
import { isSelectable } from "../lib/findings";
import { errorMessage, formatBytes, formatCount, relativeTime, tildify } from "../lib/format";
import { useApp } from "../state";

export function Downloads() {
  const { downloads, downloadsScanning, downloadsError, scanDownloads } = useApp();
  const started = useRef(false);

  // First visit: scan Downloads (a later visit keeps the list; Rescan refreshes it).
  useEffect(() => {
    if (started.current || downloads || downloadsScanning) return;
    started.current = true;
    scanDownloads();
  }, [downloads, downloadsScanning, scanDownloads]);

  if (!downloads) {
    return (
      <ScreenFrame title="Downloads" actions={<RescanButton />}>
        {downloadsError ? (
          <div className="px-5 pt-4">
            <Callout tone="critical" title="Couldn't list Downloads">
              {downloadsError}
            </Callout>
          </div>
        ) : (
          <EmptyState icon={<Spinner size={22} />} title="Reading Downloads…">
            Sizing every item and asking Spotlight when it was added and last opened.
          </EmptyState>
        )}
      </ScreenFrame>
    );
  }
  return <DownloadsList report={downloads} />;
}

function RescanButton() {
  const { downloads, downloadsScanning, scanDownloads } = useApp();
  return (
    <Button variant={downloads ? "secondary" : "primary"} onClick={scanDownloads} disabled={downloadsScanning}>
      {downloadsScanning ? <Spinner size={12} /> : null}
      {downloadsScanning ? "Reading…" : downloads ? "Rescan" : "Scan"}
    </Button>
  );
}

const COLUMNS = "grid grid-cols-[16px_40px_minmax(0,1fr)_76px_92px_104px_52px] items-center gap-x-3";

function DownloadsList({ report }: { report: ScanReport }) {
  const { report: main, downloadsSelection: selection, setDownloadsSelection, downloadsError } = useApp();
  const [sort, setSort] = useState<SortKey>("size");
  const [kind, setKind] = useState<KindFilter>("all");
  const [age, setAge] = useState<AgeFilter>("any");
  const [query, setQuery] = useState("");
  const [focused, setFocused] = useState<string | null>(null);
  const [anchor, setAnchor] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // "Not opened for N days" is measured from when the list was read.
  const now = useMemo(() => Date.parse(report.startedAt) || Date.now(), [report]);
  const entries = report.findings;
  const visible = useMemo(
    () => sortEntries(filterEntries(entries, { kind, age, query }, now), sort),
    [entries, kind, age, query, now, sort],
  );
  const flags = useMemo(() => mainScanFlags(entries, main), [entries, main]);
  const total = useMemo(() => entries.reduce((sum, f) => sum + f.bytes, 0), [entries]);
  const selectableVisible = visible.filter(isSelectable);
  const anyVisibleSelected = visible.some((f) => selection.has(f.id));

  const select = (ids: string[], on: boolean) =>
    setDownloadsSelection((prev) => {
      const next = new Set(prev);
      for (const id of ids) {
        if (on) next.add(id);
        else next.delete(id);
      }
      return next;
    });

  /** Click toggles one row; shift-click selects the visible range from the last clicked row. */
  const pick = (f: Finding, shift: boolean) => {
    setFocused(f.id);
    if (shift && anchor) {
      const byId = new Map(visible.map((v) => [v.id, v]));
      const ids = rangeIds(
        visible.map((v) => v.id),
        anchor,
        f.id,
      ).filter((id) => {
        const v = byId.get(id);
        return v !== undefined && isSelectable(v);
      });
      select(ids, true);
    } else if (isSelectable(f)) {
      select([f.id], !selection.has(f.id));
    }
    setAnchor(f.id);
  };

  const quickLook = (f: Finding) => {
    setFocused(f.id);
    if (f.path) api.quickLook(f.path).catch((e) => console.warn(errorMessage(e)));
  };

  // Keyboard: ↑/↓ move the focus, Space opens Quick Look, ⌘A selects every visible item, ⌘F searches.
  // Re-registered every render so the handler always sees the current list and focus.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const typing = target?.closest("input, textarea, select, [contenteditable=true]") !== null;
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        return;
      }
      if (typing || document.querySelector("[role=dialog]")) return;
      if ((e.metaKey || e.ctrlKey) && e.key === "a") {
        e.preventDefault();
        select(
          selectableVisible.map((f) => f.id),
          true,
        );
        return;
      }
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const index = visible.findIndex((f) => f.id === focused);
        const next = visible[Math.max(0, Math.min(visible.length - 1, index + (e.key === "ArrowDown" ? 1 : -1)))];
        if (next) {
          setFocused(next.id);
          listRef.current?.querySelector(`[data-id="${CSS.escape(next.id)}"]`)?.scrollIntoView({ block: "nearest" });
        }
        return;
      }
      if (e.key === " " && target?.closest("button") === null) {
        const f = visible.find((v) => v.id === focused);
        if (f) {
          e.preventDefault();
          quickLook(f);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  return (
    <ScreenFrame
      title="Downloads"
      subtitle={`${formatCount(entries.length, "item")} · ${formatBytes(total)} · read ${relativeTime(report.startedAt)}`}
      actions={<RescanButton />}
      filters={
        <>
          <label className="flex h-7 items-center gap-1.5 text-[12px] text-fg-2">
            <span className="sr-only">Kind</span>
            <select
              value={kind}
              onChange={(e) => setKind(e.target.value as KindFilter)}
              aria-label="Filter by kind"
              className="h-7 rounded-md border border-line bg-surface px-2 text-[12.5px] text-fg outline-none focus:border-accent"
            >
              {KIND_FILTERS.map((k) => (
                <option key={k.value} value={k.value}>
                  {k.label}
                </option>
              ))}
            </select>
          </label>
          <span className="text-[12px] text-fg-2">Not opened for</span>
          <Segmented<AgeFilter> label="Not opened for" value={age} onChange={setAge} options={AGE_FILTERS} />
          <label className="relative ml-1 flex h-7 min-w-0 flex-1 basis-36 items-center">
            <SearchIcon className="pointer-events-none absolute left-2 text-fg-3" size={14} />
            <input
              ref={searchRef}
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Filter"
              aria-label="Filter downloads"
              className="h-7 w-full rounded-md border border-line bg-surface pr-2 pl-7 text-[12.5px] outline-none placeholder:text-fg-3 focus:border-accent"
            />
          </label>
          <Button
            size="sm"
            variant="ghost"
            disabled={selectableVisible.length === 0}
            onClick={() =>
              select(
                selectableVisible.map((f) => f.id),
                true,
              )
            }
          >
            Select All
          </Button>
          <Button
            size="sm"
            variant="ghost"
            disabled={!anyVisibleSelected}
            onClick={() =>
              select(
                visible.map((f) => f.id),
                false,
              )
            }
          >
            Deselect All
          </Button>
        </>
      }
      footer={<SelectionBar entries={entries} visible={visible} />}
    >
      {downloadsError && (
        <div className="px-5 pt-3">
          <Callout tone="critical" title="The last rescan failed">
            {downloadsError}. The list below is from the scan before.
          </Callout>
        </div>
      )}
      <div
        className={cx(
          COLUMNS,
          "sticky top-0 z-10 h-8 border-b border-line bg-bg/95 px-5 text-[11.5px] font-medium text-fg-2 backdrop-blur",
        )}
      >
        <span />
        <span />
        <SortHeader label="Name" value="name" sort={sort} setSort={setSort} />
        <SortHeader label="Size" value="size" sort={sort} setSort={setSort} align="right" />
        <SortHeader label="Added" value="added" sort={sort} setSort={setSort} />
        <SortHeader label="Last opened" value="opened" sort={sort} setSort={setSort} />
        <span />
      </div>
      <div ref={listRef} className="pb-6">
        {entries.length === 0 ? (
          <EmptyState icon={<DownloadIcon size={28} />} title="Downloads is empty">
            Nothing at the top of your Downloads folder.
          </EmptyState>
        ) : visible.length === 0 ? (
          <EmptyState icon={<SearchIcon size={28} />} title="Nothing matches">
            Try another kind, age or search term.
          </EmptyState>
        ) : (
          visible.map((f) => (
            <EntryRow
              key={f.id}
              entry={f}
              flags={f.path ? (flags.get(f.path) ?? []) : []}
              selected={selection.has(f.id)}
              focused={focused === f.id}
              onPick={(shift) => pick(f, shift)}
              onQuickLook={() => quickLook(f)}
            />
          ))
        )}
        {report.errors.length > 0 && (
          <div className="px-5 pt-4">
            <Callout tone="warn" title={`${formatCount(report.errors.length, "location")} couldn't be read`}>
              <ul className="space-y-0.5">
                {report.errors.slice(0, 8).map((e) => (
                  <li key={`${e.path}:${e.message}`} className="font-mono text-[11px]">
                    {tildify(e.path)} — {e.message}
                  </li>
                ))}
              </ul>
            </Callout>
          </div>
        )}
      </div>
    </ScreenFrame>
  );
}

function SortHeader({
  label,
  value,
  sort,
  setSort,
  align = "left",
}: {
  label: string;
  value: SortKey;
  sort: SortKey;
  setSort: (s: SortKey) => void;
  align?: "left" | "right";
}) {
  const active = sort === value;
  const hint = SORTS.find((s) => s.value === value)?.hint;
  return (
    <button
      type="button"
      onClick={() => setSort(value)}
      aria-pressed={active}
      title={`Sort by ${label.toLowerCase()} (${hint?.toLowerCase()})`}
      className={cx(
        "flex min-w-0 items-center gap-1 truncate rounded px-0.5 hover:text-fg",
        align === "right" && "justify-end",
        active && "text-fg",
      )}
    >
      <span className="truncate">{label}</span>
      {active && <span aria-hidden="true">{value === "size" ? "↓" : "↑"}</span>}
    </button>
  );
}

const dayFmt = new Intl.DateTimeFormat("en", { month: "short", day: "numeric" });
const dayYearFmt = new Intl.DateTimeFormat("en", { month: "short", day: "numeric", year: "numeric" });

/** "Oct 2" this year, "Oct 2, 2024" before. */
function formatDay(iso: string): string {
  const d = new Date(iso);
  return (d.getFullYear() === new Date().getFullYear() ? dayFmt : dayYearFmt).format(d);
}

function EntryRow({
  entry: f,
  flags,
  selected,
  focused,
  onPick,
  onQuickLook,
}: {
  entry: Finding;
  flags: EntryFlag[];
  selected: boolean;
  focused: boolean;
  onPick: (shift: boolean) => void;
  onQuickLook: () => void;
}) {
  const selectable = isSelectable(f);
  const kind = entryKind(f);
  const domain = sourceDomain(f);
  const opened = openedState(f);
  const added = f.meta?.dateAdded ?? null;
  const blocked = f.blockers.length > 0;

  return (
    // biome-ignore lint/a11y/useKeyWithClickEvents: rows are driven by the window-level keyboard handler (↑/↓, Space, ⌘A)
    // biome-ignore lint/a11y/noStaticElementInteractions: a click picks the row; the checkbox is the accessible control
    <div
      data-id={f.id}
      onClick={(e) => {
        if ((e.target as HTMLElement).closest("button, input, a")) return;
        onPick(e.shiftKey);
      }}
      className={cx(
        COLUMNS,
        "border-b border-line px-5 py-1.5 [contain-intrinsic-size:auto_53px] [content-visibility:auto]",
        selected ? "bg-accent-soft/60" : "hover:bg-surface-2/60",
        focused && "shadow-[inset_2px_0_0_var(--accent)]",
      )}
    >
      <Tip content={blocked ? f.blockers.join("; ") : undefined}>
        <span className="inline-flex">
          <Checkbox
            label={`Select ${f.label}`}
            checked={selected && selectable}
            disabled={!selectable}
            onChange={() => onPick(false)}
          />
        </span>
      </Tip>

      <Thumb entry={f} kind={kind} />

      <div className="min-w-0">
        <div className="flex min-w-0 items-center gap-1.5">
          <span
            className={cx("selectable truncate text-[13px] font-medium", !selectable && "text-fg-2")}
            title={f.label}
          >
            {f.label}
          </span>
          <Chip>{KIND_LABEL[kind]}</Chip>
        </div>
        <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11.5px] text-fg-3">
          {flags.map((flag) => (
            <Tip key={flag.label} content={flag.reason}>
              <span className="inline-flex h-[17px] shrink-0 items-center rounded bg-warn-soft px-1.5 text-[11px] font-semibold text-warn-text">
                {flag.label}
              </span>
            </Tip>
          ))}
          {blocked ? (
            <span className="flex min-w-0 items-center gap-1 text-warn-text">
              <WarningIcon size={12} className="shrink-0 text-warn" />
              <span className="truncate">{f.blockers.join("; ")}</span>
            </span>
          ) : (
            <span className="selectable truncate" title={f.meta?.whereFrom ?? undefined}>
              {[
                f.isDir && kind !== "installer" ? formatCount(Number(f.files), "file") : null,
                domain ? `from ${domain}` : null,
              ]
                .filter(Boolean)
                .join(" · ") || tildify(f.path ?? "")}
            </span>
          )}
        </div>
      </div>

      <span className={cx("text-right text-[12.5px] font-semibold tabular-nums", !selectable && "text-fg-2")}>
        {formatBytes(f.bytes)}
      </span>

      <Tip content={added ? relativeTime(added) : "Unknown"}>
        <span className="truncate text-[12px] text-fg-2 tabular-nums">{added ? formatDay(added) : "—"}</span>
      </Tip>

      <Tip
        content={
          opened === "opened" && f.meta?.lastOpened
            ? relativeTime(f.meta.lastOpened)
            : opened === "never"
              ? "Spotlight has no record of it being opened"
              : "Spotlight doesn't know this item (not indexed), so the last-opened date is unknown"
        }
      >
        <span className={cx("truncate text-[12px] tabular-nums", opened === "opened" ? "text-fg-2" : "text-fg-3")}>
          {opened === "opened" && f.meta?.lastOpened
            ? formatDay(f.meta.lastOpened)
            : opened === "never"
              ? "Never opened"
              : "Unknown"}
        </span>
      </Tip>

      <div className="flex items-center justify-end gap-0.5">
        <IconButton label="Quick Look (Space)" className="size-6" onClick={onQuickLook}>
          <EyeIcon size={14} />
        </IconButton>
        <IconButton
          label="Reveal in Finder"
          className="size-6"
          onClick={() => {
            if (f.path) api.revealInFinder(f.path).catch((e) => console.warn(e));
          }}
        >
          <RevealIcon size={13} />
        </IconButton>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Thumbnails: requested lazily for rows near the viewport, four at a time, cached per file version.

const THUMB_POINTS = 40;
const MAX_IN_FLIGHT = 4;
const thumbCache = new Map<string, Promise<string | null>>();
const waiting: (() => void)[] = [];
let inFlight = 0;

async function withSlot<T>(run: () => Promise<T>): Promise<T> {
  if (inFlight >= MAX_IN_FLIGHT) await new Promise<void>((resolve) => waiting.push(resolve));
  inFlight += 1;
  try {
    return await run();
  } finally {
    inFlight -= 1;
    waiting.shift()?.();
  }
}

function loadThumbnail(f: Finding): Promise<string | null> {
  const path = f.path;
  if (!path) return Promise.resolve(null);
  const key = `${path}|${f.meta?.modified ?? ""}|${f.bytes}`;
  let pending = thumbCache.get(key);
  if (!pending) {
    pending = withSlot(() => api.thumbnail(path, THUMB_POINTS)).then(
      (blob) => URL.createObjectURL(blob),
      () => null,
    );
    thumbCache.set(key, pending);
  }
  return pending;
}

function KindIcon({ kind }: { kind: EntryKind }) {
  return kind === "folder" ? <FolderStackIcon size={20} /> : <DocumentIcon size={20} />;
}

function Thumb({ entry, kind }: { entry: Finding; kind: EntryKind }) {
  const ref = useRef<HTMLDivElement>(null);
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    let cancelled = false;
    const observer = new IntersectionObserver(
      (records) => {
        if (!records.some((r) => r.isIntersecting)) return;
        observer.disconnect();
        loadThumbnail(entry).then((u) => !cancelled && setUrl(u));
      },
      { rootMargin: "240px 0px" },
    );
    observer.observe(el);
    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, [entry]);

  return (
    <div ref={ref} className="flex size-10 items-center justify-center overflow-hidden rounded-md text-fg-3">
      {url ? (
        <img src={url} alt="" draggable={false} className="max-h-10 max-w-10 object-contain" />
      ) : (
        <KindIcon kind={kind} />
      )}
    </div>
  );
}

function SelectionBar({ entries, visible }: { entries: Finding[]; visible: Finding[] }) {
  const { downloadsSelection, setDownloadsSelection, openConfirm } = useApp();
  const { selected, bytes } = selectionSummary(entries, downloadsSelection);
  const visibleIds = new Set(visible.map((f) => f.id));
  const hidden = selected.filter((f) => !visibleIds.has(f.id)).length;
  const details = [formatCount(selected.length, "item"), hidden > 0 ? `${hidden} hidden by the filter` : null].filter(
    Boolean,
  );

  return (
    <footer className="flex shrink-0 items-center gap-3 border-t border-line bg-surface px-5 py-2.5">
      {selected.length === 0 ? (
        <div className="text-[12.5px] text-fg-2">
          Nothing selected. Click rows to pick them, Shift-click for a range, Space for Quick Look.
        </div>
      ) : (
        <div className="min-w-0">
          <div className="flex items-baseline gap-1.5">
            <span className="text-[13px] text-fg-2">Selected:</span>
            <span className="text-[16px] font-semibold">{formatBytes(bytes)}</span>
          </div>
          <div className="truncate text-[11.5px] text-fg-2">{details.join(" · ")}</div>
        </div>
      )}
      <span className="flex-1" />
      <Button variant="ghost" onClick={() => setDownloadsSelection(() => new Set())} disabled={selected.length === 0}>
        Clear
      </Button>
      <Button variant="primary" onClick={() => openConfirm(selected.map((f) => f.id))} disabled={selected.length === 0}>
        <TrashIcon size={14} />
        Move to Trash…
      </Button>
    </footer>
  );
}
