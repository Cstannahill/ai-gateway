"use client";
import { useState } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import * as Dialog from "@radix-ui/react-dialog";
import {
  ArrowDown,
  Copy,
  Download,
  ExternalLink,
  RefreshCw,
  X,
} from "lucide-react";
import type { Detail } from "@/lib/contracts";
import {
  download,
  duration,
  milliseconds,
  number,
  relativeMs,
  timestamp,
  transition,
  usageWarnings,
} from "@/lib/metrics";
import { useRead, useRuntime } from "@/components/runtime";
import { Badge, Empty, Notice, Outcome, QueryState } from "@/components/ui";
import { Timeline } from "./timeline";
import { isRequestId } from "@/lib/proxy-policy";

function TextBlock({ value }: { value: unknown }) {
  const [expanded, setExpanded] = useState(false);
  const text =
    typeof value === "string"
      ? value
      : (JSON.stringify(value, null, 2) ?? "Not captured");
  return (
    <>
      <pre className="captured">{text.slice(0, expanded ? 100000 : 6000)}</pre>
      {text.length > 6000 && (
        <button onClick={() => setExpanded(!expanded)}>
          {expanded ? "Collapse" : "Expand text (up to 100,000 characters)"}
        </button>
      )}
      {text.length > 100000 && expanded && (
        <small>
          Preview bounded to 100,000 characters. Use the explicit JSON export
          for the full returned record.
        </small>
      )}
    </>
  );
}
function Content({ detail }: { detail: Detail }) {
  const messages = Array.isArray(detail.request.canonical_messages)
    ? detail.request.canonical_messages
    : [];
  return (
    <>
      <Notice>
        Captured text is displayed literally. HTML, remote images and tool calls
        are never executed. The backend does not expose choice identity and may
        have combined choices; these are attempt-level generation records.
      </Notice>
      <h3 className="section-label">Input messages</h3>
      {messages.length ? (
        messages.slice(0, 100).map((m: unknown, i) => {
          const message =
            m && typeof m === "object" ? (m as Record<string, unknown>) : {};
          return (
            <div className="content-block" key={i}>
              <Badge>
                {typeof message.role === "string"
                  ? message.role
                  : "Unknown role"}
              </Badge>
              <TextBlock value={message.content ?? m} />
            </div>
          );
        })
      ) : (
        <Empty title="Input messages not captured" />
      )}
      {messages.length > 100 && (
        <Notice>
          First 100 messages shown. The explicit request export includes all
          returned messages.
        </Notice>
      )}
      <h3 className="section-label">Generated output</h3>
      {detail.generations.length ? (
        detail.generations.slice(0, 100).map((g, i) => (
          <div className="content-block" key={i}>
            <div className="section-intro">
              <h3>
                Attempt {g.attempt_number} · record {i + 1}
              </h3>
              <Badge>
                {detail.attempts.find(
                  (a) => a.attempt_number === g.attempt_number,
                )?.status === "success"
                  ? "Recorded output"
                  : "Partial / terminal state uncertain"}
              </Badge>
            </div>
            <TextBlock value={g.content ?? "No answer text returned"} />
            <details>
              <summary>
                Returned reasoning ·{" "}
                {g.reasoning_visibility ?? "visibility unknown"}
              </summary>
              <p className="muted">
                Only reasoning returned by the provider is shown.
              </p>
              <TextBlock
                value={
                  g.reasoning_content ??
                  (g.reasoning_visibility === "count_only"
                    ? "Token count only; no reasoning text exposed."
                    : "No reasoning text captured. This does not establish absence of reasoning.")
                }
              />
            </details>
            {g.tool_calls != null && (
              <details>
                <summary>Captured tool calls</summary>
                <TextBlock value={g.tool_calls} />
              </details>
            )}
          </div>
        ))
      ) : (
        <Empty title="No generation recorded yet">
          The request may have failed before producing output, or telemetry may
          still be arriving.
        </Empty>
      )}
    </>
  );
}
function UsageView({ detail }: { detail: Detail }) {
  const attemptsWithUsage = new Set(detail.usage.map((u) => u.attempt_number))
    .size;
  return (
    <>
      <div className="section-intro">
        <h3>Usage observations</h3>
        <Badge>
          {attemptsWithUsage} / {detail.attempts.length} attempts have usage
          rows
        </Badge>
      </div>
      <Notice>
        Rows are observations, not certified final totals. The API omits update
        timestamps and final/delta identity. Zero totals may encode missing
        usage even in current records. Cached tokens belong to prompt tokens;
        reasoning tokens belong to completion tokens. Neither is added again.
      </Notice>
      {detail.usage.length ? (
        detail.usage.slice(0, 100).map((u, i) => (
          <div className="usage-record" key={i}>
            <h3>
              Attempt {u.attempt_number} · observation {i + 1}
            </h3>
            <div className="usage-grid">
              {[
                ["Prompt", u.prompt_tokens],
                ["Completion", u.completion_tokens],
                ["Reported total", u.total_tokens],
                ["Cached · prompt subset", u.cached_tokens],
                ["Reasoning · output subset", u.reasoning_tokens],
              ].map(([label, value]) => (
                <div key={String(label)}>
                  <small>{label}</small>
                  <strong>{number(value as number | null)}</strong>
                </div>
              ))}
            </div>
            {usageWarnings(
              u,
              detail.usage.filter((x) => x.attempt_number === u.attempt_number)
                .length,
            ).map((w) => (
              <p className="warning-text" key={w}>
                {w}
              </p>
            ))}
          </div>
        ))
      ) : (
        <Empty title="Usage not available">
          No usage rows have been captured for this request. This does not mean
          zero tokens.
        </Empty>
      )}
      <Notice>
        Costs and pricing coverage are not exposed by this read API. No price
        means unknown; configured zero would mean zero estimated token charge,
        not zero operating cost.
      </Notice>
    </>
  );
}
function Attempts({ detail }: { detail: Detail }) {
  const attempts = [...detail.attempts].sort(
    (a, b) => a.attempt_number - b.attempt_number,
  );
  const genuine = attempts
    .filter((a) => !a.synthetic_timing && a.first_token_at)
    .map((a) => relativeMs(a.first_token_at, detail.request.received_at))
    .filter((n): n is number => n !== null && n >= 0);
  return (
    <>
      <div className="section-intro">
        <div>
          <h3>Attempt chain</h3>
          <p>
            {attempts.length} recorded attempts · request TTFT{" "}
            {genuine.length ? duration(Math.min(...genuine)) : "unavailable"}
          </p>
        </div>
        <Badge>Observed order</Badge>
      </div>
      {attempts.length ? (
        attempts.map((a, i) => (
          <div key={i}>
            {i > 0 && (
              <div className="attempt-transition">
                <ArrowDown size={14} />
                {transition(attempts[i - 1], a)}
              </div>
            )}
            <article className="attempt-card">
              <div className="attempt-index">
                {String(a.attempt_number).padStart(2, "0")}
              </div>
              <div className="attempt-body">
                <div className="section-intro">
                  <div>
                    <h3>{a.provider}</h3>
                    <p className="mono">{a.model}</p>
                  </div>
                  <Badge
                    tone={
                      a.status === "success"
                        ? "good"
                        : a.status === "started"
                          ? "warn"
                          : "bad"
                    }
                  >
                    {a.status}
                  </Badge>
                </div>
                <div className="attempt-metrics">
                  <span>
                    Recorded duration{" "}
                    <b>{duration(milliseconds(a.duration_us))}</b>
                  </span>
                  <span>
                    Finish reason <b>{a.finish_reason ?? "—"}</b>
                  </span>
                  <span>
                    Timing{" "}
                    <b>
                      {a.synthetic_timing
                        ? "Synthetic reconstruction"
                        : "Observed"}
                    </b>
                  </span>
                </div>
                {a.error_code && (
                  <div className="attempt-error">
                    <strong>{a.error_code}</strong>
                    <TextBlock
                      value={a.error_message ?? "No error detail returned"}
                    />
                  </div>
                )}
                {a.upstream_provider && (
                  <p>Upstream host: {a.upstream_provider}</p>
                )}
                {a.provider_response_id && (
                  <small className="mono">
                    Provider response: {a.provider_response_id}
                  </small>
                )}
              </div>
            </article>
          </div>
        ))
      ) : (
        <Empty title="No attempt rows yet" />
      )}
      <Notice>
        Request TTFT uses the earliest genuine generation marker minus gateway
        receipt. Timing markers do not establish complete capture. Streaming
        recorded duration starts after dispatch returns; it cannot safely be
        subtracted from total request latency to infer overhead.
      </Notice>
    </>
  );
}
export function DetailBody({ id }: { id: string }) {
  const valid = isRequestId(id),
    q = useRead<Detail>("/v1/analytics/requests/" + id, 0, valid),
    { zone, mode } = useRuntime();
  const client = useQueryClient();
  const [tab, setTab] = useState("Attempts"),
    [copyStatus, setCopyStatus] = useState("");
  if (!valid)
    return (
      <Empty title="Invalid request ID">
        Use the complete gateway UUID, not a client correlation ID.
      </Empty>
    );
  return (
    <QueryState query={q}>
      {(d) => (
        <>
          <div className="detail-toolbar">
            <Outcome value={d.request.success} />
            <span className="muted">
              {timestamp(d.request.received_at, zone)} · {zone}
            </span>
            <button
              onClick={() =>
                client.invalidateQueries({
                  predicate: (query) =>
                    query.queryKey[0] === mode &&
                    typeof query.queryKey[2] === "string" &&
                    query.queryKey[2].startsWith(
                      "/v1/analytics/requests/" + id,
                    ),
                })
              }
            >
              <RefreshCw size={14} />
              Refresh
            </button>
          </div>
          <h2 className="detail-model">{d.request.requested_model}</h2>
          <p className="detail-resolution">
            {d.request.serving_provider ?? "Provider not recorded"}{" "}
            <span> / </span>
            {d.request.resolved_model ?? "Resolution not recorded"}
          </p>
          <dl className="identity-grid">
            <div>
              <dt>Gateway request ID</dt>
              <dd className="mono">{id}</dd>
            </div>
            <div>
              <dt>Client correlation ID</dt>
              <dd className="mono">
                {d.request.client_correlation_id ?? "Not supplied"}
              </dd>
            </div>
            <div>
              <dt>Request latency</dt>
              <dd>{duration(milliseconds(d.request.total_latency_us))}</dd>
            </div>
            <div>
              <dt>Response mode</dt>
              <dd>
                {d.request.streaming === null
                  ? "Unknown"
                  : d.request.streaming
                    ? "Streaming"
                    : "Non-streaming"}
              </dd>
            </div>
          </dl>
          <p className="muted detail-capture">
            Capture is asynchronous. Refresh to check for later observations;
            this view does not certify completeness.
          </p>
          <div
            className="detail-tabs"
            role="tablist"
            aria-label="Request investigation"
          >
            {["Attempts", "Timeline", "Content", "Usage", "Raw data"].map(
              (t) => (
                <button
                  key={t}
                  role="tab"
                  aria-selected={tab === t}
                  id={"tab-" + t.replaceAll(" ", "-")}
                  aria-controls="investigation-panel"
                  onClick={() => setTab(t)}
                  onKeyDown={(e) => {
                    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
                      e.preventDefault();
                      const tabs = [
                        "Attempts",
                        "Timeline",
                        "Content",
                        "Usage",
                        "Raw data",
                      ];
                      const next =
                        tabs[
                          (tabs.indexOf(t) + (e.key === "ArrowRight" ? 1 : 4)) %
                            5
                        ];
                      setTab(next);
                      document
                        .getElementById("tab-" + next.replaceAll(" ", "-"))
                        ?.focus();
                    }
                  }}
                >
                  {t}
                </button>
              ),
            )}
          </div>
          <div
            id="investigation-panel"
            role="tabpanel"
            aria-labelledby={"tab-" + tab.replaceAll(" ", "-")}
            className="detail-tab-body"
          >
            {tab === "Attempts" ? (
              <Attempts detail={d} />
            ) : tab === "Timeline" ? (
              <Timeline detail={d} />
            ) : tab === "Content" ? (
              <Content detail={d} />
            ) : tab === "Usage" ? (
              <UsageView detail={d} />
            ) : (
              <>
                <Notice>
                  Contains captured inference content. Export and copy are
                  explicit. This is the returned API record, not all database
                  fields.
                </Notice>
                <div className="actions raw-actions">
                  <button
                    onClick={async () => {
                      try {
                        await navigator.clipboard.writeText(
                          JSON.stringify(d, null, 2),
                        );
                        setCopyStatus("Copied returned record");
                      } catch {
                        setCopyStatus(
                          "Clipboard unavailable; use JSON export.",
                        );
                      }
                    }}
                  >
                    <Copy size={14} />
                    Copy JSON
                  </button>
                  <button
                    onClick={() =>
                      download(
                        "request-" + id,
                        d,
                        `${mode}; selected request; includes captured input/output; does not include separately loaded timeline`,
                      )
                    }
                  >
                    <Download size={14} />
                    Export request JSON
                  </button>
                  <span role="status">{copyStatus}</span>
                </div>
                <TextBlock value={d} />
              </>
            )}
          </div>
        </>
      )}
    </QueryState>
  );
}
export function RequestDrawer() {
  const params = useSearchParams(),
    router = useRouter(),
    { mode } = useRuntime();
  const id = params.get("request");
  const returnParams = new URLSearchParams(params);
  returnParams.delete("request");
  if (!returnParams.has("view")) returnParams.set("view", "requests");
  const permalinkParams = new URLSearchParams({
    back: returnParams.toString(),
    ...(mode === "demo" ? { demo: "1" } : {}),
  });
  return (
    <Dialog.Root
      open={!!id}
      onOpenChange={(open) => {
        if (!open) {
          const p = new URLSearchParams(params);
          p.delete("request");
          router.replace("/?" + p, { scroll: false });
        }
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="drawer-overlay" />
        <Dialog.Content
          className="drawer"
          onCloseAutoFocus={(e) => {
            e.preventDefault();
            if (id && isRequestId(id))
              document
                .querySelector<HTMLAnchorElement>(`a[title="${id}"]`)
                ?.focus({ preventScroll: true });
          }}
        >
          <div className="drawer-heading">
            <div>
              <Dialog.Title>Request investigation</Dialog.Title>
              <Dialog.Description>
                Follow each attempt, inspect stream observations and captured
                content.
              </Dialog.Description>
            </div>
            <div className="actions">
              <Link
                className="icon-button"
                aria-label="Open full request page"
                href={`/requests/${encodeURIComponent(id ?? "")}?${permalinkParams}`}
              >
                <ExternalLink size={17} />
              </Link>
              <Dialog.Close className="icon-button" aria-label="Close request">
                <X size={19} />
              </Dialog.Close>
            </div>
          </div>
          {id && <DetailBody key={id} id={id} />}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
