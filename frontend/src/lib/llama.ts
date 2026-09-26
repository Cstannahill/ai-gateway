import { ApiError } from "./api";

export type LlamaState = "disabled" | "stopped" | "starting" | "loading" | "ready" | "exited" | "failed";
export interface SavedModel { id: string; alias: string; gguf_path: string; mmproj_path: string | null; args: string | null; pass_alias: boolean; notes: string | null; created_at: string; updated_at: string }
export interface ModelInput { alias: string; gguf_path: string; mmproj_path: string | null; args: string | null; pass_alias: boolean; notes: string | null }
export interface LogLine { seq: number; ts: string; stream: "stdout" | "stderr" | "gateway"; text: string }
export interface ServerInfo { health: "ok" | "loading" | "error" | null; model_path: string | null; model_id: string | null; n_ctx: number | null; n_ctx_train: number | null; n_params: number | null; model_size_bytes: number | null; vision: boolean | null; audio: boolean | null; build_info: string | null; total_slots: number | null }
export interface ExitInfo { code: number | null; at: string; unexpected: boolean; reason: string; log_tail: LogLine[] }
export interface Status { enabled: true; state: Exclude<LlamaState, "disabled">; managed: boolean; external: boolean; pid: number | null; started_at: string | null; ready_at: string | null; uptime_secs: number | null; model: { id: string; alias: string; gguf_path: string; mmproj_path: string | null } | null; args: { effective: string; argv: string[] } | null; server: ServerInfo | null; matched_model: { id: string; alias: string } | null; failure: string | null; last_exit: ExitInfo | null; endpoint: { host: string; port: number; base_url: string } }
export type StatusResponse = Status | { enabled: false; state: "disabled"; message: string };
export interface LogsPage { lines: LogLine[]; next_cursor: number; truncated: boolean; state: LlamaState }
export interface ScanFile { path: string; file_name: string; size_bytes: number; modified: string | null; already_saved: boolean }
export interface ScanResult { configured: boolean; dirs: { dir: string; exists: boolean; truncated: boolean; models: ScanFile[]; mmproj_candidates: ScanFile[] }[] }
export interface ParseResult { argv: string[]; issues: { code: string; message: string; token?: string }[] }
export interface Validation { valid: boolean; gguf_path?: string; mmproj_path?: string | null; alias_taken?: boolean; error?: { code: string; message: string } }
export interface ModelList { models: SavedModel[]; default_args: string; running_model_id: string | null }

export async function llamaRequest<T>(path: string, method = "GET", body?: object, signal?: AbortSignal): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`/api/llama-server/${path}`, {
      method,
      headers: body === undefined ? undefined : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      cache: "no-store",
      signal,
    });
  } catch (error) {
    if (signal?.aborted) throw error;
    throw new ApiError(0, "connection_failure", "The console cannot connect to its llama-server proxy.");
  }
  if (response.status === 204) return undefined as T;
  const raw = await response.text();
  let data: Record<string, unknown> | null = null;
  try { data = JSON.parse(raw) as Record<string, unknown>; } catch { /* Framework errors may be plain text. */ }
  if (!response.ok) {
    const envelope = data?.error as { code?: string; message?: string } | undefined;
    throw new ApiError(response.status, envelope?.code ?? "http_error", envelope?.message ?? (raw || `HTTP ${response.status}`));
  }
  if (!data) throw new ApiError(502, "invalid_gateway_response", "Gateway returned invalid JSON.");
  return data as T;
}

export const fileName = (path: string | null | undefined) => path?.split(/[\\/]/).pop() || "Unknown file";
export const formatBytes = (value: number | null | undefined) => value == null ? "" : value >= 1e9 ? `${(value / 1e9).toFixed(1)} GB` : value >= 1e6 ? `${(value / 1e6).toFixed(1)} MB` : `${value} B`;
export const formatParams = (value: number | null | undefined) => value == null ? "" : value >= 1e9 ? `${(value / 1e9).toFixed(1)} B` : value >= 1e6 ? `${(value / 1e6).toFixed(1)} M` : value.toLocaleString();
export const quoteArg = (value: string) => /\s/.test(value) ? JSON.stringify(value) : value;
