// Storage: what the used space is made of (ARCHITECTURE.md §11), including the Overview's
// "Other used" that no scan rule targets. Read-only: sizes, drill-down, Reveal in Finder and a
// link to the matching findings. Nothing on this screen deletes anything.

import { type ReactNode, useEffect, useMemo, useState } from "react";
import { api } from "../api";
import type { Finding } from "../bindings/Finding";
import type { StorageBreakdown } from "../bindings/StorageBreakdown";
import type { StorageNode } from "../bindings/StorageNode";
import type { StorageProgress } from "../bindings/StorageProgress";
import { CapacityBar } from "../components/charts";
import {
  ChevronIcon,
  DashedCircleIcon,
  DiskIcon,
  DocIcon,
  EllipsisIcon,
  FolderStackIcon,
  LayersIcon,
  LockIcon,
  RevealIcon,
  StorageIcon,
} from "../components/icons";
import { ScreenFrame } from "../components/layout";
import { Button, Callout, Card, cx, EmptyState, IconButton, ProgressBar, Spinner, Tip } from "../components/ui";
import { showFindingsFor } from "../lib/findingsLink";
import { errorMessage, formatBytes, formatCount, formatDuration, relativeTime, tildify } from "../lib/format";
import {
  accounting,
  barShare,
  canOpen,
  childrenOf,
  findingsIn,
  findingsQuery,
  hasProtected,
  isFolderRest,
  isStale,
  needsListing,
  progressFraction,
  resolveTrail,
  rootNode,
} from "../lib/storage";
import { useApp } from "../state";
import { storage, useStorage } from "../storageState";

