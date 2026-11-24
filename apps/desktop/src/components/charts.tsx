// Hand-rolled SVG/HTML charts: a free-space trend line, a disk capacity bar and a growth delta bar.
// Specs follow the dataviz guide: 2px line + 10% area wash, hairline solid grid, crosshair tooltip,
// end label; segmented bar with 2px surface gaps and a legend; text always in ink tokens.

import { type KeyboardEvent, type PointerEvent, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { FreeSpacePoint } from "../bindings/FreeSpacePoint";
import { formatBytes, formatDate, formatDateTime } from "../lib/format";
import { cx, Tip } from "./ui";

function useElementWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.clientWidth);
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setWidth(entry.contentRect.width);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return [ref, width] as const;
}

function niceStep(range: number, count: number): number {
  const raw = range / Math.max(1, count);
  const pow = 10 ** Math.floor(Math.log10(raw));
  const n = raw / pow;
  const step = n <= 1 ? 1 : n <= 2 ? 2 : n <= 2.5 ? 2.5 : n <= 5 ? 5 : 10;
  return step * pow;
}

/** Axis labels: "40 GB", "1.2 TB" — no trailing ".0". */
function axisBytes(v: number): string {
  return formatBytes(v).replace(/\.0 /, " ");
}

export function FreeSpaceChart({ points, height, label }: { points: FreeSpacePoint[]; height: number; label: string }) {
  const [ref, width] = useElementWidth<HTMLDivElement>();
  const [active, setActive] = useState<number | null>(null);

  const data = useMemo(
    () =>
      points
        .map((p) => ({ t: Date.parse(p.at), v: p.freeBytes }))
        .filter((p) => Number.isFinite(p.t))
        .sort((a, b) => a.t - b.t),
    [points],
  );

  const margin = { top: 14, right: 64, bottom: 22, left: 48 };
  const plotW = Math.max(10, width - margin.left - margin.right);
  const plotH = Math.max(10, height - margin.top - margin.bottom);

  const scales = useMemo(() => {
    if (data.length === 0) return null;
    const vs = data.map((d) => d.v);
    const lo = Math.min(...vs);
    const hi = Math.max(...vs);
    const pad = Math.max((hi - lo) * 0.15, 2e9);
    const step = niceStep(hi - lo + pad * 2, 3);
    const y0 = Math.max(0, Math.floor((lo - pad) / step) * step);
    const y1 = Math.ceil((hi + pad) / step) * step;
    const yTicks: number[] = [];
    for (let v = y0; v <= y1 + step / 2; v += step) yTicks.push(v);
    const t0 = data[0]?.t ?? 0;
    const t1 = data[data.length - 1]?.t ?? t0 + 1;
    const span = Math.max(1, t1 - t0);
    const x = (t: number) => ((t - t0) / span) * plotW;
    const y = (v: number) => plotH - ((v - y0) / (y1 - y0)) * plotH;
    const xTickCount = Math.max(2, Math.min(5, Math.floor(plotW / 110)));
    const xTicks = Array.from({ length: xTickCount }, (_, i) => t0 + (span * i) / (xTickCount - 1));
    return { x, y, yTicks, xTicks };
  }, [data, plotW, plotH]);

  if (data.length < 2 || !scales) {
    return (
      <div
        ref={ref}
        style={{ height }}
        className="flex items-center justify-center px-6 text-center text-[12.5px] text-fg-2"
      >
        Not enough history yet — every scan records a free-space point; the trend appears after two.
      </div>
    );
  }

  const { x, y, yTicks, xTicks } = scales;
  const line = data.map((d, i) => `${i === 0 ? "M" : "L"}${x(d.t).toFixed(1)},${y(d.v).toFixed(1)}`).join("");
  const last = data[data.length - 1] as { t: number; v: number };
  const first = data[0] as { t: number; v: number };
  const area = `${line}L${x(last.t).toFixed(1)},${plotH}L${x(first.t).toFixed(1)},${plotH}Z`;
  const activePoint = active === null ? null : (data[active] ?? null);

  function nearest(clientX: number, rect: DOMRect) {
    const px = clientX - rect.left - margin.left;
    let best = 0;
    let bestDist = Number.POSITIVE_INFINITY;
    for (const [i, d] of data.entries()) {
      const dist = Math.abs(x(d.t) - px);
      if (dist < bestDist) {
        best = i;
        bestDist = dist;
      }
    }
    return best;
  }

  function onPointerMove(e: PointerEvent<SVGSVGElement>) {
    setActive(nearest(e.clientX, e.currentTarget.getBoundingClientRect()));
  }

  function onKeyDown(e: KeyboardEvent<SVGSVGElement>) {
    if (e.key === "ArrowLeft") setActive((a) => Math.max(0, (a ?? data.length) - 1));
    else if (e.key === "ArrowRight") setActive((a) => Math.min(data.length - 1, (a ?? -1) + 1));
    else return;
    e.preventDefault();
  }

  const tooltipLeft = activePoint ? margin.left + x(activePoint.t) : 0;
  const flip = tooltipLeft > width - 150;

  return (
    <div ref={ref} className="relative" style={{ height }}>
      <svg
        width={width}
        height={height}
        role="img"
        aria-label={`${label}: ${axisBytes(first.v)} on ${formatDate(first.t)}, ${axisBytes(last.v)} now. Use arrow keys to read points.`}
        // biome-ignore lint/a11y/noNoninteractiveTabindex: the chart is keyboard-explorable with arrow keys
        tabIndex={0}
        onPointerMove={onPointerMove}
        onPointerLeave={() => setActive(null)}
        onKeyDown={onKeyDown}
        onBlur={() => setActive(null)}
        className="block overflow-visible outline-none"
      >
        <g transform={`translate(${margin.left},${margin.top})`}>
          {yTicks.map((v) => (
            <g key={v}>
              <line x1={0} x2={plotW} y1={y(v)} y2={y(v)} stroke="var(--grid)" strokeWidth={1} />
              <text x={-8} y={y(v)} dy="0.32em" textAnchor="end" className="fill-fg-3 text-[10.5px] tabular-nums">
                {axisBytes(v)}
              </text>
            </g>
          ))}
          {xTicks.map((t, i) => (
            <text
              key={t}
              x={x(t)}
              y={plotH + 16}
              textAnchor={i === 0 ? "start" : i === xTicks.length - 1 ? "end" : "middle"}
              className="fill-fg-3 text-[10.5px] tabular-nums"
            >
              {formatDate(t)}
            </text>
          ))}
          <path d={area} fill="var(--series-soft)" />
          <path
            d={line}
            fill="none"
            stroke="var(--series)"
            strokeWidth={2}
            strokeLinejoin="round"
            strokeLinecap="round"
          />
          <circle cx={x(last.t)} cy={y(last.v)} r={4} fill="var(--series)" stroke="var(--surface)" strokeWidth={2} />
          <text x={x(last.t) + 9} y={y(last.v)} dy="0.32em" className="fill-fg text-[11.5px] font-semibold">
            {axisBytes(last.v)}
          </text>
          {activePoint && (
            <g pointerEvents="none">
              <line
                x1={x(activePoint.t)}
                x2={x(activePoint.t)}
                y1={0}
                y2={plotH}
                stroke="var(--fg-3)"
                strokeWidth={1}
              />
              <circle
                cx={x(activePoint.t)}
                cy={y(activePoint.v)}
                r={4.5}
                fill="var(--series)"
                stroke="var(--surface)"
                strokeWidth={2}
              />
            </g>
          )}
        </g>
      </svg>
      {activePoint && (
        <div
          className="pointer-events-none absolute top-1 z-10 rounded-md border border-line bg-surface px-2 py-1 shadow-md"
          style={flip ? { right: width - tooltipLeft + 10 } : { left: tooltipLeft + 10 }}
        >
          <div className="text-[12.5px] font-semibold tabular-nums">{formatBytes(activePoint.v)} free</div>
          <div className="text-[11px] text-fg-2">{formatDateTime(activePoint.t)}</div>
        </div>
      )}
    </div>
  );
}

