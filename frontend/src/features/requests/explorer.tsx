"use client";
import { useMemo, useState, type FormEvent } from "react";
import Link from "next/link";
import { useSearchParams, useRouter } from "next/navigation";
import type { ColumnDef } from "@tanstack/react-table";
import {
  ArrowRight,
  ChevronLeft,
  ChevronRight,
  Download,
  RefreshCw,
  Search,
} from "lucide-react";
import type { RequestList, RequestSummary } from "@/lib/contracts";
import {
  csvCell,
  duration,
  milliseconds,
  saveBlob,
  timestamp,
} from "@/lib/metrics";
import { isRequestId } from "@/lib/proxy-policy";
import { useRead, useRuntime } from "@/components/runtime";
import { DataTable } from "@/components/data-table";
import { Badge, Notice, Outcome, Panel, QueryState } from "@/components/ui";
export function requestHref(params: URLSearchParams, id: string) {
  const p = new URLSearchParams(params);
  p.set("request", id);
  return "/?" + p.toString();
}
export function RequestTable({ rows }: { rows: RequestSummary[] }) {
  const { zone } = useRuntime(),
    params = useSearchParams();
  const columns = useMemo<ColumnDef<RequestSummary>[]>(
    () => [
      {
        accessorKey: "received_at",
        header: `Received · ${zone}`,
        cell: ({ row }) => (
          <span className="mono nowrap" title={row.original.received_at}>
            {timestamp(row.original.received_at, zone)}
          </span>
        ),
      },
      {
        accessorKey: "request_id",
        header: "Request ID",
        enableHiding: false,
        cell: ({ getValue }) => (
          <Link
            scroll={false}
            className="id-link mono"
            title={getValue<string>()}
            href={requestHref(new URLSearchParams(params), getValue<string>())}
          >
            {getValue<string>().slice(-12)}
            <ArrowRight size={12} />
          </Link>
        ),
      },
      { accessorKey: "requested_model", header: "Requested model" },
      {
        accessorKey: "resolved_model",
        header: "Resolved model",
        cell: ({ getValue }) => (
          <span className="truncate" title={getValue<string>()}>
            {getValue<string>() ?? "—"}
          </span>
        ),
      },
      {
        accessorKey: "serving_provider",
        header: "Serving provider",
        cell: ({ getValue }) => (
          <span className="mono">{getValue<string>() ?? "—"}</span>
        ),
      },
      {
        accessorKey: "success",
        header: "Outcome",
        cell: ({ row }) => <Outcome value={row.original.success} />,
      },
      {
        accessorKey: "streaming",
        header: "Mode",
        cell: ({ getValue }) => (
          <Badge>
            {getValue() === null
              ? "Unknown"
              : getValue()
                ? "Stream"
                : "Response"}
          </Badge>
        ),
      },
      {
        accessorKey: "total_latency_us",
        header: "Latency",
        cell: ({ getValue }) => (
          <span className="mono">
            {duration(milliseconds(getValue<number | null>()))}
          </span>
        ),
      },
    ],
    [zone, params],
  );
  return (
    <DataTable
      rows={rows}
      columns={columns}
      visibility
      label="Recorded requests"
    />
  );
}
export function Explorer({ compact = false }: { compact?: boolean }) {
  const params = useSearchParams(),
    router = useRouter();
  const { mode } = useRuntime();
  const limit = compact
    ? 8
    : [25, 50, 100].includes(Number(params.get("limit")))
      ? Number(params.get("limit"))
      : 25;
  const offset = compact
    ? 0
    : Math.min(
        1000000,
        Math.max(0, Math.floor(Number(params.get("offset")) || 0)),
      );
  const q = new URLSearchParams({
    limit: String(limit),
    offset: String(offset),
  });
  if (!compact)
    for (const key of ["provider", "model", "success"]) {
      const v = params.get(key);
      if (v) q.set(key, v);
    }
  const path = "/v1/analytics/requests?" + q;
  // The visible page does not poll. A separate head probe offers an explicit refresh.
  const query = useRead<RequestList>(path),
    head = useRead<RequestList>(
      path.replace(/offset=\d+/, "offset=0"),
      30000,
      !compact && offset === 0,
      "head-probe",
    );
  const newRows =
    !!query.data &&
    !!head.data &&
    query.data.requests[0]?.request_id !== head.data.requests[0]?.request_id;
  const [exact, setExact] = useState(""),
    [idError, setIdError] = useState("");
  function update(values: Record<string, string>) {
    const p = new URLSearchParams(params);
    for (const [key, value] of Object.entries(values)) {
      if (value) p.set(key, value);
      else p.delete(key);
    }
    router.push("/?" + p, { scroll: false });
  }
  function filter(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    update({
      provider: String(form.get("provider") ?? "").trim(),
      model: String(form.get("model") ?? "").trim(),
      success: String(form.get("success") ?? ""),
      offset: "0",
    });
  }
  function go(event: FormEvent) {
    event.preventDefault();
    if (!isRequestId(exact.trim())) {
      setIdError("Enter a complete gateway UUID.");
      return;
    }
    setIdError("");
    update({ request: exact.trim() });
  }
  function exportCsv() {
    if (!query.data) return;
    const fields = [
      "received_at",
      "request_id",
      "requested_model",
      "resolved_model",
      "serving_provider",
      "success",
      "streaming",
      "total_latency_us",
    ] as const;
    const scope = `${mode}; loaded request page; ${q}; sorting is local`;
    const generated = new Date().toISOString();
    const text = [
      ["scope", "generated_at", ...fields].map(csvCell).join(","),
      ...query.data.requests.map((r) =>
        [scope, generated, ...fields.map((k) => r[k])].map(csvCell).join(","),
      ),
    ].join("\r\n");
    saveBlob(
      "gateway-loaded-requests.csv",
      new Blob([text], { type: "text/csv;charset=utf-8" }),
    );
  }
  return (
    <Panel
      title={compact ? "Recent requests" : "Recorded requests"}
      scope={
        compact
          ? "Latest 8 recorded requests · asynchronous capture"
          : "Newest first · exact server filters · no total count is exposed"
      }
      actions={
        <div className="actions">
          {!compact && (
            <button onClick={exportCsv} disabled={!query.data}>
              <Download size={14} />
              CSV
            </button>
          )}
          <button onClick={() => query.refetch()} disabled={query.isFetching}>
            <RefreshCw size={14} />
            Refresh
          </button>
          {compact && (
            <Link
              className="button"
              href={"/?view=requests" + (mode === "demo" ? "&demo=1" : "")}
            >
              Explore all
              <ArrowRight size={14} />
            </Link>
          )}
        </div>
      }
    >
      {!compact && (
        <>
          <form
            className="filters"
            onSubmit={filter}
            key={
              params.get("provider") +
              ":" +
              params.get("model") +
              ":" +
              params.get("success")
            }
          >
            <label>
              Serving provider
              <input
                name="provider"
                placeholder="All providers"
                defaultValue={params.get("provider") ?? ""}
              />
            </label>
            <label>
              Requested or resolved model
              <input
                name="model"
                placeholder="All models"
                defaultValue={params.get("model") ?? ""}
              />
            </label>
            <label>
              Outcome
              <select name="success" defaultValue={params.get("success") ?? ""}>
                <option value="">All outcomes</option>
                <option value="true">Succeeded</option>
                <option value="false">Failed</option>
              </select>
            </label>
            <button className="primary" type="submit">
              <Search size={14} />
              Apply filters
            </button>
            <button
              type="button"
              onClick={() =>
                update({ model: "", provider: "", success: "", offset: "0" })
              }
            >
              Reset
            </button>
          </form>
          <form className="exact-search" onSubmit={go}>
            <Search size={15} />
            <input
              aria-label="Exact gateway request ID"
              placeholder="Open an exact gateway request UUID…"
              value={exact}
              onChange={(e) => setExact(e.target.value)}
            />
            <button>
              Open request
              <ArrowRight size={13} />
            </button>
            {idError && <span role="alert">{idError}</span>}
          </form>
          {newRows && (
            <div className="new-rows">
              <button onClick={() => query.refetch()}>
                New requests available — load latest
              </button>
              <span>Your current rows have been kept in place.</span>
            </div>
          )}
        </>
      )}
      <QueryState query={query}>
        {(data) => (
          <>
            <RequestTable rows={data.requests} />
            {!compact && (
              <div className="pagination">
                <span>
                  {data.requests.length} loaded · offset {offset}{" "}
                  <span className="muted">/ total unavailable</span>
                </span>
                <label>
                  Rows
                  <select
                    value={limit}
                    onChange={(e) =>
                      update({ limit: e.target.value, offset: "0" })
                    }
                  >
                    {[25, 50, 100].map((n) => (
                      <option key={n}>{n}</option>
                    ))}
                  </select>
                </label>
                <button
                  aria-label="Previous page"
                  disabled={offset === 0}
                  onClick={() =>
                    update({ offset: String(Math.max(0, offset - limit)) })
                  }
                >
                  <ChevronLeft size={15} />
                </button>
                <button
                  aria-label="Next page"
                  disabled={
                    data.requests.length < limit || offset + limit > 1000000
                  }
                  onClick={() => update({ offset: String(offset + limit) })}
                >
                  <ChevronRight size={15} />
                </button>
              </div>
            )}
          </>
        )}
      </QueryState>
      {!compact && (
        <Notice>
          Offset pages can shift as records arrive. No total or stable cursor is
          available; an empty next page means you have reached the current end.
          Filters apply to the final serving provider.
        </Notice>
      )}
    </Panel>
  );
}
