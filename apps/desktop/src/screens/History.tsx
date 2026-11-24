import { useState } from "react";
import { api } from "../api";
import type { FreeSpacePoint } from "../bindings/FreeSpacePoint";
import type { GrowthItem } from "../bindings/GrowthItem";
import { DeltaBar, FreeSpaceChart } from "../components/charts";
import { ScreenFrame } from "../components/layout";
import { Button, Callout, Card, CardHeader, cx, Segmented } from "../components/ui";
import {
  formatBytes,
  formatDate,
  formatDateTime,
  formatDelta,
  isNotImplemented,
  stubDetail,
  tildify,
} from "../lib/format";
import { useApp, useAsync } from "../state";

const RANGES = [7, 30, 90] as const;
type Range = (typeof RANGES)[number];

export function History() {
  const { dataVersion } = useApp();
  const [days, setDays] = useState<Range>(30);
  const trend = useAsync(() => api.historyFreeSpace(days), [days, dataVersion]);
  const growth = useAsync(() => api.historyGrowth(days), [days, dataVersion]);
  const error = trend.error ?? growth.error;

  // GrowthItem rows mix rules ("cache.npm") and findings ("cache.npm:/path"): finding ids contain ':'.
  const byRule = (growth.data ?? []).filter((g) => !g.key.includes(":"));
  const byItem = (growth.data ?? []).filter((g) => g.key.includes(":"));

  return (
    <ScreenFrame
      title="History"
      subtitle="What changed between scans. Every scan adds a data point."
      filters={
        <Segmented<Range>
          label="Time range"
          value={days}
          onChange={setDays}
          options={RANGES.map((d) => ({ value: d, label: `${d} days` }))}
        />
      }
    >
      <div
        className={cx(
          "mx-auto flex max-w-[980px] flex-col gap-4 p-5 transition-opacity",
          (trend.loading || growth.loading) && (trend.data || growth.data) && "opacity-60",
        )}
      >
        {error ? (
          <Callout tone={isNotImplemented(error) ? "info" : "critical"} title="History isn't available">
            {isNotImplemented(error) ? `Not built into this version of the engine yet (${stubDetail(error)}).` : error}
          </Callout>
        ) : (
          <>
            <FreeSpaceCard points={trend.data ?? []} days={days} />
            <GrowthTable title="Growth by rule" rows={byRule} loading={growth.data === null} />
            <GrowthTable title="Growth by item" rows={byItem} loading={growth.data === null} showPath />
          </>
        )}
      </div>
    </ScreenFrame>
  );
}

function FreeSpaceCard({ points, days }: { points: FreeSpacePoint[]; days: number }) {
  const [showTable, setShowTable] = useState(false);
  const sorted = [...points].sort((a, b) => Date.parse(a.at) - Date.parse(b.at));
  const first = sorted[0];
  const last = sorted[sorted.length - 1];
  const lowest = sorted.reduce<FreeSpacePoint | null>((m, p) => (!m || p.freeBytes < m.freeBytes ? p : m), null);
  const change = first && last ? last.freeBytes - first.freeBytes : 0;

  return (
    <Card>
      <CardHeader
        title="Free space"
        aside={
          sorted.length > 1 && (
            <Button size="sm" variant="ghost" onClick={() => setShowTable((s) => !s)}>
              {showTable ? "Show Chart" : "Show Table"}
            </Button>
          )
        }
      />
      {sorted.length > 1 && last && lowest && (
        <dl className="flex flex-wrap gap-x-8 gap-y-1 px-4 pt-1 text-[12px]">
          <Figure label="Latest scan" value={formatBytes(last.freeBytes)} />
          <Figure label={`Change in ${days} days`} value={formatDelta(change)} />
          <Figure label="Lowest" value={`${formatBytes(lowest.freeBytes)} · ${formatDate(lowest.at)}`} />
        </dl>
      )}
      <div className="px-4 pt-2 pb-3">
        {showTable ? (
          <div className="max-h-[260px] overflow-y-auto">
            <table className="w-full text-[12px]">
              <thead className="sticky top-0 bg-surface text-left text-fg-2">
                <tr>
                  <th className="py-1 font-medium">Scan</th>
                  <th className="py-1 text-right font-medium">Free</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-line">
                {[...sorted].reverse().map((p) => (
                  <tr key={p.at}>
                    <td className="py-1">{formatDateTime(p.at)}</td>
                    <td className="py-1 text-right tabular-nums">{formatBytes(p.freeBytes)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <FreeSpaceChart points={sorted} height={220} label={`Free space, last ${days} days`} />
        )}
      </div>
    </Card>
  );
}

function Figure({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-fg-2">{label}</dt>
      <dd className="text-[14px] font-semibold">{value}</dd>
    </div>
  );
}

function GrowthTable({
  title,
  rows,
  loading,
  showPath = false,
}: {
  title: string;
  rows: GrowthItem[];
  loading: boolean;
  showPath?: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const shown = expanded ? rows : rows.slice(0, 8);
  const maxAbs = Math.max(0, ...rows.map((r) => Math.abs(r.delta)));
  return (
    <Card>
      <CardHeader
        title={title}
        aside={
          rows.length > 8 && (
            <Button size="sm" variant="ghost" onClick={() => setExpanded((e) => !e)}>
              {expanded ? "Show Top 8" : `Show All ${rows.length}`}
            </Button>
          )
        }
      />
      <div className="px-4 pt-1 pb-3">
        {rows.length === 0 ? (
          <p className="py-5 text-center text-[12.5px] text-fg-2">
            {loading ? "Loading…" : "No changes recorded in this period yet."}
          </p>
        ) : (
          <table className="w-full table-fixed text-[12.5px]">
            <thead className="text-left text-[11.5px] text-fg-2">
              <tr className="border-b border-line">
                <th className="py-1.5 font-medium">Name</th>
                <th className="w-[84px] py-1.5 text-right font-medium">Before</th>
                <th className="w-[84px] py-1.5 text-right font-medium">Now</th>
                <th className="w-[112px] py-1.5 pl-4 font-medium">
                  <span className="sr-only">Change bar</span>
                </th>
                <th className="w-[84px] py-1.5 text-right font-medium">Change</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {shown.map((g) => {
                const path = showPath && g.key.includes(":") ? g.key.slice(g.key.indexOf(":") + 1) : null;
                return (
                  <tr key={g.key}>
                    <td className="py-1.5 pr-3">
                      <div className="truncate">{g.label}</div>
                      <div className="selectable truncate text-[11px] text-fg-3" title={path ?? g.key}>
                        {path ? tildify(path) : g.key}
                      </div>
                    </td>
                    <td className="py-1.5 text-right text-fg-2 tabular-nums">
                      {g.bytesThen === 0 ? "new" : formatBytes(g.bytesThen)}
                    </td>
                    <td className="py-1.5 text-right tabular-nums">
                      {g.bytesNow === 0 ? "gone" : formatBytes(g.bytesNow)}
                    </td>
                    <td className="py-1.5 pl-4">
                      <DeltaBar delta={g.delta} maxAbs={maxAbs} width={88} />
                    </td>
                    <td className="py-1.5 text-right font-medium tabular-nums">{formatDelta(g.delta)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
      </div>
    </Card>
  );
}
