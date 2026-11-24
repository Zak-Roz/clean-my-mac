// Helpers for the Settings screen (background scan, alert thresholds).

import { formatBytes } from "./format";

/** A binary inside a Cargo target/ directory vanishes on `cargo clean`. */
export function isDevBuild(path: string): boolean {
  return /\/target\/(debug|release)\//.test(path);
}

/** "50 GB" for round thresholds, otherwise the usual 3 significant digits. */
export function formatThreshold(bytes: number): string {
  return bytes >= 1e9 && bytes % 1e9 === 0 ? `${bytes / 1e9} GB` : formatBytes(bytes);
}
