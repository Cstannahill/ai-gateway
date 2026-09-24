"use client";
import type { ReactNode } from "react";
import type { UseQueryResult } from "@tanstack/react-query";
import { AlertCircle, Inbox, RefreshCw } from "lucide-react";
import { ApiError } from "@/lib/api";
export function Panel({
  title,
  scope,
  actions,
  children,
  className = "",
}: {
  title: string;
  scope?: string;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={"panel " + className}>
      <div className="panel-head">
        <div>
          <h2>{title}</h2>
          {scope && <p>{scope}</p>}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}
export function Badge({
  children,
  tone = "neutral",
}: {
  children: ReactNode;
  tone?: string;
}) {
  return <span className={"badge " + tone}>{children}</span>;
}
export function Outcome({ value }: { value: boolean | null }) {
  return (
    <Badge tone={value === true ? "good" : value === false ? "bad" : "warn"}>
      {value === true
        ? "Succeeded"
        : value === false
          ? "Failed"
          : "Unfinished / unknown"}
    </Badge>
  );
}
export function Empty({
  title = "No observations yet",
  children,
}: {
  title?: string;
  children?: ReactNode;
}) {
  return (
    <div className="empty">
      <Inbox size={25} />
      <h3>{title}</h3>
      <p>
        {children ??
          "Records will appear here when the gateway has recorded matching traffic."}
      </p>
    </div>
  );
}
export function Notice({ children }: { children: ReactNode }) {
  return (
    <div className="notice">
      <AlertCircle size={15} />
      <div>{children}</div>
    </div>
  );
}
export function QueryState<T>({
  query,
  children,
}: {
  query: UseQueryResult<T, ApiError>;
  children: (data: T) => ReactNode;
}) {
  if (query.isPending)
    return (
      <div className="skeleton" role="status" aria-label="Loading observations">
        <span />
        <span />
        <span />
      </div>
    );
  if (query.error && !query.data) {
    const e = query.error;
    const title =
      e.status === 503
        ? "Analytics unavailable"
        : e.status === 404
          ? "Request not found"
          : e.code === "gateway_unreachable" || e.status === 0
            ? "Gateway connection unavailable"
            : "Unable to load this panel";
    return (
      <div className="error-state" role="alert">
        <AlertCircle size={23} />
        <h3>{title}</h3>
        <p>
          {e.status === 404
            ? "This ID may not exist or may not have been persisted yet."
            : e.message}
        </p>
        <button onClick={() => query.refetch()}>
          <RefreshCw size={14} />
          Try again
        </button>
      </div>
    );
  }
  return (
    <>
      {query.error && (
        <Notice>
          Stale data — refresh failed. Last success:{" "}
          {new Date(query.dataUpdatedAt).toLocaleTimeString()}.{" "}
          <button onClick={() => query.refetch()}>Retry</button>
        </Notice>
      )}
      {query.data !== undefined && children(query.data)}
    </>
  );
}
export function Stat({
  label,
  value,
  detail,
  tone = "",
}: {
  label: string;
  value: ReactNode;
  detail: string;
  tone?: string;
}) {
  return (
    <div className={"stat " + tone}>
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{detail}</small>
    </div>
  );
}
