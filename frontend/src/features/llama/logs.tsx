"use client";
import { useEffect, useRef, useState } from "react";
import { Badge, Panel } from "@/components/ui";
import { demoLogs } from "@/lib/llama-demo";
import { llamaRequest, type LlamaState, type LogLine, type LogsPage } from "@/lib/llama";
import { errorText } from "./llama";

type DisplayLine = LogLine | { divider: string; seq: number };
export function LogsPanel({ mode, enabled, visible, state, zone }: { mode: "live" | "demo"; enabled: boolean; visible: boolean; state?: LlamaState; zone: "UTC" | "local" }) {
  const [lines, setLines] = useState<DisplayLine[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [updated, setUpdated] = useState(0);
  const [paused, setPaused] = useState(false);
  const [filter, setFilter] = useState("");
  const [wrap, setWrap] = useState(false);
  const [atBottom, setAtBottom] = useState(true);
  const [refresh, setRefresh] = useState(0);
  const cursor = useRef<number | undefined>(undefined);
  const inflight = useRef(false);
  const scroll = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!enabled || !visible) return;
    let cancelled = false;
    async function load() {
      if (inflight.current) return;
      inflight.current = true;
      try {
        const page: LogsPage = mode === "demo" ? demoLogs(cursor.current) : await llamaRequest(`logs${cursor.current === undefined ? "" : `?after=${cursor.current}&limit=500`}`);
        if (cancelled) return;
        cursor.current = page.next_cursor;
        setLines((current) => {
          const seen = new Set(current.filter((line): line is LogLine => "text" in line).map((line) => line.seq));
          const extra: DisplayLine[] = page.lines.filter((line) => !seen.has(line.seq));
          if (page.truncated) extra.unshift({ divider: "…older output was dropped", seq: page.lines[0]?.seq ?? page.next_cursor });
          return [...current, ...extra].slice(-5000);
        });
        setUpdated(Date.now()); setError(null);
      } catch (e) { if (!cancelled) setError(e); }
      finally { inflight.current = false; }
    }
    void load();
    const timer = setInterval(() => void load(), state === "starting" || state === "loading" ? 1000 : 2000);
    return () => { cancelled = true; clearInterval(timer); };
  }, [mode, enabled, visible, state, refresh]);
  useEffect(() => { if (!paused && atBottom && scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight; }, [lines, paused, atBottom]);
  const shown = lines.filter((line) => "divider" in line || `${line.stream} ${line.text}`.toLowerCase().includes(filter.toLowerCase()));
  const render = (line: DisplayLine) => "divider" in line ? line.divider : `[${new Date(line.ts).toLocaleTimeString(undefined, { timeZone: zone === "UTC" ? "UTC" : undefined })}] ${line.stream}: ${line.text}`;
  const visibleText = shown.map(render).join("\n");
  const download = () => { const url = URL.createObjectURL(new Blob([lines.map(render).join("\n")], { type: "text/plain" })); const a = document.createElement("a"); a.href = url; a.download = "llama-server-logs.txt"; a.click(); URL.revokeObjectURL(url); };
  return <Panel title="Server logs" scope="Live process output · last 5,000 lines held in this tab" actions={<><small className="llama-updated">{updated ? `Updated ${new Date(updated).toLocaleTimeString()}` : "Not updated"}</small>{error && <Badge tone="bad">Stale</Badge>}</>}><div className="llama-pad">
    {!!error && <p role="alert">{errorText(error)} <button onClick={() => setRefresh((n) => n + 1)}>Retry</button></p>}
    <div className="llama-log-controls"><button onClick={() => setRefresh((n) => n + 1)}>Refresh</button><button onClick={() => setPaused(!paused)}>{paused ? "Resume auto-scroll" : "Pause auto-scroll"}</button><button onClick={() => { setPaused(false); setAtBottom(true); if (scroll.current) scroll.current.scrollTop = scroll.current.scrollHeight; }}>Jump to latest</button><button onClick={() => void navigator.clipboard.writeText(visibleText)}>Copy visible</button><button onClick={download}>Download .txt</button><label>Filter <input value={filter} onChange={(e) => setFilter(e.target.value)} /></label><label className="llama-check"><input type="checkbox" checked={wrap} onChange={(e) => setWrap(e.target.checked)} /> Wrap</label></div>
    <div ref={scroll} className={`llama-log ${wrap ? "wrap" : ""}`} role="log" aria-label="llama-server logs" onScroll={(e) => { const box = e.currentTarget; const bottom = box.scrollHeight - box.scrollTop - box.clientHeight < 24; setAtBottom(bottom); if (!bottom) setPaused(true); }}>{shown.length ? shown.map((line, i) => <div key={`${line.seq}-${i}`} className={"divider" in line ? "divider" : line.stream}>{render(line)}</div>) : <p>No log output yet.</p>}</div>
  </div></Panel>;
}
