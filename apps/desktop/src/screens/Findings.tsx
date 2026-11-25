import { type RefObject, useEffect, useId, useMemo, useRef, useState } from "react";
import { api } from "../api";
import type { Finding } from "../bindings/Finding";
import type { Risk } from "../bindings/Risk";
import type { ScanReport } from "../bindings/ScanReport";
import {
  ChevronIcon,
  FolderStackIcon,
  ListIcon,
  RevealIcon,
  SearchIcon,
  TerminalIcon,
  WarningIcon,
} from "../components/icons";
import { ScanButton, ScreenFrame } from "../components/layout";
import {
  Button,
  Callout,
  Checkbox,
  Chip,
  cx,
  EmptyState,
  IconButton,
  RiskBadge,
  Segmented,
  Tip,
} from "../components/ui";
import {
  browserStorage,
  type CollapseState,
  categoryFoldId,
  defaultSetCollapsed,
  type Foldable,
  foldables,
  foldSummary,
  isCollapsed,
  loadOverrides,
  saveOverrides,
  setCollapsed,
  setFoldId,
} from "../lib/collapse";
import {
  actionChip,
  CATEGORY_LABEL,
  type CategorySection,
  dedupedTotal,
  groupFindings,
  isBulkSelectable,
  isSelectable,
  matchesQuery,
  RISK_META,
  RISKS,
  setTitle,
  showLogical,
} from "../lib/findings";
import { clearFindingsQuery, peekFindingsQuery } from "../lib/findingsLink";
import { formatBytes, formatCount, relativeTime, tildify } from "../lib/format";
import { useApp } from "../state";

type RiskFilter = Risk | "all";

