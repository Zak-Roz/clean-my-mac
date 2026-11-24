import { type ReactNode, useState } from "react";
import { api } from "../api";
import type { GrowthItem } from "../bindings/GrowthItem";
import type { VolumeInfo } from "../bindings/VolumeInfo";
import { CapacityBar, FreeSpaceChart } from "../components/charts";
import { InfoIcon, LockIcon, MemoryIcon, SparkIcon, TrashIcon } from "../components/icons";
import { lastScanLine, ScanButton, ScreenFrame } from "../components/layout";
import { Button, Callout, Card, CardHeader, cx, StatusPill, Tip } from "../components/ui";
import { dedupedTotal, isSelectable } from "../lib/findings";
import { errorMessage, formatBytes, formatDelta, isNotImplemented, stubDetail, tildify } from "../lib/format";
import { useApp, useAsync } from "../state";

const SWAP_WARNING_BYTES = 10e9;

function diskHealth(v: VolumeInfo): { tone: "good" | "warn" | "critical"; label: string } {
  const share = v.totalBytes > 0 ? v.freeBytes / v.totalBytes : 1;
  if (share < 0.05 || v.freeBytes < 20e9) return { tone: "critical", label: "Almost full" };
  if (share < 0.1 || v.freeBytes < 50e9) return { tone: "warn", label: "Getting full" };
  return { tone: "good", label: "Plenty of space" };
}

export function Overview() {
  const { volume, volumeError, report, dataVersion } = useApp();
  const growth = useAsync(() => api.historyGrowth(7), [dataVersion]);
  const trend = useAsync(() => api.historyFreeSpace(30), [dataVersion]);

  return (
    <ScreenFrame
      title="Overview"
      subtitle={report ? lastScanLine(report.startedAt, report.durationMs) : "No scan yet in this session"}
      actions={<ScanButton />}
    >
      <div className="mx-auto flex max-w-[980px] flex-col gap-4 p-5">
        {volumeError && (
          <Callout tone="critical" title="Couldn't read the disk">
            {volumeError}
          </Callout>
        )}
        <DiskCard volume={volume} />
        <div className="grid grid-cols-3 gap-4">
          <SwapTile volume={volume} />
          <TrashTile volume={volume} />
          <ReadyTile />
        </div>
        <Card>
          <CardHeader title="Free space" aside="Last 30 days" />
          <div className="px-4 pt-1 pb-3">
            {trend.error ? (
              <HistoryUnavailable message={trend.error} />
            ) : (
              <FreeSpaceChart points={trend.data ?? []} height={170} label="Free space, last 30 days" />
            )}
          </div>
        </Card>
        <TopGrowth items={growth.data} error={growth.error} />
      </div>
    </ScreenFrame>
  );
}

function DiskCard({ volume }: { volume: VolumeInfo | null }) {
  const { report } = useApp();
  if (!volume || volume.totalBytes === 0) {
    return (
      <Card className="h-[148px] animate-pulse">
        <span className="sr-only">Reading disk information…</span>
      </Card>
    );
  }
  const health = diskHealth(volume);
  const used = volume.totalBytes - volume.freeBytes;
  const found = report ? Math.min(used, dedupedTotal(report.findings).bytes) : 0;
  const purgeable =
    volume.availableImportantBytes !== null && volume.availableImportantBytes > volume.freeBytes + 1e8
      ? volume.availableImportantBytes
      : null;

  return (
    <Card className="px-5 pt-4 pb-4">
      <div className="flex flex-wrap items-end justify-between gap-x-6 gap-y-2">
        <div>
          <div className="text-[12px] font-medium text-fg-2">Startup disk</div>
          <div className="mt-0.5 flex items-baseline gap-2">
            <span className="text-[34px] leading-none font-semibold tracking-tight">
              {formatBytes(volume.freeBytes)}
            </span>
            <span className="text-[14px] text-fg-2">free of {formatBytes(volume.totalBytes)}</span>
          </div>
        </div>
        <StatusPill tone={health.tone}>{health.label}</StatusPill>
      </div>
      {purgeable !== null && (
        <div className="mt-2 flex items-center gap-1 text-[12px] text-fg-2">
          <span>
            <span className="font-medium text-fg">{formatBytes(purgeable)}</span> available including purgeable space
          </span>
          <Tip content="APFS can release purgeable data (local Time Machine snapshots, iCloud-optimized files, some caches) when an app needs room. Finder counts it as available; it is not guaranteed free space.">
            <button type="button" aria-label="About purgeable space" className="text-fg-3 hover:text-fg-2">
              <InfoIcon size={13} />
            </button>
          </Tip>
        </div>
      )}
      <div className="mt-4">
        <CapacityBar
          total={volume.totalBytes}
          segments={[
            { key: "used", label: report ? "Other used" : "Used", bytes: used - found, color: "var(--neutral-mark)" },
            {
              key: "found",
              label: "Found by last scan",
              bytes: found,
              color: "var(--series)",
              hint: "Everything the scan listed, including items you should review before removing.",
            },
            { key: "free", label: "Free", bytes: volume.freeBytes, color: "var(--track)" },
          ]}
        />
      </div>
    </Card>
  );
}

function Tile({
  icon,
  title,
  children,
  className,
}: {
  icon: ReactNode;
  title: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <Card className={cx("flex flex-col gap-2 p-4", className)}>
      <div className="flex items-center gap-1.5 text-[12px] font-medium text-fg-2">
        {icon}
        {title}
      </div>
      {children}
    </Card>
  );
}

