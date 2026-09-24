import type { Nullable, StreamEvent, Usage } from "./contracts.ts";
export const milliseconds = (us: Nullable) =>
  us == null || !Number.isFinite(us) ? null : us / 1000;
export const ratio = (n: Nullable, d: Nullable) =>
  n == null || d == null || d <= 0 || !Number.isFinite(n / d) ? null : n / d;
export const number = (n: Nullable, digits = 0) =>
  n == null || !Number.isFinite(n)
    ? "—"
    : new Intl.NumberFormat("en-US", { maximumFractionDigits: digits }).format(
        n,
      );
export const duration = (ms: Nullable) =>
  ms == null
    ? "—"
    : ms >= 1000
      ? `${number(ms / 1000, 2)} s`
      : `${number(ms, 2)} ms`;
export const percent = (n: Nullable) =>
  n == null ? "—" : `${number(n * 100, 1)}%`;
export const uptime = (s: Nullable) =>
  s == null
    ? "—"
    : s >= 86400
      ? `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`
      : `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
export function timestamp(s: string, zone: "UTC" | "local") {
  return new Intl.DateTimeFormat("en-US", {
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
    ...(zone === "UTC" ? { timeZone: "UTC" } : {}),
  }).format(new Date(s));
}
export interface Counter {
  count: number;
  uptime: number;
  at: number;
}
export function deltaSample(previous: Counter | null, next: Counter) {
  if (
    !previous ||
    next.at - previous.at > 12000 ||
    next.at <= previous.at ||
    next.count < previous.count ||
    next.uptime < previous.uptime
  )
    return null;
  return next.count - previous.count;
}
// Retain the original timestamp. Date.parse is used only for the whole second;
// BigInt carries fractional nanoseconds through subtraction before conversion.
function nanos(s: string): bigint | null {
  const m = s.match(
    /^(.*T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})$/,
  );
  if (!m) return null;
  const ms = Date.parse(m[1] + m[3]);
  return Number.isFinite(ms)
    ? BigInt(ms) * 1000000n + BigInt((m[2] ?? "").padEnd(9, "0"))
    : null;
}
export function relativeMs(s: string | null, origin: string) {
  if (!s) return null;
  const a = nanos(s),
    b = nanos(origin);
  return a === null || b === null ? null : Number(a - b) / 1000000;
}
export function timelineRows(events: StreamEvent[], origin: string) {
  const previous = new Map<number, StreamEvent>();
  return [...events]
    .sort(
      (a, b) => a.attempt_number - b.attempt_number || a.sequence - b.sequence,
    )
    .map((e) => {
      const p = previous.get(e.attempt_number);
      const rawGap =
        p && !p.synthetic && !e.synthetic
          ? relativeMs(e.timestamp, p.timestamp)
          : null;
      const gap =
        rawGap != null && rawGap >= 0 && p?.sequence === e.sequence - 1
          ? rawGap
          : null;
      previous.set(e.attempt_number, e);
      return { ...e, relative: relativeMs(e.timestamp, origin), gap };
    });
}
export function usageWarnings(u: Usage, records: number) {
  const warnings: string[] = [];
  if (records > 1)
    warnings.push(
      "Multiple observations; final usage cannot be identified. Do not sum.",
    );
  if ([u.prompt_tokens, u.completion_tokens, u.total_tokens].includes(0))
    warnings.push("Zero may represent missing provider usage in this backend.");
  if (u.cached_tokens != null && u.cached_tokens > u.prompt_tokens)
    warnings.push("Cached subset exceeds prompt tokens.");
  if (u.reasoning_tokens != null && u.reasoning_tokens > u.completion_tokens)
    warnings.push("Reasoning subset exceeds completion tokens.");
  if (u.total_tokens !== u.prompt_tokens + u.completion_tokens)
    warnings.push("Reported total differs from prompt + completion.");
  return warnings;
}
export function transition(
  a: { provider: string; model: string },
  b: { provider: string; model: string },
) {
  return a.provider !== b.provider
    ? "Provider switch"
    : a.model !== b.model
      ? "Model switch"
      : "Same-deployment retry";
}
export function csvCell(value: unknown) {
  let s = value == null ? "" : String(value);
  if (/^[\s]*[=+\-@\t\r]/.test(s)) s = "'" + s;
  return '"' + s.replaceAll('"', '""') + '"';
}
export function download(name: string, data: unknown, scope: string) {
  const blob = new Blob(
    [
      JSON.stringify(
        { scope, generated_at: new Date().toISOString(), data },
        null,
        2,
      ),
    ],
    { type: "application/json" },
  );
  saveBlob(name + ".json", blob);
}
export function saveBlob(name: string, blob: Blob) {
  const url = URL.createObjectURL(blob),
    a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
export function providerColor(name: string) {
  const colors = [
    "#29b6b1",
    "#8b8ee8",
    "#daab63",
    "#5a9bd5",
    "#d77ba2",
    "#83b871",
  ];
  let h = 0;
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return colors[h % colors.length];
}
