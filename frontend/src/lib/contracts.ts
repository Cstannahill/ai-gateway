export type Nullable = number | null | undefined;
export interface Health {
  status: string;
  gateway: string;
  version: string;
  providers: number;
}
export interface Provider {
  name: string;
  provider_type: string;
  health: string;
  enabled: boolean;
  active_requests: number;
  total_requests: number;
  total_errors: number;
  avg_latency_ms?: number | null;
  avg_health_check_latency_ms?: number | null;
}
export interface Metrics {
  gateway: {
    version: string;
    uptime_seconds: number;
    total_requests: number;
    total_provider_attempts: number;
    total_provider_errors: number;
    active_requests: number;
  };
  providers: Provider[];
}
export interface Model {
  id: string;
  owned_by: string;
  providers: string[];
  capabilities: {
    tools: boolean;
    vision: boolean;
    reasoning: boolean;
    structured_output: boolean;
    streaming: boolean;
    embeddings: boolean;
    context_window: number | null;
    extra: string[];
  };
}
export interface RequestSummary {
  request_id: string;
  client_correlation_id: string | null;
  requested_model: string;
  resolved_model: string | null;
  serving_provider: string | null;
  success: boolean | null;
  streaming: boolean | null;
  total_latency_us: number | null;
  received_at: string;
}
export interface RequestList {
  requests: RequestSummary[];
  limit: number;
  offset: number;
}
export interface Attempt {
  attempt_number: number;
  provider: string;
  model: string;
  status: string;
  finish_reason: string | null;
  duration_us: number | null;
  upstream_provider: string | null;
  provider_response_id: string | null;
  error_code: string | null;
  error_message: string | null;
  first_byte_at: string | null;
  first_token_at: string | null;
  synthetic_timing: boolean;
}
export interface Generation {
  attempt_number: number;
  content: string | null;
  reasoning_content: string | null;
  reasoning_visibility: string | null;
  tool_calls: unknown;
}
export interface Usage {
  attempt_number: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cached_tokens: number | null;
  reasoning_tokens: number | null;
}
export interface Detail {
  request: RequestSummary & {
    canonical_messages: unknown;
    raw_request: unknown;
  };
  attempts: Attempt[];
  generations: Generation[];
  usage: Usage[];
}
export interface StreamEvent {
  attempt_number: number;
  timestamp: string;
  sequence: number;
  chunk_type: string;
  byte_count: number | null;
  synthetic: boolean;
}
export interface Timeline {
  request_id: string;
  events: StreamEvent[];
}
export interface Comparison {
  provider: string;
  model: string;
  total_attempts: number;
  successful_attempts: number;
  avg_duration_ms: number | null;
  avg_tokens_per_sec: number | null;
  avg_ttft_ms: number | null;
}
export interface Recovery {
  request_id: string;
  succeeded_provider: string | null;
  failed_provider: string;
  failed_model: string;
  error_code: string | null;
  error_message: string | null;
}