function SwapTile({ volume }: { volume: VolumeInfo | null }) {
  const swap = volume?.swapUsedBytes ?? null;
  return (
    <Tile icon={<MemoryIcon />} title="Swap in use">
      {swap === null ? (
        <p className="text-[12.5px] text-fg-2">Not reported in this build.</p>
      ) : (
        <>
          <div className="flex items-center gap-2">
            <span className="text-[22px] leading-tight font-semibold">{formatBytes(swap)}</span>
            {swap >= SWAP_WARNING_BYTES && <StatusPill tone="warn">High</StatusPill>}
          </div>
          <p className="text-[12px] leading-snug text-fg-2">
            {swap >= SWAP_WARNING_BYTES
              ? "A restart frees it; it means RAM pressure. Quitting heavy apps helps too."
              : "Swap files live on the disk; this amount is normal."}
          </p>
        </>
      )}
    </Tile>
  );
}

function TrashTile({ volume }: { volume: VolumeInfo | null }) {
  const { report, refreshVolume } = useApp();
  const [asked, setAsked] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trash = volume?.trashBytes ?? null;

  if (trash !== null) {
    return (
      <Tile icon={<TrashIcon />} title="Trash">
        <span className="text-[22px] leading-tight font-semibold">{trash === 0 ? "Empty" : formatBytes(trash)}</span>
        <p className="text-[12px] leading-snug text-fg-2">
          {trash === 0
            ? "Nothing waiting in the Trash."
            : "Still uses disk space. Empty the Trash in Finder to get it back."}
        </p>
      </Tile>
    );
  }
  if (report?.fullDiskAccess) {
    return (
      <Tile icon={<TrashIcon />} title="Trash">
        <p className="text-[12.5px] text-fg-2">Size not reported in this build.</p>
      </Tile>
    );
  }
  return (
    <Tile icon={<LockIcon />} title="Trash">
      <p className="text-[12px] leading-snug text-fg-2">
        Full Disk Access is needed to measure the Trash and a few protected caches.
      </p>
      <div className="mt-auto flex flex-wrap gap-2">
        <Button
          size="sm"
          onClick={async () => {
            try {
              await api.openFullDiskAccessSettings();
              setAsked(true);
              setError(null);
            } catch (e) {
              setError(errorMessage(e));
            }
          }}
        >
          Open Settings…
        </Button>
        {asked && (
          <Button size="sm" variant="ghost" onClick={refreshVolume}>
            Check Again
          </Button>
        )}
      </div>
      {asked && <p className="text-[11.5px] text-fg-2">Turn on clean-my-mac in the list, then come back.</p>}
      {error && <p className="text-[11.5px] text-critical-text">{error}</p>}
    </Tile>
  );
}

function ReadyTile() {
  const { report, selection, setScreen } = useApp();
  if (!report) {
    return (
      <Tile icon={<SparkIcon />} title="Ready to clean">
        <p className="text-[12px] leading-snug text-fg-2">
          Scan to find caches, stale build folders, old dumps and more.
        </p>
        <div className="mt-auto">
          <ScanButton variant="secondary" />
        </div>
      </Tile>
    );
  }
  const selected = report.findings.filter((f) => selection.has(f.id));
  const selectable = report.findings.filter(isSelectable);
  const toReview = selectable.filter((f) => !selection.has(f.id)).length;
  return (
    <Tile icon={<SparkIcon />} title="Selected to clean">
      <span className="text-[22px] leading-tight font-semibold">{formatBytes(dedupedTotal(selected).bytes)}</span>
      <p className="text-[12px] leading-snug text-fg-2">
        {selected.length} selected · {toReview} more to review
      </p>
      <div className="mt-auto">
        <Button size="sm" onClick={() => setScreen("findings")}>
          Review Findings
        </Button>
      </div>
    </Tile>
  );
}

function HistoryUnavailable({ message }: { message: string }) {
  return (
    <p className="py-6 text-center text-[12.5px] text-fg-2">
      {isNotImplemented(message)
        ? `History isn't available in this build yet (${stubDetail(message)}).`
        : `History unavailable: ${message}`}
    </p>
  );
}

function TopGrowth({ items, error }: { items: GrowthItem[] | null; error: string | null }) {
  const { setScreen } = useApp();
  const growing = (items ?? []).filter((g) => g.delta > 0);
  const byItem = growing.filter((g) => g.key.includes(":"));
  const rows = (byItem.length > 0 ? byItem : growing).slice(0, 5);
  const max = Math.max(1, ...rows.map((r) => r.delta));
  return (
    <Card>
      <CardHeader
        title="Growing fastest"
        aside={
          <>
            <span>Last 7 days</span>
            <Button size="sm" variant="ghost" onClick={() => setScreen("history")}>
              History
            </Button>
          </>
        }
      />
      <div className="px-4 pt-1 pb-3">
        {error ? (
          <HistoryUnavailable message={error} />
        ) : rows.length === 0 ? (
          <p className="py-6 text-center text-[12.5px] text-fg-2">
            {items === null ? "Loading…" : "Nothing grew in the last 7 days — or there is not enough history yet."}
          </p>
        ) : (
          <ul className="divide-y divide-line">
            {rows.map((g) => {
              const path = g.key.includes(":") ? g.key.slice(g.key.indexOf(":") + 1) : null;
              return (
                <li key={g.key} className="grid grid-cols-[minmax(0,1fr)_120px_80px] items-center gap-3 py-2">
                  <div className="min-w-0">
                    <div className="truncate text-[13px]">{g.label}</div>
                    <div className="selectable truncate text-[11px] text-fg-3" title={path ?? g.key}>
                      {path ? tildify(path) : g.key}
                    </div>
                  </div>
                  <div className="h-2 rounded-r-[4px] bg-grow" style={{ width: `${(g.delta / max) * 100}%` }} />
                  <div className="text-right text-[12.5px] font-medium tabular-nums">{formatDelta(g.delta)}</div>
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </Card>
  );
}
