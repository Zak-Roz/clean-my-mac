// Settings: the daily background scan (a LaunchAgent, installed only when the user flips the
// switch) and the thresholds behind the menu bar warning and the notifications (config.toml).

import { useEffect, useState } from "react";
import { api } from "../api";
import type { AgentStatus } from "../bindings/AgentStatus";
import type { MonitorSettings } from "../bindings/MonitorSettings";
import { ScreenFrame } from "../components/layout";
import { Button, Callout, Card, CardHeader, StatusPill, Switch } from "../components/ui";
import { errorMessage, tildify } from "../lib/format";
import { formatThreshold, isDevBuild } from "../lib/monitor";
import { useAsync } from "../state";

export function Settings() {
  return (
    <ScreenFrame title="Settings" subtitle="Background scan and alerts">
      <div className="mx-auto flex max-w-[760px] flex-col gap-4 p-5">
        <BackgroundScanCard />
        <AlertsCard />
      </div>
    </ScreenFrame>
  );
}

const pad = (n: number) => String(n).padStart(2, "0");

function BackgroundScanCard() {
  const [status, setStatus] = useState<AgentStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.agentStatus().then(setStatus, (e) => setError(errorMessage(e)));
  }, []);

  const enabled = status !== null && (status.installed || status.loaded);
  const canInstall = status !== null && (enabled || status.resolvedCmm !== null);
  const time = status?.hour != null ? `${pad(status.hour)}:${pad(status.minute ?? 0)}` : "10:00";

  const toggle = async (next: boolean) => {
    setBusy(true);
    setError(null);
    try {
      setStatus(next ? await api.agentInstall() : await api.agentUninstall());
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <div className="flex items-start gap-4 px-4 pt-3.5 pb-3">
        <div className="min-w-0 flex-1">
          <h2 className="text-[13px] font-semibold">Daily background scan</h2>
          <p className="mt-1 text-[12.5px] leading-snug text-fg-2">
            Scans once a day at {time}, also when the app is closed, and records the result in History so you see what
            grows. It never cleans anything. A notification tells you when free space runs low or one item grows fast.
          </p>
        </div>
        <Switch checked={enabled} disabled={!canInstall || busy} onChange={toggle} label="Daily background scan" />
      </div>
      <div className="space-y-2.5 border-t border-line px-4 py-3">
        {status === null ? (
          <p className="text-[12px] text-fg-2">{error ? "Status unavailable." : "Checking…"}</p>
        ) : enabled ? (
          <InstalledDetails status={status} />
        ) : status.resolvedCmm ? (
          <Row label="Will run">
            <Code>{tildify(status.resolvedCmm)} scan --quiet --notify</Code>
            {isDevBuild(status.resolvedCmm) && <DevBuildNote />}
          </Row>
        ) : (
          <Callout tone="warn" title="The cmm command-line tool was not found">
            The background scan runs <Code>cmm</Code>. Install it with <Code>cargo install --path crates/cmm-cli</Code>{" "}
            (or put it in /usr/local/bin), then come back here.
          </Callout>
        )}
        {error && (
          <Callout tone="critical" title="Couldn't change the background scan">
            {error}
          </Callout>
        )}
      </div>
    </Card>
  );
}

function InstalledDetails({ status }: { status: AgentStatus }) {
  const healthy = status.installed && status.loaded && status.programOk;
  const failed = status.lastExitCode !== null && status.lastExitCode !== 0;
  return (
    <>
      <Row label="Status">
        {healthy && !failed ? (
          <StatusPill tone="good">On</StatusPill>
        ) : (
          <StatusPill tone="warn">Needs attention</StatusPill>
        )}
        {!status.loaded && <Hint>Not loaded by launchd — turn the switch off and on again.</Hint>}
        {!status.programOk && <Hint>The cmm binary it runs is missing — turn the switch off and on again.</Hint>}
        {failed && <Hint>The last run exited with code {status.lastExitCode}; see the log.</Hint>}
      </Row>
      {status.program && (
        <Row label="Runs">
          <Code>{tildify(status.program)} scan --quiet --notify</Code>
          {isDevBuild(status.program) && <DevBuildNote />}
        </Row>
      )}
      <Row label="Last run">
        <span className="selectable">{status.lastLogLine ?? "Not run yet"}</span>
      </Row>
      <Row label="Log">
        <span className="selectable font-mono text-[11.5px] text-fg-2">{tildify(status.logPath)}</span>
      </Row>
      <FullDiskAccessHint program={status.program} />
    </>
  );
}

function FullDiskAccessHint({ program }: { program: string | null }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <Callout
      tone="info"
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
      Give {program ? <Code>{tildify(program)}</Code> : "cmm"} Full Disk Access too, so the background scan sees the
      same folders as the app. Notifications come from Script Editor; allow them there if none show up.
      {error && <span className="block text-critical-text">{error}</span>}
    </Callout>
  );
}

function DevBuildNote() {
  return (
    <Hint>
      This is a development build; it disappears on <Code>cargo clean</Code>. Install cmm with{" "}
      <Code>cargo install --path crates/cmm-cli</Code> for a stable path.
    </Hint>
  );
}

function AlertsCard() {
  const settings = useAsync(() => api.monitorSettings(), []);
  return (
    <Card>
      <CardHeader title="Alerts" aside="From config.toml" />
      <div className="space-y-2.5 px-4 pt-2 pb-3.5">
        {settings.error ? (
          <p className="text-[12px] text-critical-text">{settings.error}</p>
        ) : settings.data === null ? (
          <p className="text-[12px] text-fg-2">Loading…</p>
        ) : (
          <AlertRows s={settings.data} />
        )}
      </div>
    </Card>
  );
}

function AlertRows({ s }: { s: MonitorSettings }) {
  const days = `${s.growthAlertDays} ${s.growthAlertDays === 1 ? "day" : "days"}`;
  return (
    <>
      <Row label="Menu bar">Turns orange below {formatThreshold(s.lowSpaceThresholdBytes)} free</Row>
      <Row label="Notify when">
        free space is below {formatThreshold(s.lowSpaceThresholdBytes)}, or one item grows more than{" "}
        {formatThreshold(s.growthAlertBytes)} in {days}
      </Row>
      <Row label="Notifications">{s.notifications ? "On" : "Off"}</Row>
      <p className="pt-1 text-[12px] leading-snug text-fg-2">
        To change them, edit <Code>{tildify(s.configPath)}</Code> and restart the app:
      </p>
      <pre className="selectable overflow-x-auto rounded-md bg-surface-2 px-3 py-2 font-mono text-[11.5px] leading-relaxed">
        {[
          `low_space_threshold_bytes = ${s.lowSpaceThresholdBytes}`,
          `growth_alert_bytes = ${s.growthAlertBytes}`,
          `growth_alert_days = ${s.growthAlertDays}`,
          `notifications = ${s.notifications}`,
        ].join("\n")}
      </pre>
    </>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[110px_minmax(0,1fr)] items-baseline gap-3 text-[12.5px]">
      <div className="text-fg-2">{label}</div>
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">{children}</div>
    </div>
  );
}

function Hint({ children }: { children: React.ReactNode }) {
  return <span className="block w-full text-[11.5px] leading-snug text-fg-2">{children}</span>;
}

function Code({ children }: { children: React.ReactNode }) {
  return <code className="selectable rounded bg-surface-2 px-1 py-px font-mono text-[11.5px]">{children}</code>;
}
