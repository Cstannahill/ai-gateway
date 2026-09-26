import type { LogLine, ModelInput, ModelList, ParseResult, SavedModel, ScanResult, Status, StatusResponse } from "./llama";

const at = "2026-09-23T08:40:00Z";
const paths = ["S:\\Code\\Models\\qwen-vl.gguf", "S:\\Code\\Models\\llama-8b.gguf", "S:\\Code\\Models\\magicprompt.gguf"];
const initial: SavedModel[] = ["qwen-vl", "llama-8b", "magicprompt"].map((alias, i) => ({
  id: `00000000-0000-4000-8000-00000000000${i + 1}`,
  alias, gguf_path: paths[i], mmproj_path: i === 0 ? "S:\\Code\\Models\\mmproj-qwen-vl.gguf" : null,
  args: i === 2 ? "-ngl 0 -c 512" : null, pass_alias: true, notes: i === 0 ? "Vision model" : null,
  created_at: at, updated_at: at,
}));
const server = { health: "ok" as const, model_path: paths[0], model_id: "qwen-vl", n_ctx: 32768,
  n_ctx_train: 262144, n_params: 9000000000, model_size_bytes: 5400000000, vision: true, audio: false,
  build_info: "b10434-7e4c0a968", total_slots: 1 };
const endpoint = { host: "127.0.0.1", port: 8080, base_url: "http://127.0.0.1:8080" };
const sampleLogs: LogLine[] = [
  { seq: 40, ts: at, stream: "gateway", text: "--- starting 'qwen-vl' ---" },
  { seq: 41, ts: at, stream: "stdout", text: "loading model metadata" },
  { seq: 42, ts: at, stream: "stderr", text: "llama_context: n_ctx = 32768" },
];
type Scenario = "disabled" | "stopped" | "starting" | "loading" | "ready" | "exited" | "failed" | "external";
let models = [...initial], defaults = "-ngl all -fa on", scenario: Scenario = "stopped";
let selected = initial[0], effective = defaults, startedAt: string | null = null, loadingAt = 0;
let logs = [...sampleLogs];
export const scenarios: Scenario[] = ["disabled", "stopped", "starting", "loading", "ready", "exited", "failed", "external"];
export function setDemoScenario(next: Scenario) {
  scenario = next;
  selected = models[0] ?? initial[0];
  effective = selected.args ?? defaults;
  startedAt = ["starting", "loading", "ready", "exited", "failed"].includes(next) ? new Date().toISOString() : null;
  loadingAt = Date.now();
}
export function demoStatus(): StatusResponse {
  if (scenario === "disabled") return { enabled: false, state: "disabled", message: "llama-server management is not enabled" };
  if (scenario === "loading" && Date.now() - loadingAt > 6000) scenario = "ready";
  const managed = ["starting", "loading", "ready"].includes(scenario);
  return {
    enabled: true, state: scenario === "external" ? "stopped" : scenario,
    managed, external: scenario === "external", pid: managed ? 16244 : null,
    started_at: startedAt, ready_at: scenario === "ready" ? at : null,
    uptime_secs: managed && startedAt ? Math.floor((Date.now() - Date.parse(startedAt)) / 1000) : null,
    model: startedAt && scenario !== "external" ? { id: selected.id, alias: selected.alias, gguf_path: selected.gguf_path, mmproj_path: selected.mmproj_path } : null,
    args: startedAt && scenario !== "external" ? { effective, argv: ["-m", selected.gguf_path, ...effective.split(/\s+/).filter(Boolean)] } : null,
    server: scenario === "ready" || scenario === "external" ? { ...server, model_path: selected.gguf_path, model_id: selected.alias, vision: !!selected.mmproj_path } : scenario === "loading" ? { ...server, health: "loading", model_path: selected.gguf_path } : null,
    matched_model: scenario === "ready" || scenario === "external" ? { id: selected.id, alias: selected.alias } : null,
    failure: scenario === "exited" ? "llama-server exited while loading" : scenario === "failed" ? "Could not spawn llama-server" : null,
    last_exit: scenario === "exited" ? { code: 1, at, unexpected: true, reason: "Model failed to load", log_tail: [...sampleLogs, { seq: 4, ts: at, stream: "stderr", text: "error: failed to load model" }] } : null,
    endpoint,
  } as Status;
}
export function demoModels(): ModelList { return { models: [...models], default_args: defaults, running_model_id: ["ready", "loading"].includes(scenario) ? selected.id : null }; }
export function demoScan(): ScanResult { return { configured: true, dirs: [{ dir: "S:\\Code\\Models", exists: true, truncated: false,
  models: paths.map((path, i) => ({ path, file_name: path.split("\\").pop()!, size_bytes: 5400000000 - i * 1000000000, modified: at, already_saved: i !== 2 })),
  mmproj_candidates: [{ path: initial[0].mmproj_path!, file_name: "mmproj-qwen-vl.gguf", size_bytes: 400000000, modified: at, already_saved: true }] }] }; }
export function demoLogs(after?: number) { return { lines: after == null ? logs : logs.filter((line) => line.seq > after), next_cursor: logs.at(-1)?.seq ?? 0, truncated: after == null || after === 0, state: demoStatus().state }; }
export function demoStart(id: string, args: string) {
  const model = models.find((m) => m.id === id);
  if (!model) throw new Error("No saved model with that ID.");
  selected = model; effective = args; startedAt = new Date().toISOString(); loadingAt = Date.now(); scenario = "loading";
  logs = [...logs, { seq: (logs.at(-1)?.seq ?? 0) + 1, ts: startedAt, stream: "gateway", text: `--- starting '${model.alias}' ---` }];
  return demoStatus();
}
export function demoStop() { scenario = "stopped"; return demoStatus(); }
export function demoRestart() { return demoStart(selected.id, effective); }
export function demoSave(input: ModelInput, id?: string) {
  const old = models.find((m) => m.id === id);
  const record: SavedModel = { ...input, id: old?.id ?? crypto.randomUUID(), created_at: old?.created_at ?? new Date().toISOString(), updated_at: new Date().toISOString() };
  models = [...models.filter((m) => m.id !== record.id), record];
  return record;
}
export function demoDelete(id: string) { models = models.filter((m) => m.id !== id); }
export function demoDefaults(args?: string) { if (args !== undefined) defaults = args; return { args: defaults }; }
export function demoParse(args: string): ParseResult {
  const argv: string[] = [];
  let token = "", quote: "'" | '"' | null = null, started = false;
  for (const char of args) {
    if (quote) {
      if (char === quote) quote = null;
      else token += char;
    } else if (char === "'" || char === '"') { quote = char; started = true; }
    else if (/\s/.test(char)) { if (started) { argv.push(token); token = ""; started = false; } }
    else { token += char; started = true; }
  }
  if (quote) return { argv: [], issues: [{ code: "unbalanced_quote", message: "Argument string has an unbalanced quote." }] };
  if (started) argv.push(token);
  const reserved = ["-m", "--model", "--mmproj", "-mm", "--mmproj-url", "--model-url", "--hf-repo", "-hf", "--hf-file", "--docker-repo", "--host", "--port", "--models-dir", "--models-preset", "-h", "--help", "--version", "--license", "--completion-bash", "--list-devices"];
  return { argv, issues: argv.flatMap((token) => reserved.some((flag) => token === flag || token.startsWith(`${flag}=`)) ? [{ code: "reserved_flag", message: `'${token}' is managed by the gateway.`, token }] : []) };
}
