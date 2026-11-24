// Placeholder Overview. The real screens (Overview, Findings, Confirm, History) belong to the desktop stream.

import { useEffect, useState } from "react";
import { api, MOCK } from "./api";
import type { ScanProgress } from "./bindings/ScanProgress";
import type { ScanReport } from "./bindings/ScanReport";
import type { VolumeInfo } from "./bindings/VolumeInfo";

/** Decimal units, like Finder ("494.4 GB"). */
export function formatBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B` : `${value.toFixed(1)} ${units[unit]}`;
}

export function App() {
  const [volume, setVolume] = useState<VolumeInfo | null>(null);
  const [report, setReport] = useState<ScanReport | null>(null);
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.getVolume().then(setVolume, (e) => setError(String(e)));
    const unlisten = api.onScanProgress(setProgress);
    return () => {
      unlisten.then((stop) => stop());
    };
  }, []);

  async function scan() {
    setScanning(true);
    setError(null);
    try {
      const result = await api.startScan({ ruleIds: null, categories: null });
      setReport(result);
      setVolume(result.volume);
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
      setProgress(null);
    }
  }

  return (
    <main style={{ fontFamily: "system-ui, sans-serif", padding: 24 }}>
      <h1>clean-my-mac{MOCK ? " (mock)" : ""}</h1>
      <p>
        {volume
          ? `${formatBytes(volume.freeBytes)} free of ${formatBytes(volume.totalBytes)}`
          : "Reading volume information…"}
      </p>
      <button type="button" onClick={scan} disabled={scanning}>
        {scanning ? "Scanning…" : "Scan"}
      </button>
      {progress?.ruleId && (
        <p>
          Rule {progress.doneRules + 1}/{progress.totalRules}: {progress.ruleId}
        </p>
      )}
      {error && <p role="alert">{error}</p>}
      {report && (
        <ul>
          {report.findings.map((f) => (
            <li key={f.id}>
              <strong>{formatBytes(f.bytes)}</strong> {f.label} <em>({f.risk})</em> — {f.reason}
            </li>
          ))}
        </ul>
      )}
      {report && report.findings.length === 0 && <p>No findings.</p>}
    </main>
  );
}
