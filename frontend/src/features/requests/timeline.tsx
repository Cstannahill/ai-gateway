"use client";
import { useMemo, useState } from "react";
import { Download } from "lucide-react";
import type { Detail, Timeline as TimelineData } from "@/lib/contracts";
import {
  download,
  duration,
  number,
  relativeMs,
  timelineRows,
} from "@/lib/metrics";
import { useRead, useRuntime } from "@/components/runtime";
import { Badge, Empty, Notice, QueryState } from "@/components/ui";
const colors: Record<string, string> = {
  content: "#29b6b1",
  reasoning: "#9d91e8",
  tool_call: "#daab63",
};
export function Timeline({ detail }: { detail: Detail }) {
  const q = useRead<TimelineData>(
      `/v1/analytics/requests/${detail.request.request_id}/timeline`,
    ),
    { mode } = useRuntime();
  const [page, setPage] = useState(0);
  const rows = useMemo(
    () => timelineRows(q.data?.events ?? [], detail.request.received_at),
    [q.data, detail.request.received_at],
  );
  const max = detail.attempts.reduce(
    (max, a) =>
      Math.max(
        max,
        relativeMs(a.first_token_at, detail.request.received_at) ?? 0,
      ),
    rows.reduce((max, r) => Math.max(max, r.relative ?? 0), 1),
  );
  // Each pixel-width bucket retains the largest genuine gap and all chunk types.
  // Exact events remain paginated below and available in the explicit export.
  const lanes = useMemo(
    () =>
      detail.attempts.map((a) => {
        const events = rows.filter(
          (r) => r.attempt_number === a.attempt_number,
        );
        const buckets = new Map<number, (typeof events)[number]>();
        for (const e of events) {
          const key = Math.floor(((e.relative ?? 0) / max) * 250);
          const prev = buckets.get(key);
          if (!prev || (e.gap ?? 0) > (prev.gap ?? 0)) buckets.set(key, e);
        }
        return { a, events, points: Array.from(buckets.values()) };
      }),
    [detail.attempts, rows, max],
  );
  return (
    <QueryState query={q}>
      {(data) => (
        <>
          <div className="section-intro">
            <div>
              <h3>Stream observations</h3>
              <p>
                {number(data.events.length)} captured events · shared axis from
                gateway receipt
              </p>
            </div>
            <button
              onClick={() =>
                download(
                  "request-timeline",
                  data,
                  `${mode}; one request; exact events returned by the timeline API`,
                )
              }
            >
              <Download size={14} />
              Export events
            </button>
          </div>
          <div className="timeline-legend">
            <span>
              <i style={{ background: colors.content }} />
              Content
            </span>
            <span>
              <i style={{ background: colors.reasoning }} />
              Reasoning
            </span>
            <span>
              <i style={{ background: colors.tool_call }} />
              Tool call
            </span>
            <span>◇ first byte &nbsp; ◆ first generation</span>
          </div>
          {!data.events.length ? (
            <Empty title="No chunk observations recorded">
              Capture may be disabled, not yet persisted, or this attempt may
              have failed before emitting chunks.
            </Empty>
          ) : (
            <div className="timeline-plot">
              <div className="timeline-axis">
                <span>0 ms</span>
                <span>{duration(max / 2)}</span>
                <span>{duration(max)}</span>
              </div>
              {lanes.map(({ a, events, points }) => (
                <div className="timeline-lane" key={a.attempt_number}>
                  <div>
                    <strong>Attempt {a.attempt_number}</strong>
                    <small>
                      {a.provider} · {events.length} events{" "}
                      {a.synthetic_timing ? "· synthetic" : ""}
                    </small>
                  </div>
                  <div
                    className={
                      "lane-track " + (a.synthetic_timing ? "synthetic" : "")
                    }
                    aria-label={`Attempt ${a.attempt_number} event lane`}
                  >
                    {points.map((e) => (
                      <span
                        className="event-tick"
                        key={e.sequence}
                        style={{
                          left: `${Math.max(0, ((e.relative ?? 0) / max) * 100)}%`,
                          background: colors[e.chunk_type] ?? "var(--muted)",
                          height: `${Math.min(44, 12 + (e.gap ?? 0) / 8)}px`,
                        }}
                        title={`#${e.sequence} ${e.chunk_type} · ${duration(e.relative)} · gap ${duration(e.gap)} · ${e.synthetic ? "synthetic" : "observed"}`}
                      />
                    ))}
                    {[
                      { at: a.first_byte_at, label: "First byte", glyph: "◇" },
                      {
                        at: a.first_token_at,
                        label: "First generation",
                        glyph: "◆",
                      },
                    ].map((marker) => {
                      const ms = relativeMs(
                        marker.at,
                        detail.request.received_at,
                      );
                      return ms === null ? null : (
                        <span
                          key={marker.label}
                          className="timeline-marker"
                          title={`${marker.label}: ${duration(ms)} from request receipt${a.synthetic_timing ? " (synthetic)" : ""}`}
                          style={{ left: `${Math.max(0, (ms / max) * 100)}%` }}
                        >
                          {marker.glyph}
                        </span>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>
          )}
          <Notice>
            Attempt start/end timestamps are not exposed, so bars do not imply
            attempt duration. First generation can be reasoning or tool output.
            Chunks are not tokens; byte counts describe extracted deltas. The
            plot uses up to 250 time buckets per lane and retains the largest
            gap in each bucket; the exact table and export are unchanged.
          </Notice>
          <div className="table-scroll">
            <table aria-label="Exact timeline events">
              <thead>
                <tr>
                  <th>Attempt / seq.</th>
                  <th>Original UTC timestamp</th>
                  <th>Request + ms</th>
                  <th>Type</th>
                  <th>Bytes</th>
                  <th>Gap · ms</th>
                  <th>Evidence</th>
                </tr>
              </thead>
              <tbody>
                {rows.slice(page * 100, page * 100 + 100).map((e, i) => (
                  <tr key={page * 100 + i}>
                    <td className="mono">
                      {e.attempt_number} / {e.sequence}
                    </td>
                    <td className="mono nowrap">{e.timestamp}</td>
                    <td className="mono">{number(e.relative, 6)}</td>
                    <td>{e.chunk_type}</td>
                    <td>{number(e.byte_count)}</td>
                    <td className="mono">{number(e.gap, 6)}</td>
                    <td>
                      <Badge tone={e.synthetic ? "warn" : "neutral"}>
                        {e.synthetic ? "Synthetic" : "Observed"}
                      </Badge>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {rows.length > 100 && (
            <div className="pagination">
              <span>
                Exact events {page * 100 + 1}–
                {Math.min(rows.length, page * 100 + 100)} / {rows.length}
              </span>
              <button disabled={!page} onClick={() => setPage(page - 1)}>
                Previous
              </button>
              <button
                disabled={(page + 1) * 100 >= rows.length}
                onClick={() => setPage(page + 1)}
              >
                Next
              </button>
            </div>
          )}
        </>
      )}
    </QueryState>
  );
}
