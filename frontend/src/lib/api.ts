export class ApiError extends Error {
  status: number;
  code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}
type Shape = Record<string, unknown>;
const object = (value: unknown): value is Shape =>
  !!value && typeof value === "object" && !Array.isArray(value);
const string = (value: unknown) => typeof value === "string";
const numeric = (value: unknown) =>
  typeof value === "number" && Number.isFinite(value);
const count = (value: unknown) =>
  numeric(value) && Number.isSafeInteger(value) && (value as number) >= 0;
const nullable = (test: (value: unknown) => boolean) => (value: unknown) =>
  value === null || test(value);
const optionalNumber = (value: unknown) => value == null || numeric(value);
const date = (value: unknown) =>
  string(value) && Number.isFinite(Date.parse(value as string));
const boolean = (value: unknown) => typeof value === "boolean";
function fields(
  value: unknown,
  schema: Record<string, (v: unknown) => boolean>,
) {
  return (
    object(value) &&
    Object.entries(schema).every(([key, test]) => test(value[key]))
  );
}
const summary = (v: unknown) =>
  fields(v, {
    request_id: string,
    received_at: date,
    requested_model: string,
    resolved_model: nullable(string),
    serving_provider: nullable(string),
    success: nullable(boolean),
    streaming: nullable(boolean),
    total_latency_us: nullable(numeric),
  });
const provider = (v: unknown) =>
  fields(v, {
    name: string,
    provider_type: string,
    health: string,
    enabled: boolean,
    total_requests: count,
    total_errors: count,
    active_requests: count,
    avg_latency_ms: optionalNumber,
  });
const array = (v: unknown, test: (v: unknown) => boolean) =>
  Array.isArray(v) && v.every(test);
export function validate(value: unknown, path: string) {
  if (!value || typeof value !== "object")
    throw new ApiError(
      502,
      "contract_mismatch",
      "The gateway returned an unexpected response shape.",
    );
  const v = value as Record<string, unknown>;
  const route = path.split("?")[0];
  const arrayKey = route.endsWith("/timeline")
    ? "events"
    : route.endsWith("/comparison") || route === "/providers"
      ? "providers"
      : route.endsWith("/fallbacks")
        ? "fallbacks"
        : route === "/v1/models"
          ? "data"
          : route === "/v1/analytics/requests"
            ? "requests"
            : null;
  if (arrayKey && !Array.isArray(v[arrayKey]))
    throw new ApiError(
      502,
      "contract_mismatch",
      `Missing ${arrayKey} collection.`,
    );
  if (route === "/metrics" && (!v.gateway || typeof v.gateway !== "object"))
    throw new ApiError(502, "contract_mismatch", "Missing gateway metrics.");
  if (route === "/health" && typeof v.status !== "string")
    throw new ApiError(502, "contract_mismatch", "Missing gateway status.");
  if (
    !arrayKey &&
    route.startsWith("/v1/analytics/requests/") &&
    (!v.request ||
      !Array.isArray(v.attempts) ||
      !Array.isArray(v.generations) ||
      !Array.isArray(v.usage))
  )
    throw new ApiError(
      502,
      "contract_mismatch",
      "Incomplete request detail contract.",
    );
  let valid = true;
  if (route === "/health")
    valid = fields(v, {
      status: string,
      gateway: string,
      version: string,
      providers: count,
    });
  else if (route === "/metrics")
    valid =
      fields(v.gateway, {
        version: string,
        uptime_seconds: count,
        total_requests: count,
        total_provider_attempts: count,
        total_provider_errors: count,
        active_requests: count,
      }) && array(v.providers, provider);
  else if (route === "/providers") valid = array(v.providers, provider);
  else if (route === "/v1/models")
    valid = array(
      v.data,
      (m) =>
        object(m) &&
        fields(m, { id: string, owned_by: string }) &&
        array(m.providers, string) &&
        fields(m.capabilities, {
          tools: boolean,
          vision: boolean,
          reasoning: boolean,
          streaming: boolean,
          structured_output: boolean,
          embeddings: boolean,
          context_window: optionalNumber,
        }),
    );
  else if (route === "/v1/analytics/requests")
    valid =
      array(v.requests, summary) && fields(v, { limit: count, offset: count });
  else if (route.endsWith("/comparison"))
    valid = array(v.providers, (p) =>
      fields(p, {
        provider: string,
        model: string,
        total_attempts: count,
        successful_attempts: count,
        avg_duration_ms: nullable(numeric),
        avg_tokens_per_sec: nullable(numeric),
        avg_ttft_ms: nullable(numeric),
      }),
    );
  else if (route.endsWith("/fallbacks"))
    valid = array(v.fallbacks, (f) =>
      fields(f, {
        request_id: string,
        failed_provider: string,
        failed_model: string,
        succeeded_provider: nullable(string),
        error_code: nullable(string),
        error_message: nullable(string),
      }),
    );
  else if (route.endsWith("/timeline"))
    valid = array(v.events, (e) =>
      fields(e, {
        attempt_number: count,
        sequence: count,
        timestamp: date,
        chunk_type: string,
        byte_count: nullable(count),
        synthetic: boolean,
      }),
    );
  else if (route.startsWith("/v1/analytics/requests/"))
    valid =
      summary(v.request) &&
      array(v.attempts, (a) =>
        fields(a, {
          attempt_number: count,
          provider: string,
          model: string,
          status: string,
          duration_us: nullable(numeric),
          first_byte_at: nullable(date),
          first_token_at: nullable(date),
          synthetic_timing: boolean,
        }),
      ) &&
      array(v.generations, (g) =>
        fields(g, {
          attempt_number: count,
          content: nullable(string),
          reasoning_content: nullable(string),
          reasoning_visibility: nullable(string),
        }),
      ) &&
      array(v.usage, (u) =>
        fields(u, {
          attempt_number: count,
          prompt_tokens: count,
          completion_tokens: count,
          total_tokens: count,
          cached_tokens: nullable(count),
          reasoning_tokens: nullable(count),
        }),
      );
  if (!valid)
    throw new ApiError(
      502,
      "contract_mismatch",
      "Gateway response fields do not match the verified read contract. Refresh or check the deployed gateway version.",
    );
}
export async function readApi<T>(
  path: string,
  mode: "live" | "demo",
  signal?: AbortSignal,
): Promise<T> {
  if (mode === "demo") {
    const { demoResponse } = await import("./demo");
    const value = demoResponse(path);
    if (value == null)
      throw new ApiError(
        404,
        "not_found",
        "No fixture matches this request ID.",
      );
    validate(value, path);
    return value as T;
  }
  let response: Response;
  try {
    response = await fetch("/api/gateway" + path, {
      signal,
      cache: "no-store",
    });
  } catch (error) {
    if (signal?.aborted) throw error;
    throw new ApiError(
      0,
      "connection_failure",
      "The dashboard cannot connect to its read proxy.",
    );
  }
  const body = await response.json().catch(() => null);
  if (!response.ok)
    throw new ApiError(
      response.status,
      body?.error?.code ?? "http_error",
      body?.error?.message ?? `Read failed (HTTP ${response.status}).`,
    );
  validate(body, path);
  return body as T;
}
