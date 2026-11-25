// Window chrome: source-list sidebar, unified toolbar (drag region under the overlay title bar),
// and the scan status banner shared by every screen.

import type { ReactNode } from "react";
import { MOCK } from "../api";
import { formatBytes, formatDuration, relativeTime } from "../lib/format";
import { type Screen, useApp } from "../state";
import { ClockIcon, DownloadIcon, GaugeIcon, ListIcon, SlidersIcon, StorageIcon } from "./icons";
import { Button, Callout, cx, ProgressBar, Spinner } from "./ui";

// Sidebar order; `key` is the screen's ⌘ shortcut (App.tsx builds its key map from this list).
export const NAV: { screen: Screen; label: string; Icon: typeof GaugeIcon; key: string }[] = [
  { screen: "overview", label: "Overview", Icon: GaugeIcon, key: "1" },
  { screen: "findings", label: "Findings", Icon: ListIcon, key: "2" },
  { screen: "downloads", label: "Downloads", Icon: DownloadIcon, key: "3" },
  { screen: "storage", label: "Storage", Icon: StorageIcon, key: "4" },
  { screen: "history", label: "History", Icon: ClockIcon, key: "5" },
  { screen: "settings", label: "Settings", Icon: SlidersIcon, key: "6" },
];

export function Sidebar() {
  const { screen, setScreen, report, downloads, volume, scanning, progress } = useApp();
  return (
    <nav aria-label="Sections" className="flex w-[196px] shrink-0 flex-col border-r border-line bg-sidebar">
      {/* Room for the traffic lights; drags the window. */}
      <div data-tauri-drag-region className="h-[52px] shrink-0" />
      <ul className="flex flex-col gap-0.5 px-2.5">
        {NAV.map(({ screen: s, label, Icon, key }) => {
          const active = s === screen;
          return (
            <li key={s}>
              <button
                type="button"
                onClick={() => setScreen(s)}
                aria-current={active ? "page" : undefined}
                title={`${label} (⌘${key})`}
                className={cx(
                  "flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-[13px]",
                  active ? "bg-surface-3 font-medium text-fg" : "text-fg hover:bg-surface-3/60",
                )}
              >
                <Icon className={active ? "text-accent" : "text-fg-2"} />
                <span className="flex-1">{label}</span>
                {s === "findings" && report && report.findings.length > 0 && (
                  <span className="text-[11px] text-fg-2 tabular-nums">{report.findings.length}</span>
                )}
                {s === "downloads" && downloads && downloads.findings.length > 0 && (
                  <span className="text-[11px] text-fg-2 tabular-nums">{downloads.findings.length}</span>
                )}
              </button>
            </li>
          );
        })}
      </ul>
      <div className="flex-1" data-tauri-drag-region />
      <div className="space-y-1 border-t border-line px-4 py-3 text-[11.5px] text-fg-2">
        {scanning ? (
          <div className="flex items-center gap-1.5">
            <Spinner size={11} />
            <span>
              Scanning{progress && progress.totalRules > 0 ? ` ${progress.doneRules}/${progress.totalRules}` : "…"}
            </span>
          </div>
        ) : (
          <div>{report ? `Scanned ${relativeTime(report.startedAt)}` : "Not scanned yet"}</div>
        )}
        {volume && volume.totalBytes > 0 && (
          <div className="tabular-nums">
            <span className="font-medium text-fg">{formatBytes(volume.freeBytes)}</span> free
          </div>
        )}
        {MOCK && (
          <div className="pt-1">
            <span className="rounded bg-warn-soft px-1.5 py-0.5 text-[10.5px] font-semibold text-warn-text">
              MOCK DATA
            </span>
          </div>
        )}
      </div>
    </nav>
  );
}

export function ScreenFrame({
  title,
  subtitle,
  actions,
  filters,
  footer,
  children,
}: {
  title: string;
  subtitle?: ReactNode;
  actions?: ReactNode;
  filters?: ReactNode;
  footer?: ReactNode;
  children: ReactNode;
}) {
  return (
    <main className="flex min-w-0 flex-1 flex-col bg-bg">
      <header
        data-tauri-drag-region
        className="flex h-[52px] shrink-0 items-center gap-3 border-b border-line bg-bg px-5"
      >
        <div data-tauri-drag-region className="min-w-0 flex-1">
          <h1 data-tauri-drag-region className="truncate text-[15px] font-semibold leading-tight">
            {title}
          </h1>
          {subtitle && (
            <div data-tauri-drag-region className="truncate text-[11.5px] text-fg-2">
              {subtitle}
            </div>
          )}
        </div>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </header>
      {filters && (
        <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-line px-5 py-2">{filters}</div>
      )}
      <ScanStatus />
      <div className="relative min-h-0 flex-1 overflow-y-auto">{children}</div>
      {footer}
    </main>
  );
}

export function ScanButton({ variant = "primary" }: { variant?: "primary" | "secondary" }) {
  const { scanning, startScan, report } = useApp();
  return (
    <Button variant={variant} onClick={startScan} disabled={scanning}>
      {scanning ? <Spinner size={12} /> : null}
      {scanning ? "Scanning…" : report ? "Scan Again" : "Scan"}
    </Button>
  );
}

function ScanStatus() {
  const { scanning, progress, scanError } = useApp();
  if (scanning) {
    const fraction =
      progress && progress.totalRules > 0 && progress.phase !== "volume"
        ? progress.doneRules / progress.totalRules
        : null;
    return (
      <div className="shrink-0 border-b border-line bg-surface px-5 py-2.5" aria-live="polite">
        <div className="mb-1.5 flex items-baseline justify-between gap-4 text-[12px]">
          <span className="truncate">
            {!progress || progress.phase === "volume" ? (
              "Reading volume information…"
            ) : (
              <>
                Checking <span className="font-mono text-[11.5px] text-fg-2">{progress.ruleId ?? "…"}</span>
              </>
            )}
          </span>
          {progress && progress.totalRules > 0 && (
            <span className="shrink-0 text-fg-2 tabular-nums">
              {progress.doneRules} of {progress.totalRules} rules · {progress.findings} found
            </span>
          )}
        </div>
        <ProgressBar value={fraction} label="Scan progress" />
      </div>
    );
  }
  if (scanError) {
    return (
      <div className="shrink-0 px-5 pt-3">
        <Callout tone="critical" title="The scan failed">
          {scanError}
        </Callout>
      </div>
    );
  }
  return null;
}

export function lastScanLine(startedAt: string, durationMs: number): string {
  return `Last scan ${relativeTime(startedAt)} · took ${formatDuration(durationMs)}`;
}
