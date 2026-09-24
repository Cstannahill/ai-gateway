import type {
  Attempt,
  Comparison,
  Detail,
  Health,
  Metrics,
  Model,
  Provider,
  Recovery,
  RequestSummary,
  Timeline,
} from "./contracts";
const base = Date.parse("2026-09-23T08:40:00Z");
const names = ["llama-local", "openrouter", "ollama", "gemini"];
const models = [
  "qwen3.5-9b",
  "anthropic/claude-sonnet-4",
  "llama3.2:latest",
  "gemini-2.5-pro",
];
export const demoProviders: Provider[] = names.map((name, i) => ({
  name,
  provider_type: ["llama_cpp", "open_router", "ollama", "gemini"][i],
  health: i === 2 ? "degraded" : "healthy",
  enabled: true,
  active_requests: i === 0 ? 2 : i === 1 ? 1 : 0,
  total_requests: [1820, 940, 360, 520][i],
  total_errors: [24, 9, 18, 3][i],
  avg_latency_ms: [1240, 2380, 1680, 3010][i],
}));
export const demoHealth: Health = {
  status: "degraded",
  gateway: "ai-gateway",
  version: "0.1.0",
  providers: 4,
};
export const demoMetrics: Metrics = {
  gateway: {
    version: "0.1.0",
    uptime_seconds: 31242,
    total_requests: 3482,
    total_provider_attempts: 3640,
    total_provider_errors: 54,
    active_requests: 3,
  },
  providers: demoProviders,
};
export const demoRequests: RequestSummary[] = Array.from(
  { length: 67 },
  (_, i) => ({
    request_id: `0199768a-6e00-7000-8000-${(i + 1).toString(16).padStart(12, "0")}`,
    client_correlation_id: i === 0 ? "editor-session-42" : null,
    requested_model: ["coding-fast", "coding-strong", "general", "reasoning"][
      i % 4
    ],
    resolved_model: models[i % 4],
    serving_provider: names[i % 4],
    success: i === 6 ? null : i % 11 !== 3,
    streaming: i % 7 !== 2,
    total_latency_us:
      i === 6 ? null : [1482000, 2875000, 912000, 4218000][i % 4] + i * 14000,
    received_at: new Date(base - i * 72000).toISOString(),
  }),
);
export const demoComparison: Comparison[] = names.map((provider, i) => ({
  provider,
  model: models[i],
  total_attempts: demoProviders[i].total_requests,
  successful_attempts:
    demoProviders[i].total_requests - demoProviders[i].total_errors,
  avg_duration_ms: [1210, 2380, 1680, 3010][i],
  avg_tokens_per_sec: [62.4, 48.1, 39.5, 81.3][i],
  avg_ttft_ms: [182, 430, 261, 690][i],
}));
export const demoModels: Model[] = models.map((id, i) => ({
  id: names[i] + "/" + id,
  owned_by: names[i],
  providers: [names[i]],
  capabilities: {
    tools: true,
    vision: i % 2 === 1,
    reasoning: i !== 2,
    structured_output: true,
    streaming: true,
    embeddings: false,
    context_window: [32768, 200000, 8192, 1000000][i],
    extra: [],
  },
}));
demoModels.unshift({
  id: "coding-fast",
  owned_by: "ai-gateway",
  providers: ["llama-local", "openrouter"],
  capabilities: { ...demoModels[0].capabilities },
});
export const demoRecoveries: Recovery[] = [0, 1, 4, 8, 12, 16].flatMap(
  (index) => [
    {
      request_id: demoRequests[index].request_id,
      succeeded_provider: demoRequests[index].serving_provider,
      failed_provider:
        index === 1 ? "llama-local" : demoRequests[index].serving_provider!,
      failed_model: "qwen3.5-9b",
      error_code: index % 2 ? "connection_refused" : "rate_limit",
      error_message:
        index % 2
          ? "Connection refused by local inference server."
          : "Provider returned HTTP 429; retry succeeded.",
    },
  ],
);
export function demoDetail(id: string): Detail | null {
  const index = demoRequests.findIndex((r) => r.request_id === id);
  if (index < 0) return null;
  const request = demoRequests[index];
  const recovery = demoRecoveries.find((r) => r.request_id === id);
  const time = (ms: number) =>
    new Date(Date.parse(request.received_at) + ms).toISOString();
  const attempt: Attempt = {
    attempt_number: recovery ? 2 : 1,
    provider: request.serving_provider!,
    model: request.resolved_model!,
    status:
      request.success === null
        ? "started"
        : request.success
          ? "success"
          : "failed",
    finish_reason: request.success ? "stop" : null,
    duration_us: request.total_latency_us,
    upstream_provider: null,
    provider_response_id: "fixture-response-" + index,
    error_code: request.success === false ? "stream_error" : null,
    error_message:
      request.success === false
        ? "Upstream closed the stream after partial output."
        : null,
    first_byte_at: time(120),
    first_token_at: time(182),
    synthetic_timing: !request.streaming,
  };
  const attempts: Attempt[] = recovery
    ? [
        {
          ...attempt,
          attempt_number: 1,
          provider: recovery.failed_provider,
          model: recovery.failed_model,
          status: "failed",
          duration_us: 42000,
          error_code: recovery.error_code,
          error_message: recovery.error_message,
          first_byte_at: null,
          first_token_at: null,
          finish_reason: null,
        },
        attempt,
      ]
    : [attempt];
  return {
    request: {
      ...request,
      canonical_messages: [
        { role: "system", content: "You are a careful Rust engineer." },
        {
          role: "user",
          content:
            'Explain how bounded channels provide backpressure.\n<script>alert("untrusted content")</script>\n![remote image](https://example.com/tracker.png)',
        },
      ],
      raw_request: {
        model: request.requested_model,
        stream: request.streaming,
        messages: [{ role: "user", content: "Explain bounded channels." }],
      },
    },
    attempts,
    generations:
      request.success === null
        ? []
        : [
            {
              attempt_number: attempt.attempt_number,
              content:
                request.success === false
                  ? "A bounded channel holds a fixed number of messages. When the buffer"
                  : "A bounded channel holds a fixed number of messages. When the buffer fills, an awaited send waits for capacity. This lets the consumer control the pace of the producer.\n\nWith try_send, a full buffer returns an error immediately. The caller decides whether to retry, persist, or drop the message.",
              reasoning_content:
                index % 3 === 0
                  ? "The distinction is between awaited sends and nonblocking sends. The queue policy determines whether backpressure is applied."
                  : null,
              reasoning_visibility:
                index % 3 === 0
                  ? "exposed"
                  : index % 3 === 1
                    ? "count_only"
                    : "none",
              tool_calls:
                index === 4
                  ? [
                      {
                        id: "call-1",
                        function: {
                          name: "read_docs",
                          arguments: '{"topic":"channels"}',
                        },
                      },
                    ]
                  : null,
            },
          ],
    usage:
      index === 3 || request.success === null
        ? []
        : [
            {
              attempt_number: attempt.attempt_number,
              prompt_tokens: 420 + index,
              completion_tokens: 186,
              total_tokens: 606 + index,
              cached_tokens: index === 2 ? null : 128,
              reasoning_tokens: index % 3 === 1 ? 80 : 0,
            },
          ],
  };
}
export function demoTimeline(id: string): Timeline {
  const d = demoDetail(id);
  if (!d) return { request_id: id, events: [] };
  const a = d.attempts.at(-1)!;
  return {
    request_id: id,
    events: d.generations.length
      ? Array.from({ length: 38 }, (_, i) => ({
          attempt_number: a.attempt_number,
          sequence: i + 1,
          timestamp: new Date(
            Date.parse(d.request.received_at) +
              182 +
              i * 26 +
              (i > 18 ? 190 : 0),
          ).toISOString(),
          chunk_type: i < 5 ? "reasoning" : i === 22 ? "tool_call" : "content",
          byte_count: [5, 18, 9, 24][i % 4],
          synthetic: a.synthetic_timing,
        }))
      : [],
  };
}
export function demoResponse(path: string): unknown {
  const u = new URL(path, "http://fixture");
  if (u.pathname === "/health") return demoHealth;
  if (u.pathname === "/metrics") return demoMetrics;
  if (u.pathname === "/providers") return { providers: demoProviders };
  if (u.pathname === "/v1/models") return { object: "list", data: demoModels };
  if (u.pathname === "/v1/analytics/providers/comparison")
    return { providers: demoComparison };
  if (u.pathname === "/v1/analytics/fallbacks")
    return { fallbacks: demoRecoveries };
  if (u.pathname === "/v1/analytics/requests") {
    const q = u.searchParams;
    const offset = +(q.get("offset") ?? 0),
      limit = +(q.get("limit") ?? 50);
    return {
      limit,
      offset,
      requests: demoRequests
        .filter(
          (r) =>
            (!q.get("provider") || r.serving_provider === q.get("provider")) &&
            (!q.get("model") ||
              r.requested_model === q.get("model") ||
              r.resolved_model === q.get("model")) &&
            (!q.get("success") || String(r.success) === q.get("success")),
        )
        .slice(offset, offset + limit),
    };
  }
  const id = u.pathname.split("/")[4];
  if (u.pathname.endsWith("/timeline")) return demoTimeline(id);
  return demoDetail(id);
}
// Future cost contracts only; never presented as live API fields.
export const pricingCases = [
  { configured: false, estimated_cost: null },
  { configured: true, estimated_cost: 0, currency: "USD" },
];