export function Storage() {
  const { report } = useApp();
  const s = useStorage();

  // Show the stored measurement (found marks for the current scan), and measure again when there
  // is none yet or a scan finished after it: the breakdown is cached per scan.
  useEffect(() => {
    let cancelled = false;
    storage.load().then(() => {
      const { breakdown, measuring, error } = storage.get();
      if (!cancelled && !measuring && !error && isStale(breakdown, report)) storage.measure();
    });
    return () => {
      cancelled = true;
    };
  }, [report]);

  const root = useMemo(() => (s.breakdown ? rootNode(s.breakdown) : null), [s.breakdown]);
  const trail = useMemo(() => (root ? resolveTrail(root, s.trail, s.listings) : []), [root, s.trail, s.listings]);
  const current = trail[trail.length - 1] ?? null;
  const ids = trail.map((n) => n.id);

  const open = (node: StorageNode) => {
    if (isFolderRest(node)) {
      storage.list(node); // the folder on screen, listed in full
      return;
    }
    storage.setTrail([...ids, node.id]);
    if (needsListing(node, s.listings)) storage.list(node);
  };
  const up = () => {
    if (ids.length > 1) storage.setTrail(ids.slice(0, -1));
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;
      if (!typing && ((e.metaKey && e.key === "ArrowUp") || e.key === "Backspace") && ids.length > 1) {
        e.preventDefault();
        storage.setTrail(ids.slice(0, -1));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [ids]);

  const b = s.breakdown;
  return (
    <ScreenFrame
      title="Storage"
      subtitle={
        b
          ? `Measured ${relativeTime(b.measuredAt)} · took ${formatDuration(b.durationMs)}`
          : "What the used space on this Mac is made of"
      }
      actions={
        <Button onClick={() => storage.measure()} disabled={s.measuring}>
          {s.measuring && <Spinner size={12} />}
          {s.measuring ? "Measuring…" : b ? "Measure Again" : "Measure"}
        </Button>
      }
    >
      <div className="mx-auto flex max-w-[980px] flex-col gap-4 p-5">
        {s.measuring && <MeasureProgress progress={s.progress} previous={b !== null} />}
        {s.error && (
          <Callout tone="critical" title="Couldn't measure the disk">
            {s.error}
          </Callout>
        )}
        {!b && !s.measuring && s.loaded && (
          <EmptyState icon={<StorageIcon size={28} />} title="Not measured yet">
            Measuring walks the whole disk once (it takes a minute or less) and shows what the used space is made of,
            including everything no scan rule looks at. Nothing is changed.
            <div className="mt-4">
              <Button variant="primary" onClick={() => storage.measure()}>
                Measure
              </Button>
            </div>
          </EmptyState>
        )}
        {b && root && current && (
          <>
            <FullDiskAccessHint breakdown={b} />
            <Summary breakdown={b} />
            <Card className="overflow-hidden">
              <LevelHeader trail={trail} onJump={(i) => storage.setTrail(ids.slice(0, i + 1))} onUp={up} />
              <Level node={current} findings={report?.findings ?? []} onOpen={open} fullDiskAccess={b.fullDiskAccess} />
            </Card>
            <Accounting breakdown={b} />
          </>
        )}
      </div>
    </ScreenFrame>
  );
}

function MeasureProgress({ progress, previous }: { progress: StorageProgress | null; previous: boolean }) {
  const fraction = progressFraction(progress);
  return (
    <Card className="px-4 py-3" aria-live="polite">
      <div className="mb-1.5 flex items-baseline justify-between gap-4 text-[12px]">
        <span>
          {!progress || progress.phase === "volumes" ? "Reading the volumes…" : "Measuring every folder on the disk…"}
          {previous && <span className="text-fg-2"> Showing the previous measurement meanwhile.</span>}
        </span>
        {progress && progress.expectedBytes > 0 && (
          <span className="shrink-0 text-fg-2 tabular-nums">
            {formatBytes(progress.walkedBytes)} of ~{formatBytes(progress.expectedBytes)}
          </span>
        )}
      </div>
      <ProgressBar value={fraction} label="Measuring progress" />
    </Card>
  );
}

function FullDiskAccessHint({ breakdown }: { breakdown: StorageBreakdown }) {
  const [error, setError] = useState<string | null>(null);
  if (breakdown.fullDiskAccess || !hasProtected(breakdown.nodes)) return null;
  return (
    <Callout
      tone="info"
      title="Some folders need Full Disk Access"
      action={
        <Button
          size="sm"
          onClick={() =>
            api.openFullDiskAccessSettings().then(
              () => setError(null),
              (e) => setError(errorMessage(e)),
            )
          }
        >
          Open Settings…
        </Button>
      }
    >
      macOS keeps folders such as the Trash, Mail and some app containers from being read, so their size is part of “Not
      measured”. Turn on clean-my-mac in Full Disk Access, then Measure Again.
      {error && <span className="block text-critical-text">{error}</span>}
    </Callout>
  );
}

/** The Overview's capacity bar with "Other used" split into what was measured and what wasn't. */
function Summary({ breakdown: b }: { breakdown: StorageBreakdown }) {
  const found = Math.min(b.foundBytes, b.usedBytes);
  const unmeasured = Math.min(b.unmeasuredBytes, b.usedBytes - found);
  const other = Math.max(0, b.usedBytes - found - unmeasured);
  return (
    <Card className="px-5 pt-4 pb-4">
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="text-[28px] leading-none font-semibold tracking-tight">{formatBytes(b.usedBytes)}</span>
        <span className="text-[14px] text-fg-2">used of {formatBytes(b.totalBytes)}</span>
      </div>
      <p className="mt-1.5 max-w-[720px] text-[12.5px] leading-snug text-fg-2">
        A scan only measures what its rules look for. This map measures everything: the folders below hold{" "}
        <span className="font-medium text-fg">{formatBytes(other + found)}</span>
        {found > 0 && (
          <>
            {" "}
            (<span className="font-medium text-fg">{formatBytes(found)}</span> of it found by the last scan)
          </>
        )}
        {unmeasured > 0 && (
          <>
            , and <span className="font-medium text-fg">{formatBytes(unmeasured)}</span> could not be measured
          </>
        )}
        .
      </p>
      <div className="mt-4">
        <CapacityBar
          total={b.totalBytes}
          segments={[
            {
              key: "found",
              label: "Found by last scan",
              bytes: found,
              color: "var(--series)",
              hint: "The same total as on the Overview.",
            },
            {
              key: "other",
              label: "Other used, measured",
              bytes: other,
              color: "var(--neutral-mark)",
              hint: "Folders, apps and system volumes no scan rule targets. Drill down below.",
            },
            {
              key: "unmeasured",
              label: "Not measured",
              bytes: unmeasured,
              color: "color-mix(in srgb, var(--neutral-mark) 45%, var(--track))",
              hint: "Used space no readable file accounts for: snapshots, protected folders, file-system metadata.",
            },
            { key: "free", label: "Free", bytes: b.freeBytes, color: "var(--track)" },
          ]}
        />
      </div>
    </Card>
  );
}

function LevelHeader({
  trail,
  onJump,
  onUp,
}: {
  trail: StorageNode[];
  onJump: (index: number) => void;
  onUp: () => void;
}) {
  const current = trail[trail.length - 1] as StorageNode;
  const revealable = current.path !== null && (current.kind === "folder" || current.kind === "file");
  return (
    <header className="border-b border-line px-4 pt-3 pb-2.5">
      <div className="flex items-center gap-3">
        <nav aria-label="Location" className="flex min-w-0 flex-1 flex-wrap items-center gap-0.5 text-[13px]">
          {trail.map((n, i) => {
            const last = i === trail.length - 1;
            return (
              <span key={n.id} className="flex min-w-0 items-center gap-0.5">
                {i > 0 && <ChevronIcon size={12} className="shrink-0 text-fg-3" />}
                {last ? (
                  <span className="truncate font-semibold" aria-current="location">
                    {n.label}
                  </span>
                ) : (
                  <button
                    type="button"
                    onClick={() => onJump(i)}
                    className="truncate rounded px-1 text-accent hover:bg-accent-soft"
                  >
                    {n.label}
                  </button>
                )}
              </span>
            );
          })}
        </nav>
        <span className="shrink-0 text-[12.5px] font-medium tabular-nums">{formatBytes(current.bytes)}</span>
        {revealable && (
          <IconButton label="Reveal in Finder" onClick={() => api.revealInFinder(current.path as string)}>
            <RevealIcon size={14} />
          </IconButton>
        )}
        {trail.length > 1 && (
          <Button size="sm" variant="ghost" onClick={onUp} title="Up one level (⌘↑)">
            Up
          </Button>
        )}
      </div>
      {current.note && <p className="mt-1 text-[12px] leading-snug text-fg-2">{current.note}</p>}
      {current.path && current.kind === "folder" && (
        <p className="selectable mt-0.5 truncate text-[11.5px] text-fg-3" title={current.path}>
          {tildify(current.path)}
        </p>
      )}
    </header>
  );
}

const GRID = "grid grid-cols-[minmax(0,1fr)_minmax(80px,150px)_76px_100px_92px_52px] items-center gap-3";

function Level({
  node,
  findings,
  onOpen,
  fullDiskAccess,
}: {
  node: StorageNode;
  findings: readonly Finding[];
  onOpen: (node: StorageNode) => void;
  fullDiskAccess: boolean;
}) {
  const s = useStorage();
  const children = childrenOf(node, s.listings);
  const listing = s.listing.has(node.id);
  if (children.length === 0) {
    return (
      <div className="px-4 py-8 text-center text-[12.5px] text-fg-2">
        {listing ? (
          <span className="inline-flex items-center gap-2">
            <Spinner size={12} /> Listing…
          </span>
        ) : s.listingError ? (
          <span className="text-critical-text">{s.listingError}</span>
        ) : node.unreadable ? (
          "This folder can't be read."
        ) : (
          "Nothing in here."
        )}
      </div>
    );
  }
  return (
    <div>
      <div className={cx(GRID, "border-b border-line px-4 py-1.5 text-[11px] font-medium text-fg-3")}>
        <span>Name</span>
        <span aria-hidden="true" />
        <span className="text-right">Size</span>
        <span className="text-right" title="Part of it the last scan found">
          Found
        </span>
        <span className="text-right">Modified</span>
        <span />
      </div>
      <ul className="divide-y divide-line">
        {children.map((child) => (
          <Row
            key={child.id}
            node={child}
            parentBytes={node.bytes}
            findings={findings}
            onOpen={onOpen}
            busy={listing && isFolderRest(child)}
            fullDiskAccess={fullDiskAccess}
          />
        ))}
      </ul>
      {s.listingError && <p className="px-4 py-2 text-[12px] text-critical-text">{s.listingError}</p>}
    </div>
  );
}

function KindIcon({ node }: { node: StorageNode }) {
  const className = "shrink-0 text-fg-2";
  switch (node.kind) {
    case "folder":
      return <FolderStackIcon className={cx(className, "text-accent")} />;
    case "file":
      return <DocIcon className={className} />;
    case "group":
      return <LayersIcon className={className} />;
    case "volume":
      return <DiskIcon className={className} />;
    case "rest":
      return <EllipsisIcon className={className} />;
    case "unmeasured":
      return <DashedCircleIcon className={className} />;
  }
}

function unreadableHint(node: StorageNode, fullDiskAccess: boolean): string | null {
  switch (node.unreadable) {
    case "protected":
      return fullDiskAccess
        ? "Part of this is protected by macOS (System Integrity Protection) and was not counted."
        : "Part of this needs Full Disk Access to be read and was not counted.";
    case "denied":
      return "Part of this belongs to the system or another user and was not counted.";
    default:
      return null;
  }
}

function subtitle(node: StorageNode): ReactNode {
  switch (node.kind) {
    case "folder":
      return node.entries === 0 ? "Empty" : formatCount(node.entries, "item");
    case "rest":
      return isFolderRest(node) ? "Show all" : null;
    case "file":
      return null;
    default:
      return node.note;
  }
}

function Row({
  node,
  parentBytes,
  findings,
  onOpen,
  busy,
  fullDiskAccess,
}: {
  node: StorageNode;
  parentBytes: number;
  findings: readonly Finding[];
  onOpen: (node: StorageNode) => void;
  busy: boolean;
  fullDiskAccess: boolean;
}) {
  const { setScreen } = useApp();
  const share = barShare(node, parentBytes);
  const openable = canOpen(node);
  const matching = node.foundBytes > 0 ? findingsIn(node, findings) : [];
  const revealable = node.path !== null && (node.kind === "folder" || node.kind === "file");
  const lock = unreadableHint(node, fullDiskAccess);
  const sub = subtitle(node);
  const name = (
    <>
      <KindIcon node={node} />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5">
          <span className="truncate text-[13px]">{node.label}</span>
          {lock && (
            <Tip content={lock}>
              <span className="inline-flex shrink-0 text-warn" role="img" aria-label={lock}>
                <LockIcon size={12} />
              </span>
            </Tip>
          )}
        </span>
        {sub && (
          <span className="block truncate text-[11px] text-fg-3" title={typeof sub === "string" ? sub : undefined}>
            {sub}
          </span>
        )}
      </span>
    </>
  );
  return (
    <li className={cx(GRID, "px-4 py-1.5", openable && "hover:bg-surface-2")}>
      {openable ? (
        <button
          type="button"
          onClick={() => onOpen(node)}
          className="flex min-w-0 items-center gap-2 rounded text-left focus-visible:outline-2 focus-visible:outline-accent"
        >
          {name}
        </button>
      ) : (
        <div className="flex min-w-0 items-center gap-2">{name}</div>
      )}
      <div className="h-2 w-full rounded-full bg-track" aria-hidden="true">
        <div
          className="flex h-full overflow-hidden rounded-full bg-neutral-mark"
          style={{ width: `${Math.max(share.total > 0 ? 1.5 : 0, share.total * 100)}%` }}
        >
          {share.found > 0 && (
            <div className="h-full bg-series" style={{ width: `${(share.found / share.total) * 100}%` }} />
          )}
        </div>
      </div>
      <div className="text-right text-[12.5px] font-medium tabular-nums">
        {formatBytes(node.bytes)}
        {parentBytes > 0 && (
          <span className="block text-[10.5px] font-normal text-fg-3">
            {share.total < 0.001 ? "<0.1" : (share.total * 100).toFixed(1)}%
          </span>
        )}
      </div>
      <div className="text-right text-[12px] tabular-nums">
        {node.foundBytes <= 0 ? (
          <span className="text-fg-3">—</span>
        ) : matching.length > 0 ? (
          <Tip content={`Show the ${formatCount(matching.length, "finding")} in Findings`}>
            <button
              type="button"
              className="rounded px-1 text-accent hover:bg-accent-soft"
              onClick={() => {
                showFindingsFor(findingsQuery(node, matching));
                setScreen("findings");
              }}
            >
              {formatBytes(node.foundBytes)}
            </button>
          </Tip>
        ) : (
          formatBytes(node.foundBytes)
        )}
      </div>
      <div className="text-right text-[12px] text-fg-2" title={node.modified ?? undefined}>
        {node.modified ? relativeTime(node.modified) : "—"}
      </div>
      <div className="flex items-center justify-end gap-0.5">
        {busy && <Spinner size={12} />}
        {revealable && (
          <IconButton label="Reveal in Finder" onClick={() => api.revealInFinder(node.path as string)}>
            <RevealIcon size={14} />
          </IconButton>
        )}
        {openable && !busy && (
          <IconButton label={node.kind === "rest" ? "Show all items" : "Open"} onClick={() => onOpen(node)}>
            <ChevronIcon size={14} />
          </IconButton>
        )}
      </div>
    </li>
  );
}

/** How the parts add up, and what could not be read. */
function Accounting({ breakdown: b }: { breakdown: StorageBreakdown }) {
  const { parts, difference, unmeasuredShare } = accounting(b);
  return (
    <div className="space-y-1 px-1 text-[11.5px] leading-snug text-fg-2">
      <p>
        The parts add up to {formatBytes(parts)}
        {difference === 0 ? ", the used space exactly" : ` (${formatBytes(Math.abs(difference))} off the used space)`}:{" "}
        {formatBytes(b.walkedBytes)} measured file by file, {formatBytes(b.volumesBytes)} in macOS volumes sized as a
        whole, {formatBytes(b.unmeasuredBytes)} not measured ({(unmeasuredShare * 100).toFixed(1)}%).
        {b.overcountBytes > 0 &&
          ` Files counted ${formatBytes(b.overcountBytes)} more than the volume uses: APFS clones share space but are counted per copy.`}
        {b.foundBytes > b.foundLocatedBytes &&
          ` ${formatBytes(b.foundBytes - b.foundLocatedBytes)} of the findings have no folder here (such as Docker data cleaned by a command).`}
        {b.localSnapshots.length > 0 &&
          ` ${formatCount(b.localSnapshots.length, "local snapshot")} hold space that is part of “Not measured”.`}
      </p>
      {b.errors.length > 0 && (
        <details>
          <summary className="cursor-default select-none hover:text-fg">
            {formatCount(b.errors.length, "path")} could not be read
          </summary>
          <ul className="selectable mt-1 max-h-48 space-y-0.5 overflow-y-auto font-mono text-[11px]">
            {b.errors.map((e) => (
              <li key={`${e.path}:${e.message}`} className="truncate" title={e.path}>
                {tildify(e.path)} — {e.message}
              </li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