export interface CapacitySegment {
  key: string;
  label: string;
  bytes: number;
  color: string;
  hint?: string;
}

/** Tall segmented bar: one whole (the disk) split into parts, with a legend underneath. */
export function CapacityBar({ total, segments }: { total: number; segments: CapacitySegment[] }) {
  const visible = segments.filter((s) => s.bytes > 0);
  return (
    <div>
      <div
        className="flex h-6 w-full gap-[2px]"
        role="img"
        aria-label={visible.map((s) => `${s.label} ${formatBytes(s.bytes)}`).join(", ")}
      >
        {visible.map((s) => (
          <Tip
            key={s.key}
            content={
              <span>
                <span className="font-semibold tabular-nums">{formatBytes(s.bytes)}</span>{" "}
                <span className="text-fg-2">
                  {s.label} · {((s.bytes / total) * 100).toFixed(1)}%
                </span>
                {s.hint && <span className="block text-fg-2">{s.hint}</span>}
              </span>
            }
          >
            <div
              className="h-full min-w-1.5 rounded-[4px] transition-[filter] hover:brightness-110"
              style={{ flexGrow: s.bytes, flexBasis: 0, background: s.color }}
            />
          </Tip>
        ))}
      </div>
      <ul className="mt-2.5 flex flex-wrap gap-x-5 gap-y-1 text-[12px]">
        {visible.map((s) => (
          <li key={s.key} className="flex items-center gap-1.5">
            <span className="size-2.5 rounded-[3px]" style={{ background: s.color }} aria-hidden="true" />
            <span className="text-fg-2">{s.label}</span>
            <span className="font-medium tabular-nums">{formatBytes(s.bytes)}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Diverging bar around a zero line: growth to the right (warm), shrink to the left (cool). */
export function DeltaBar({ delta, maxAbs, width = 88 }: { delta: number; maxAbs: number; width?: number }) {
  const half = width / 2;
  const len = maxAbs > 0 ? Math.max(delta === 0 ? 0 : 2, (Math.abs(delta) / maxAbs) * (half - 1)) : 0;
  const grow = delta > 0;
  return (
    <div className="relative h-2" style={{ width }} aria-hidden="true">
      <div className="absolute inset-y-[-3px] w-px bg-line-strong" style={{ left: half }} />
      {len > 0 && (
        <div
          className={cx("absolute inset-y-0", grow ? "rounded-r-[4px]" : "rounded-l-[4px]")}
          style={{
            left: grow ? half + 1 : half - len,
            width: len,
            background: grow ? "var(--grow)" : "var(--shrink)",
          }}
        />
      )}
    </div>
  );
}