export function Findings() {
  const { report } = useApp();
  const [risk, setRisk] = useState<RiskFilter>("all");
  // Opened from Storage's "found" link: start with that search.
  const [query, setQuery] = useState(() => peekFindingsQuery() ?? "");
  const searchRef = useRef<HTMLInputElement>(null);
  useEffect(() => clearFindingsQuery(), []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        searchRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (!report) {
    return (
      <ScreenFrame title="Findings" actions={<ScanButton />}>
        <EmptyState icon={<ListIcon size={28} />} title="No scan yet">
          Run a scan to list what can be removed, grouped by category with a reason for every item.
          <div className="mt-4">
            <ScanButton />
          </div>
        </EmptyState>
      </ScreenFrame>
    );
  }

  return (
    <FindingsList
      report={report}
      risk={risk}
      setRisk={setRisk}
      query={query}
      setQuery={setQuery}
      searchRef={searchRef}
    />
  );
}

function FindingsList({
  report,
  risk,
  setRisk,
  query,
  setQuery,
  searchRef,
}: {
  report: ScanReport;
  risk: RiskFilter;
  setRisk: (r: RiskFilter) => void;
  query: string;
  setQuery: (q: string) => void;
  searchRef: RefObject<HTMLInputElement | null>;
}) {
  const { selection, setSelected } = useApp();
  const [folds, setFolds] = useState<CollapseState>(() => ({
    overrides: loadOverrides(browserStorage()),
    search: null,
  }));

  const visible = useMemo(
    () => report.findings.filter((f) => (risk === "all" || f.risk === risk) && matchesQuery(f, query)),
    [report, risk, query],
  );
  const sections = useMemo(() => groupFindings(visible), [visible]);
  const counts = useMemo(() => {
    const c: Record<RiskFilter, number> = { all: report.findings.length, safe: 0, review: 0, manual: 0 };
    for (const f of report.findings) c[f.risk] += 1;
    return c;
  }, [report]);
  const total = useMemo(() => dedupedTotal(report.findings).bytes, [report]);
  const bulk = visible.filter(isBulkSelectable);
  const anyVisibleSelected = visible.some((f) => selection.has(f.id));

  // Folding is view state only: it never touches the selection, `visible` or the totals.
  const folded = (item: Foldable) => isCollapsed(folds, query, item);
  const fold = (ids: string[], collapsed: boolean) => {
    const next = setCollapsed(folds, query, ids, collapsed);
    setFolds(next);
    if (next.overrides !== folds.overrides) saveOverrides(browserStorage(), next.overrides);
  };
  const foldItems = useMemo(() => foldables(sections), [sections]);
  const anyFolded = foldItems.some(folded);
  const allFolded = foldItems.every(folded);
  const foldIds = foldItems.map((i) => i.id);

  return (
    <ScreenFrame
      title="Findings"
      subtitle={`${formatCount(report.findings.length, "item")} · ${formatBytes(total)} found · scanned ${relativeTime(report.startedAt)}`}
      actions={<ScanButton variant="secondary" />}
      filters={
        <>
          <Segmented<RiskFilter>
            label="Filter by risk"
            value={risk}
            onChange={setRisk}
            options={[
              { value: "all", label: <>All {counts.all}</> },
              ...RISKS.map((r) => ({
                value: r,
                label: (
                  <>
                    {RISK_META[r].label} <span className="text-fg-3">{counts[r]}</span>
                  </>
                ),
              })),
            ]}
          />
          <label className="relative ml-1 flex h-7 min-w-0 flex-1 basis-44 items-center">
            <SearchIcon className="pointer-events-none absolute left-2 text-fg-3" size={14} />
            <input
              ref={searchRef}
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Filter"
              aria-label="Filter findings"
              className="h-7 w-full rounded-md border border-line bg-surface pr-2 pl-7 text-[12.5px] outline-none placeholder:text-fg-3 focus:border-accent"
            />
          </label>
          <Tip content="Selects every visible Safe and Review item that isn't blocked. Manual items are never bulk-selected.">
            <Button
              size="sm"
              variant="ghost"
              disabled={bulk.length === 0}
              onClick={() =>
                setSelected(
                  bulk.map((f) => f.id),
                  true,
                )
              }
            >
              Select All
            </Button>
          </Tip>
          <Button
            size="sm"
            variant="ghost"
            disabled={!anyVisibleSelected}
            onClick={() =>
              setSelected(
                visible.map((f) => f.id),
                false,
              )
            }
          >
            Deselect All
          </Button>
          <span aria-hidden="true" className="h-4 w-px bg-line-strong" />
          <Button size="sm" variant="ghost" disabled={!anyFolded} onClick={() => fold(foldIds, false)}>
            Expand All
          </Button>
          <Button size="sm" variant="ghost" disabled={allFolded} onClick={() => fold(foldIds, true)}>
            Collapse All
          </Button>
        </>
      }
      footer={<SelectionBar report={report} visible={visible} />}
    >
      <div className="flex flex-col gap-4 px-5 pt-3 pb-6">
        {sections.length === 0 ? (
          <EmptyState icon={<SearchIcon size={28} />} title="No findings match">
            Try another risk filter or search term.
          </EmptyState>
        ) : (
          sections.map((section) => <Section key={section.category} section={section} folded={folded} fold={fold} />)
        )}
        {report.errors.length > 0 && <ScanErrors report={report} />}
      </div>
    </ScreenFrame>
  );
}

/** Tri-state checkbox state for a set of findings; bulk-selection never adds manual items. */
function useBulkState(findings: Finding[]) {
  const { selection, setSelected } = useApp();
  const bulk = findings.filter(isBulkSelectable);
  const selectedCount = findings.filter((f) => selection.has(f.id)).length;
  const checked = bulk.length > 0 && bulk.every((f) => selection.has(f.id));
  return {
    checked,
    indeterminate: !checked && selectedCount > 0,
    disabled: bulk.length === 0 && selectedCount === 0,
    onChange: (on: boolean) =>
      on
        ? setSelected(
            bulk.map((f) => f.id),
            true,
          )
        : setSelected(
            findings.map((f) => f.id),
            false,
          ),
  };
}

type FoldProps = {
  folded: (item: Foldable) => boolean;
  fold: (ids: string[], collapsed: boolean) => void;
};

/** Disclosure chevron for a category or a set; `controls` is the id of the rows it hides. */
function FoldButton({
  collapsed,
  label,
  controls,
  onToggle,
}: {
  collapsed: boolean;
  label: string;
  controls: string;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={!collapsed}
      aria-controls={controls}
      aria-label={`${collapsed ? "Expand" : "Collapse"} ${label}`}
      className="inline-flex size-5 shrink-0 items-center justify-center rounded text-fg-3 hover:bg-surface-3 hover:text-fg"
    >
      <ChevronIcon size={12} className={cx("transition-transform", !collapsed && "rotate-90")} />
    </button>
  );
}

/** What a folded category or set hides that matters: ticked items and blocked ones. */
function FoldSummary({ findings }: { findings: Finding[] }) {
  const { selection } = useApp();
  const { selected, hints } = foldSummary(findings, selection);
  if (selected === 0 && hints.length === 0) return null;
  return (
    <span className="flex min-w-0 shrink items-center gap-1.5 overflow-hidden">
      {selected > 0 && (
        <span className="shrink-0 text-[11.5px] whitespace-nowrap text-fg-2 tabular-nums">{selected} selected</span>
      )}
      {hints.map((h) => (
        <Tip key={h.text} content={h.reasons.length > 0 ? h.reasons.join("; ") : undefined}>
          <span className="inline-flex shrink-0 items-center gap-1 rounded bg-warn-soft px-1.5 py-0.5 text-[11px] font-medium whitespace-nowrap text-warn-text">
            <WarningIcon size={11} className="text-warn" />
            {h.text}
          </span>
        </Tip>
      ))}
    </span>
  );
}

function Section({ section, folded, fold }: { section: CategorySection } & FoldProps) {
  const bulk = useBulkState(section.findings);
  const label = CATEGORY_LABEL[section.category];
  const rowsId = useId();
  const id = categoryFoldId(section.category);
  const collapsed = folded({ id, defaultCollapsed: false });
  return (
    <section aria-label={label}>
      <header className="sticky top-0 z-10 -mx-5 flex h-9 items-center gap-2 bg-bg/95 px-5 backdrop-blur">
        <span className="-ml-1 inline-flex">
          <FoldButton collapsed={collapsed} label={label} controls={rowsId} onToggle={() => fold([id], !collapsed)} />
        </span>
        <Checkbox label={`Select ${label}`} {...bulk} />
        <h2 className="shrink-0 text-[13px] font-semibold">{label}</h2>
        <span className="text-[12px] text-fg-3 tabular-nums">{section.findings.length}</span>
        {collapsed && <FoldSummary findings={section.findings} />}
        <span className="flex-1" />
        <span className="shrink-0 text-[13px] font-semibold tabular-nums">{formatBytes(section.total)}</span>
      </header>
      <div id={rowsId} hidden={collapsed} className="overflow-hidden rounded-lg border border-line bg-surface">
        {!collapsed &&
          section.blocks.map((block, i) =>
            block.kind === "single" ? (
              <FindingRow key={block.finding.id} finding={block.finding} first={i === 0} />
            ) : (
              <FindingSet
                key={block.key}
                setKey={block.key}
                findings={block.findings}
                first={i === 0}
                folded={folded}
                fold={fold}
              />
            ),
          )}
      </div>
    </section>
  );
}

function FindingSet({
  setKey,
  findings,
  first,
  folded,
  fold,
}: { setKey: string; findings: Finding[]; first: boolean } & FoldProps) {
  const bulk = useBulkState(findings);
  const { title, subtitle } = setTitle(setKey);
  const total = dedupedTotal(findings).bytes;
  const allManual = findings.every((f) => f.risk === "manual");
  const rowsId = useId();
  const id = setFoldId(setKey);
  const collapsed = folded({ id, defaultCollapsed: defaultSetCollapsed(findings.length) });
  return (
    <div className={cx(!first && "border-t border-line")}>
      {/* The chevron sits over the rows' guide line; the set's checkbox lines up with its rows' checkboxes. */}
      <div className="flex items-center gap-3 bg-surface-2/60 py-2 pr-3 pl-2.5">
        <span className="inline-flex items-center gap-[3px]">
          <FoldButton
            collapsed={collapsed}
            label={`set ${title}`}
            controls={rowsId}
            onToggle={() => fold([id], !collapsed)}
          />
          <Tip content={allManual ? "Manual items are picked one by one" : undefined}>
            <span className="inline-flex">
              <Checkbox label={`Select set ${title}`} {...bulk} />
            </span>
          </Tip>
        </span>
        <FolderStackIcon className="shrink-0 text-fg-2" />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[12.5px] font-semibold">{title}</div>
          <div className="selectable truncate text-[11px] text-fg-3">
            {formatCount(findings.length, "item")} in this set{subtitle ? ` · ${subtitle}` : ""}
          </div>
        </div>
        {collapsed && <FoldSummary findings={findings} />}
        <span className="shrink-0 text-[12.5px] font-semibold tabular-nums">{formatBytes(total)}</span>
      </div>
      <div id={rowsId} hidden={collapsed} className="relative ml-[19px] border-l-2 border-line-strong">
        {!collapsed && findings.map((f) => <FindingRow key={f.id} finding={f} first={false} />)}
      </div>
    </div>
  );
}

function FindingRow({ finding: f, first }: { finding: Finding; first: boolean }) {
  const { selection, setSelected } = useApp();
  const selectable = isSelectable(f);
  const selected = selection.has(f.id);
  const chip = actionChip(f.action);
  const blocked = f.blockers.length > 0;
  const disabledReason = blocked
    ? `Blocked: ${f.blockers.join("; ")}`
    : f.action === "info-only"
      ? "Info only — nothing to clean automatically"
      : undefined;

  return (
    <div
      className={cx(
        "grid grid-cols-[16px_minmax(0,1fr)_auto] gap-x-3 py-2.5 pr-3 pl-3",
        !first && "border-t border-line",
        selected && "bg-accent-soft/50",
      )}
    >
      <div className="pt-[2px]">
        <Tip content={disabledReason}>
          <span className="inline-flex">
            <Checkbox
              label={`Select ${f.label}`}
              checked={selected && selectable}
              disabled={!selectable}
              onChange={(on) => setSelected([f.id], on)}
            />
          </span>
        </Tip>
      </div>

      <div className="min-w-0">
        <div className="flex min-w-0 items-center gap-1.5">
          <span className={cx("truncate text-[13px] font-medium", !selectable && "text-fg-2")}>{f.label}</span>
          <RiskBadge risk={f.risk} />
          {chip && <Chip>{chip}</Chip>}
        </div>
        <p className="selectable mt-0.5 text-[12px] leading-snug text-fg-2">{f.reason}</p>
        {blocked && (
          <div className="mt-1.5 flex flex-wrap gap-1.5">
            {f.blockers.map((b) => (
              <span
                key={b}
                className="inline-flex items-center gap-1 rounded bg-warn-soft px-1.5 py-0.5 text-[11.5px] font-medium text-warn-text"
              >
                <WarningIcon size={12} className="text-warn" />
                {b}
              </span>
            ))}
          </div>
        )}
        <div className="mt-1 flex min-w-0 items-center gap-1">
          {f.path ? (
            <>
              <span className="selectable truncate font-mono text-[11px] text-fg-3" title={f.path}>
                {tildify(f.path)}
              </span>
              <IconButton
                label="Reveal in Finder"
                className="size-5"
                onClick={() => {
                  if (f.path) api.revealInFinder(f.path).catch((e) => console.warn(e));
                }}
              >
                <RevealIcon size={13} />
              </IconButton>
            </>
          ) : f.command ? (
            <span className="flex min-w-0 items-center gap-1 text-fg-3">
              <TerminalIcon size={12} className="shrink-0" />
              <code className="selectable truncate font-mono text-[11px]">{f.command.join(" ")}</code>
            </span>
          ) : null}
        </div>
      </div>

      <div className="flex flex-col items-end text-right">
        <span className={cx("text-[13px] font-semibold tabular-nums", !selectable && "text-fg-2")}>
          {formatBytes(f.bytes)}
        </span>
        {showLogical(f) && (
          <Tip content="Allocated on disk vs. the files' logical size (sparse or compressed files). Removing frees the allocated amount.">
            <span className="text-[11px] text-fg-3 tabular-nums">{formatBytes(f.logicalBytes)} logical</span>
          </Tip>
        )}
        <span className="mt-0.5 text-[11px] whitespace-nowrap text-fg-3">
          {f.lastUsed ? `used ${relativeTime(f.lastUsed)}` : null}
        </span>
      </div>
    </div>
  );
}

function SelectionBar({ report, visible }: { report: ScanReport; visible: Finding[] }) {
  const { selection, clearSelection, setConfirmOpen } = useApp();
  const selected = report.findings.filter((f) => selection.has(f.id) && isSelectable(f));
  const { bytes, covered } = dedupedTotal(selected);
  const manual = selected.filter((f) => f.risk === "manual").length;
  const visibleIds = new Set(visible.map((f) => f.id));
  const hidden = selected.filter((f) => !visibleIds.has(f.id)).length;
  const details = [
    formatCount(selected.length, "item"),
    covered.length > 0 ? `${formatCount(covered.length, "nested item")} counted once` : null,
    manual > 0 ? `includes ${manual} manual` : null,
    hidden > 0 ? `${hidden} hidden by the filter` : null,
  ].filter(Boolean);

  return (
    <footer className="flex shrink-0 items-center gap-3 border-t border-line bg-surface px-5 py-2.5">
      {selected.length === 0 ? (
        <div className="text-[12.5px] text-fg-2">Nothing selected. Tick items to clean; Safe ones are preselected.</div>
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
      <Button variant="ghost" onClick={clearSelection} disabled={selected.length === 0}>
        Clear
      </Button>
      <Button variant="primary" onClick={() => setConfirmOpen(true)} disabled={selected.length === 0}>
        Review &amp; Clean…
      </Button>
    </footer>
  );
}

function ScanErrors({ report }: { report: ScanReport }) {
  const [open, setOpen] = useState(false);
  return (
    <Callout
      tone="warn"
      title={`${formatCount(report.errors.length, "location")} couldn't be read`}
      action={
        <Button size="sm" variant="ghost" onClick={() => setOpen((o) => !o)}>
          {open ? "Hide" : "Show"}
        </Button>
      }
    >
      {report.fullDiskAccess
        ? "They were skipped; sizes above may be a little low."
        : "Usually a missing Full Disk Access permission. They were skipped; sizes above may be a little low."}
      {open && (
        <ul className="mt-2 space-y-1">
          {report.errors.map((e) => (
            <li key={`${e.path}:${e.message}`} className="font-mono text-[11px]">
              <span className="text-fg">{tildify(e.path)}</span> — {e.message}
            </li>
          ))}
        </ul>
      )}
    </Callout>
  );
}
