"use client";
import { useMemo, useState } from "react";
import Link from "next/link";
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  Cell,
} from "recharts";
import type { ColumnDef } from "@tanstack/react-table";
import { Download } from "lucide-react";
import type { Comparison } from "@/lib/contracts";
import {
  download,
  duration,
  number,
  percent,
  providerColor,
  ratio,
} from "@/lib/metrics";
import { useRead, useRuntime } from "@/components/runtime";
import { Panel, QueryState, Notice, Empty } from "@/components/ui";
import { DataTable } from "@/components/data-table";
const metrics = {
  avg_duration_ms: "Mean recorded duration",
  avg_ttft_ms: "Mean attempt TTFT",
  avg_tokens_per_sec: "Reported output throughput",
  total_attempts: "Reported attempts",
  success_rate: "Reported attempt success",
};
type Metric = keyof typeof metrics;
export function ComparisonView({ compact = false }: { compact?: boolean }) {
  const query = useRead<{ providers: Comparison[] }>(
      "/v1/analytics/providers/comparison",
      60000,
    ),
    { mode } = useRuntime();
  const [metric, setMetric] = useState<Metric>("avg_duration_ms"),
    [selected, setSelected] = useState<string[]>([]);
  const columns = useMemo<ColumnDef<Comparison>[]>(
    () => [
      {
        accessorKey: "provider",
        header: "Provider / model",
        cell: ({ row }) => (
          <Link
            className="pair-link"
            href={
              "/?" +
              new URLSearchParams({
                view: "requests",
                provider: row.original.provider,
                model: row.original.model,
                ...(mode === "demo" ? { demo: "1" } : {}),
              })
            }
          >
            <span>
              <i style={{ background: providerColor(row.original.provider) }} />
              {row.original.provider}
            </span>
            <small>{row.original.model}</small>
          </Link>
        ),
      },
      {
        accessorKey: "total_attempts",
        header: "Reported attempts",
        cell: ({ getValue }) => number(getValue<number>()),
      },
      {
        id: "success",
        accessorFn: (r) => ratio(r.successful_attempts, r.total_attempts),
        header: "Reported success",
        cell: ({ getValue }) => percent(getValue<number | null>()),
      },
      {
        accessorKey: "avg_duration_ms",
        header: "Mean duration",
        cell: ({ getValue }) => duration(getValue<number | null>()),
      },
      {
        accessorKey: "avg_ttft_ms",
        header: "Mean attempt TTFT",
        cell: ({ getValue }) => duration(getValue<number | null>()),
      },
      {
        accessorKey: "avg_tokens_per_sec",
        header: "Output tok/s",
        cell: ({ getValue }) => number(getValue<number | null>(), 1),
      },
    ],
    [mode],
  );
  return (
    <Panel
      title={compact ? "Provider performance" : "Compare observed performance"}
      scope="All retained history · provider / model pairs"
      actions={
        <select
          aria-label="Comparison metric"
          value={metric}
          onChange={(e) => setMetric(e.target.value as Metric)}
        >
          {Object.entries(metrics).map(([key, label]) => (
            <option key={key} value={key}>
              {label}
            </option>
          ))}
        </select>
      }
    >
      <QueryState query={query}>
        {(data) => {
          const visible = data.providers
            .filter(
              (p) =>
                !selected.length ||
                selected.includes(p.provider + "/" + p.model),
            )
            .slice(0, 6);
          const chart = visible.map((p) => ({
            ...p,
            label: p.provider + " / " + p.model,
            value:
              metric === "success_rate"
                ? ratio(p.successful_attempts, p.total_attempts) === null
                  ? null
                  : ratio(p.successful_attempts, p.total_attempts)! * 100
                : p[metric],
          }));
          return !data.providers.length ? (
            <Empty />
          ) : (
            <>
              {!compact && (
                <div className="pair-select">
                  <span>Select up to 6 pairs · default first 6</span>
                  {data.providers.map((p) => {
                    const id = p.provider + "/" + p.model;
                    return (
                      <label key={id}>
                        <input
                          type="checkbox"
                          checked={selected.includes(id)}
                          disabled={
                            !selected.includes(id) && selected.length >= 6
                          }
                          onChange={(e) =>
                            setSelected(
                              e.target.checked
                                ? [...selected, id]
                                : selected.filter((v) => v !== id),
                            )
                          }
                        />
                        {p.provider} / {p.model}
                      </label>
                    );
                  })}
                </div>
              )}
              <div
                className="chart"
                role="img"
                aria-label={
                  metrics[metric] +
                  " by provider and model; exact values in the table below"
                }
              >
                <ResponsiveContainer width="100%" height="100%">
                  <BarChart
                    data={chart}
                    layout="vertical"
                    margin={{ top: 6, right: 25, bottom: 5, left: 4 }}
                  >
                    <CartesianGrid stroke="var(--border)" horizontal={false} />
                    <XAxis
                      type="number"
                      tick={{ fill: "var(--muted)", fontSize: 11 }}
                      axisLine={false}
                      tickLine={false}
                      unit={
                        metric === "success_rate"
                          ? "%"
                          : metric === "avg_tokens_per_sec"
                            ? " t/s"
                            : metric === "total_attempts"
                              ? ""
                              : " ms"
                      }
                    />
                    <YAxis
                      dataKey="provider"
                      type="category"
                      width={98}
                      tick={{ fill: "var(--text)", fontSize: 12 }}
                      axisLine={false}
                      tickLine={false}
                    />
                    <Tooltip
                      cursor={{ fill: "var(--hover)" }}
                      contentStyle={{
                        background: "var(--panel)",
                        borderColor: "var(--border)",
                        borderRadius: 6,
                        color: "var(--text)",
                      }}
                      labelFormatter={(_, p) => p[0]?.payload?.label ?? ""}
                    />
                    <Bar
                      dataKey="value"
                      name={metrics[metric]}
                      radius={[0, 3, 3, 0]}
                      barSize={16}
                      isAnimationActive={false}
                    >
                      {chart.map((p) => (
                        <Cell key={p.label} fill={providerColor(p.provider)} />
                      ))}
                    </Bar>
                  </BarChart>
                </ResponsiveContainer>
              </div>
              <DataTable
                rows={compact ? visible : data.providers}
                columns={columns}
                label="Provider comparison"
              />
              {!compact && (
                <div className="panel-footer">
                  <button
                    onClick={() =>
                      download(
                        "gateway-comparison",
                        data.providers,
                        `${mode}; all retained server aggregates; usage-join caveat applies`,
                      )
                    }
                  >
                    <Download size={14} />
                    Export comparison JSON
                  </button>
                </div>
              )}
            </>
          );
        }}
      </QueryState>
      <Notice>
        Server-reported aggregates. Repeated usage observations can inflate
        counts and weight averages. Synthetic timing is excluded; timing sample
        counts are unavailable. Throughput divides completion tokens by recorded
        duration, whose streaming clock starts after dispatch returns. It is not
        pure decode speed or verified end-to-end throughput.
      </Notice>
    </Panel>
  );
}
