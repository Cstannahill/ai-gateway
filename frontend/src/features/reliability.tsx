"use client";
import { useMemo } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import type { ColumnDef } from "@tanstack/react-table";
import { ArrowRight, Download } from "lucide-react";
import type { Recovery } from "@/lib/contracts";
import { download, number } from "@/lib/metrics";
import { useRead, useRuntime } from "@/components/runtime";
import { DataTable } from "@/components/data-table";
import { Badge, Empty, Notice, Panel, QueryState, Stat } from "@/components/ui";
import { requestHref } from "./requests/explorer";
export function Reliability() {
  const q = useRead<{ fallbacks: Recovery[] }>(
      "/v1/analytics/fallbacks",
      60000,
    ),
    params = useSearchParams(),
    { mode } = useRuntime();
  const cols = useMemo<ColumnDef<Recovery>[]>(
    () => [
      {
        accessorKey: "request_id",
        header: "Request ID",
        cell: ({ getValue }) => (
          <Link
            className="mono id-link"
            href={requestHref(new URLSearchParams(params), getValue<string>())}
          >
            {getValue<string>().slice(-12)}
            <ArrowRight size={12} />
          </Link>
        ),
      },
      { accessorKey: "failed_provider", header: "Failed provider" },
      { accessorKey: "failed_model", header: "Failed model" },
      { accessorKey: "succeeded_provider", header: "Final provider" },
      {
        accessorKey: "error_code",
        header: "Error category",
        cell: ({ getValue }) => (
          <Badge tone="warn">{getValue<string>() ?? "Not recorded"}</Badge>
        ),
      },
      { accessorKey: "error_message", header: "Error detail" },
    ],
    [params],
  );
  return (
    <QueryState query={q}>
      {(d) => {
        const pairs = new Map<string, number>();
        for (const r of d.fallbacks) {
          const key =
            r.failed_provider + " → " + (r.succeeded_provider ?? "Unknown");
          pairs.set(key, (pairs.get(key) ?? 0) + 1);
        }
        const maximum = Math.max(1, ...pairs.values());
        return (
          <>
            <div className="stats-grid three">
              <Stat
                label="Distinct recovered requests"
                value={number(
                  new Set(d.fallbacks.map((f) => f.request_id)).size,
                )}
                detail="Within returned recovery sample"
              />
              <Stat
                label="Failed-attempt rows"
                value={number(d.fallbacks.length)}
                detail="Latest 200 rows maximum"
              />
              <Stat
                label="Same-provider recovery rows"
                value={number(
                  d.fallbacks.filter(
                    (f) => f.failed_provider === f.succeeded_provider,
                  ).length,
                )}
                detail="May include retries or model switches"
              />
            </div>
            <Panel
              title="Recovery paths"
              scope="Recent recovery sample · each count is a failed-attempt row"
            >
              {pairs.size ? (
                <div className="recovery-paths">
                  {Array.from(pairs)
                    .sort((a, b) => b[1] - a[1])
                    .map(([label, count]) => (
                      <div key={label}>
                        <span className="mono">{label}</span>
                        <div>
                          <i style={{ width: `${(count / maximum) * 100}%` }} />
                        </div>
                        <strong>{count}</strong>
                      </div>
                    ))}
                </div>
              ) : (
                <Empty title="No recovered requests in the sample" />
              )}
              <Notice>
                This endpoint returns at most 200 failed-attempt rows from
                successful requests, so the last request can be truncated. It
                excludes unrecovered failures and has no fleet-wide denominator.
                These are not overall fallback rates.
              </Notice>
            </Panel>
            <Panel
              title="Recovered request evidence"
              scope="Failed attempt → final serving provider"
              actions={
                <button
                  onClick={() =>
                    download(
                      "gateway-recovery-sample",
                      d.fallbacks,
                      `${mode}; up to 200 failed-attempt rows in recovered requests`,
                    )
                  }
                >
                  <Download size={14} />
                  Export sample
                </button>
              }
            >
              <DataTable
                rows={d.fallbacks}
                columns={cols}
                visibility
                label="Recovered requests"
              />
            </Panel>
          </>
        );
      }}
    </QueryState>
  );
}
