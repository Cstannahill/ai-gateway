"use client";
import { useMemo, useState } from "react";
import type { ColumnDef } from "@tanstack/react-table";
import { Check, Minus, Server, Search } from "lucide-react";
import type { Model, Provider } from "@/lib/contracts";
import { duration, number } from "@/lib/metrics";
import { useRead } from "@/components/runtime";
import { Badge, Notice, Panel, QueryState } from "@/components/ui";
import { DataTable } from "@/components/data-table";
export function Inventory() {
  const providers = useRead<{ providers: Provider[] }>("/providers", 30000),
    models = useRead<{ data: Model[] }>("/v1/models", 120000);
  const [search, setSearch] = useState("");
  const cols = useMemo<ColumnDef<Model>[]>(
    () => [
      {
        accessorKey: "id",
        header: "Model / alias",
        cell: ({ getValue }) => (
          <span className="mono">{getValue<string>()}</span>
        ),
      },
      { accessorKey: "owned_by", header: "Catalog owner" },
      {
        id: "providers",
        accessorFn: (r) => r.providers.join(", "),
        header: "Provider instances",
      },
      ...(
        [
          "tools",
          "vision",
          "reasoning",
          "streaming",
          "structured_output",
          "embeddings",
        ] as const
      ).map((k) => ({
        id: k,
        accessorFn: (r: Model) => r.capabilities[k],
        header: k.replace("_", " "),
        cell: ({ getValue }: { getValue: () => unknown }) =>
          getValue() ? (
            <span className="capability">
              <Check size={13} />
              Yes
            </span>
          ) : (
            <span className="muted">
              <Minus size={13} />
              <span className="sr-only">No</span>
            </span>
          ),
      })),
      {
        id: "context",
        accessorFn: (r) => r.capabilities.context_window,
        header: "Context tokens",
        cell: ({ getValue }) => number(getValue<number | null>()),
      },
    ],
    [],
  );
  return (
    <>
      <Panel
        title="Provider instances"
        scope="Current status · counters since gateway restart"
      >
        <QueryState query={providers}>
          {(d) => (
            <div className="inventory-grid">
              {d.providers.map((p) => (
                <article className="inventory-card" key={p.name}>
                  <div className="section-intro">
                    <Server size={21} />
                    <Badge
                      tone={
                        !p.enabled
                          ? "neutral"
                          : p.health === "healthy"
                            ? "good"
                            : "warn"
                      }
                    >
                      {!p.enabled ? "Disabled" : p.health}
                    </Badge>
                  </div>
                  <h3>{p.name}</h3>
                  <p>{p.provider_type}</p>
                  <dl>
                    <div>
                      <dt>Active attempts</dt>
                      <dd>{p.active_requests}</dd>
                    </div>
                    <div>
                      <dt>Total attempts</dt>
                      <dd>{number(p.total_requests)}</dd>
                    </div>
                    <div>
                      <dt>Attempt errors</dt>
                      <dd>{number(p.total_errors)}</dd>
                    </div>
                    <div>
                      <dt>Mean recorded inference latency</dt>
                      <dd>{duration(p.avg_latency_ms)}</dd>
                    </div>
                  </dl>
                </article>
              ))}
            </div>
          )}
        </QueryState>
      </Panel>
      <Panel
        title="Model catalog"
        scope="Configured aliases and provider discovery · capability declarations"
        actions={
          <label className="search-label">
            <Search size={14} />
            <input
              aria-label="Search model catalog"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Search names or capabilities"
            />
          </label>
        }
      >
        <QueryState query={models}>
          {(d) => (
            <DataTable
              columns={cols}
              rows={d.data.filter((m) =>
                [
                  m.id,
                  m.owned_by,
                  ...m.providers,
                  ...Object.entries(m.capabilities)
                    .filter(([, v]) => v === true)
                    .map(([k]) => k),
                ]
                  .join(" ")
                  .toLowerCase()
                  .includes(search.toLowerCase()),
              )}
              visibility
              label="Model capability catalog"
            />
          )}
        </QueryState>
        <Notice>
          Capability declarations are not per-model certification. Discovery
          failures can omit models. Ordered candidates, active routing policy,
          provider URLs and concurrency ceilings are not exposed. An aggregator
          instance does not identify its upstream host.
        </Notice>
      </Panel>
    </>
  );
}
