"use client";
import { useEffect, useState } from "react";
import { Activity, ArrowUpRight, Server } from "lucide-react";
import Link from "next/link";
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from "recharts";
import type { Health, Metrics, Provider } from "@/lib/contracts";
import {
  deltaSample,
  number,
  ratio,
  percent,
  uptime,
  type Counter,
} from "@/lib/metrics";
import { useRead, useRuntime } from "@/components/runtime";
import { Badge, Notice, Panel, QueryState, Stat } from "@/components/ui";
import { ComparisonView } from "./comparison";
import { Explorer } from "./requests/explorer";
function SessionActivity({
  metrics,
  updated,
  error,
}: {
  metrics?: Metrics;
  updated: number;
  error: boolean;
}) {
  const { mode, paused } = useRuntime();
  const [samples, setSamples] = useState<{
    previous: Counter | null;
    rows: { time: string; requests: number | null }[];
  }>({ previous: null, rows: [] });
  useEffect(() => {
    if (!metrics || !updated) return;
    const next = {
      at: updated,
      count: metrics.gateway.total_requests,
      uptime: metrics.gateway.uptime_seconds,
    };
    // Synchronize an external query observation into a bounded browser-session history.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSamples((s) => ({
      previous: error ? null : next,
      rows: [
        ...s.rows,
        {
          time: new Date(updated).toLocaleTimeString("en-US", {
            hour12: false,
          }),
          requests: error ? null : deltaSample(s.previous, next),
        },
      ].slice(-120),
    }));
  }, [metrics, updated, error]);
  return (
    <Panel
      title="Request activity"
      scope="Since opening this dashboard · counter deltas per observed interval"
      actions={
        <Badge>
          <Activity size={12} />
          {mode === "demo"
            ? "Fixed demo snapshot"
            : paused
              ? "Sampling paused"
              : "5 second sampling"}
        </Badge>
      }
    >
      {samples.rows.filter((r) => r.requests !== null).length < 2 ? (
        <div className="activity-wait">
          <Activity size={26} />
          <h3>
            {mode === "demo"
              ? "Activity starts with observation"
              : "Listening for request activity"}
          </h3>
          <p>
            {mode === "demo"
              ? "Demo counters are fixed. No historical traffic has been fabricated."
              : "At least three successful samples are needed. Outages and counter resets leave gaps."}
          </p>
        </div>
      ) : (
        <div className="chart activity-chart">
          <ResponsiveContainer width="100%" height="100%">
            <LineChart data={samples.rows}>
              <CartesianGrid stroke="var(--border)" vertical={false} />
              <XAxis
                dataKey="time"
                minTickGap={45}
                tick={{ fill: "var(--muted)", fontSize: 10 }}
                axisLine={false}
              />
              <YAxis
                width={34}
                allowDecimals={false}
                tick={{ fill: "var(--muted)", fontSize: 10 }}
                axisLine={false}
              />
              <Tooltip
                contentStyle={{
                  background: "var(--panel)",
                  borderColor: "var(--border)",
                }}
              />
              <Line
                dataKey="requests"
                name="Requests in interval"
                stroke="var(--accent)"
                strokeWidth={2}
                dot={false}
                connectNulls={false}
                isAnimationActive={false}
              />
            </LineChart>
          </ResponsiveContainer>
        </div>
      )}
      <div className="chart-note">
        Up to 120 samples in browser memory. Navigation away clears this session
        chart. No persisted time series is available.
      </div>
    </Panel>
  );
}
export function Overview() {
  const health = useRead<Health>("/health", 30000),
    metrics = useRead<Metrics>("/metrics", 5000),
    providers = useRead<{ providers: Provider[] }>("/providers", 30000),
    { mode } = useRuntime();
  return (
    <>
      <div className="status-line">
        <Server size={15} />
        <QueryState query={health}>
          {(h) => (
            <>
              <Badge tone={h.status === "healthy" ? "good" : "warn"}>
                {h.status}
              </Badge>
              <span>
                AI-Gateway <span className="mono">v{h.version}</span>
              </span>
              <span className="muted">
                Uptime {uptime(metrics.data?.gateway.uptime_seconds)}
              </span>
            </>
          )}
        </QueryState>
        <span className="status-disclaimer">
          Reachability does not certify durable capture
        </span>
      </div>
      <QueryState query={metrics}>
        {(m) => (
          <div className="stats-grid">
            <Stat
              label="Gateway requests"
              value={number(m.gateway.total_requests)}
              detail="Since gateway restart"
            />
            <Stat
              label="Active provider attempts"
              value={number(m.gateway.active_requests)}
              detail="Currently in flight"
            />
            <Stat
              label="Provider attempt errors"
              value={number(m.gateway.total_provider_errors)}
              detail={`${percent(ratio(m.gateway.total_provider_errors, m.gateway.total_provider_attempts))} of ${number(m.gateway.total_provider_attempts)} attempts · since restart`}
              tone={m.gateway.total_provider_errors ? "warning" : ""}
            />
            <Stat
              label="Registered providers"
              value={health.data ? number(health.data.providers) : "—"}
              detail="Current gateway registration"
            />
          </div>
        )}
      </QueryState>
      <div className="overview-middle">
        <SessionActivity
          key={mode}
          metrics={metrics.data}
          updated={metrics.dataUpdatedAt}
          error={metrics.isError}
        />
        <Panel
          title="Provider health"
          scope="Current instance status"
          actions={
            <Link
              aria-label="Open provider inventory"
              href={"/?view=inventory" + (mode === "demo" ? "&demo=1" : "")}
            >
              <ArrowUpRight size={16} />
            </Link>
          }
        >
          <QueryState query={providers}>
            {(d) => (
              <div className="provider-list">
                {d.providers.length ? (
                  d.providers.map((p) => (
                    <div key={p.name}>
                      <div className="provider-avatar">
                        <Server size={17} />
                      </div>
                      <div>
                        <strong>{p.name}</strong>
                        <small>{p.provider_type}</small>
                      </div>
                      <div className="provider-state">
                        <Badge
                          tone={
                            !p.enabled
                              ? "neutral"
                              : p.health === "healthy"
                                ? "good"
                                : p.health === "unhealthy"
                                  ? "bad"
                                  : "warn"
                          }
                        >
                          {!p.enabled ? "Disabled" : p.health}
                        </Badge>
                        <small>{p.active_requests} active attempts</small>
                      </div>
                    </div>
                  ))
                ) : (
                  <p className="muted">No providers registered.</p>
                )}
              </div>
            )}
          </QueryState>
        </Panel>
      </div>
      <ComparisonView compact />
      <Explorer compact />
      <Notice>
        Usage and spend summaries need additional read contracts. Individual
        usage observations are available inside requests; fleet totals and
        pricing coverage are not exposed.
      </Notice>
    </>
  );
}
