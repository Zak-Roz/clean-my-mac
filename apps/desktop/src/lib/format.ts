// Display formatting. Sizes use decimal units like Finder ("494.4 GB").

const UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

export function formatBytes(bytes: number): string {
  const sign = bytes < 0 ? "−" : "";
  let value = Math.abs(bytes);
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  if (unit === 0) return `${sign}${value} B`;
  // 3 significant digits keeps columns readable: 44.6 GB, 912 GB, 1.2 TB.
  const digits = value >= 100 ? 0 : 1;
  return `${sign}${value.toFixed(digits)} ${UNITS[unit]}`;
}

/** "+1.2 GB" / "−3.4 GB" / "0 B". */
export function formatDelta(bytes: number): string {
  if (bytes === 0) return "0 B";
  return bytes > 0 ? `+${formatBytes(bytes)}` : formatBytes(bytes);
}

const rtf = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

/** "just now", "12 minutes ago", "yesterday", "4 months ago". */
export function relativeTime(iso: string, now: number = Date.now()): string {
  const seconds = (new Date(iso).getTime() - now) / 1000;
  const abs = Math.abs(seconds);
  if (abs < 45) return "just now";
  const steps: [number, Intl.RelativeTimeFormatUnit][] = [
    [60, "second"],
    [60, "minute"],
    [24, "hour"],
    [30, "day"],
    [12, "month"],
    [Number.POSITIVE_INFINITY, "year"],
  ];
  let value = seconds;
  for (const [size, unit] of steps) {
    if (Math.abs(value) < size) return rtf.format(Math.round(value), unit);
    value /= size;
  }
  return rtf.format(Math.round(value), "year");
}

const dateFmt = new Intl.DateTimeFormat("en", { month: "short", day: "numeric" });
const dateTimeFmt = new Intl.DateTimeFormat("en", {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

export function formatDate(iso: string | number): string {
  return dateFmt.format(new Date(iso));
}

export function formatDateTime(iso: string | number): string {
  return dateTimeFmt.format(new Date(iso));
}

export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s} s`;
  return `${Math.floor(s / 60)} min ${s % 60} s`;
}

export function formatCount(n: number, singular: string, plural = `${singular}s`): string {
  return `${n.toLocaleString("en")} ${n === 1 ? singular : plural}`;
}

/** "/Users/you/Library/Caches/Yarn" -> "~/Library/Caches/Yarn". */
export function tildify(path: string): string {
  return path.replace(/^\/Users\/[^/]+(?=\/|$)/, "~");
}

export function basename(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  return trimmed.slice(trimmed.lastIndexOf("/") + 1) || trimmed;
}

/** Tauri rejects with a string; anything else becomes a readable message. */
export function errorMessage(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

/** Core stubs answer "not implemented: actions::plan" until their stream lands. */
export function isNotImplemented(message: string): boolean {
  return message.includes("not implemented");
}

/** "history unavailable: not implemented: History::open" -> "not implemented: History::open". */
export function stubDetail(message: string): string {
  const at = message.indexOf("not implemented");
  return at >= 0 ? message.slice(at) : message;
}
