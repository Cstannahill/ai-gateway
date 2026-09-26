"use client";
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Copy, RefreshCw } from "lucide-react";
import * as Dialog from "@radix-ui/react-dialog";
import { useRuntime } from "@/components/runtime";
import { Badge, Notice, Panel } from "@/components/ui";
import { ApiError } from "@/lib/api";
import { demoDefaults, demoModels, demoParse, demoRestart, demoSave, demoScan, demoStart, demoStatus, demoStop, setDemoScenario, scenarios } from "@/lib/llama-demo";
import { fileName, formatBytes, formatParams, llamaRequest, quoteArg, type ModelList, type ParseResult, type SavedModel, type ScanResult, type StatusResponse } from "@/lib/llama";
import { ModelsPanel } from "./models";
import { LogsPanel } from "./logs";
import "./llama.css";

export const errorText = (error: unknown) => error instanceof ApiError ? `${error.message} (${error.code})` : error instanceof Error ? error.message : String(error);
const copy = (text: string) => navigator.clipboard.writeText(text);
function Updated({ at }: { at: number }) { return <small className="llama-updated">{at ? `Updated ${new Date(at).toLocaleTimeString()}` : "Not updated"}</small>; }
function PanelError({ error, retry }: { error: unknown; retry: () => void }) { return <div className="llama-error" role="alert"><strong>Gateway unreachable or request failed</strong><p>{errorText(error)}</p><button onClick={retry}>Retry</button></div>; }

export function LlamaServer() {
  const { mode, paused, zone } = useRuntime();
  const client = useQueryClient();
  const [visible, setVisible] = useState(true);
  const [logInView, setLogInView] = useState(false);
  const [scenario, setScenario] = useState("stopped");
  const [busy, setBusy] = useState("");
  const [actionError, setActionError] = useState<unknown>(null);
  const [selectedOverride, setSelectedOverride] = useState("");
  const [argsEdit, setArgsEdit] = useState<{ id: string; value: string } | null>(null);
  const [saveArgs, setSaveArgs] = useState(false);
  const [confirm, setConfirm] = useState<{ title: string; message: string; action: () => Promise<void> } | null>(null);
  const [editModel, setEditModel] = useState<SavedModel | "new" | null>(null);
  const [parse, setParse] = useState<ParseResult | null>(null);
  const [parsedArgs, setParsedArgs] = useState<string | null>(null);
  const [parseError, setParseError] = useState<unknown>(null);
  const [now, setNow] = useState(0);
  const launcherRef = useRef<HTMLElement>(null);
  const logRef = useRef<HTMLElement>(null);
  useEffect(() => {
    const update = () => setVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", update);
    return () => document.removeEventListener("visibilitychange", update);
  }, []);
  useEffect(() => { if (mode === "demo") { setDemoScenario(scenario as typeof scenarios[number]); client.invalidateQueries({ queryKey: [mode, "llama"] }); } }, [mode, scenario, client]);
  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer); }, []);
  const status = useQuery<StatusResponse>({
    queryKey: [mode, "llama", "status"],
    queryFn: ({ signal }) => mode === "demo" ? demoStatus() : llamaRequest("status", "GET", undefined, signal),
    refetchInterval: (query) => paused || !visible ? false : query.state.data?.state === "starting" || query.state.data?.state === "loading" ? 1000 : mode === "demo" && scenario === "loading" ? 1000 : 5000,
    refetchIntervalInBackground: false,
    retry: false,
  });
  const enabled = status.data?.enabled === true;
  useEffect(() => {
    if (!logRef.current) return;
    const observer = new IntersectionObserver(([entry]) => setLogInView(entry.isIntersecting));
    observer.observe(logRef.current);
    return () => observer.disconnect();
  }, [enabled]);
  const models = useQuery<ModelList>({ queryKey: [mode, "llama", "models"], queryFn: ({ signal }) => mode === "demo" ? demoModels() : llamaRequest("models", "GET", undefined, signal), enabled, retry: false });
  const defaults = useQuery<{ args: string }>({ queryKey: [mode, "llama", "defaults"], queryFn: ({ signal }) => mode === "demo" ? demoDefaults() : llamaRequest("defaults", "GET", undefined, signal), enabled, retry: false });
  const scan = useQuery<ScanResult>({ queryKey: [mode, "llama", "scan"], queryFn: ({ signal }) => mode === "demo" ? demoScan() : llamaRequest("models/scan", "GET", undefined, signal), enabled, retry: false });
  const active = status.data?.enabled ? status.data : null;
  const loaded = !!active && (active.managed || !!active.server);
  const selectedId = selectedOverride || models.data?.models[0]?.id || "";
  const selected = models.data?.models.find((model) => model.id === selectedId);
  const args = argsEdit?.id === selectedId ? argsEdit.value : selected?.args ?? defaults.data?.args ?? "";
  const argsSource = argsEdit?.id === selectedId ? "Edited" : selected?.args === null ? "Global defaults" : "Model arguments";
  const currentParse = parsedArgs === args ? parse : null;
  useEffect(() => {
    if (!enabled) return;
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      try {
        if (mode === "demo") setParse(demoParse(args));
        else setParse(await llamaRequest<ParseResult>("args/parse", "POST", { args }, controller.signal));
        setParsedArgs(args);
        setParseError(null);
      } catch (error) { if (!controller.signal.aborted) setParseError(error); }
    }, 250);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [args, enabled, mode]);
  useEffect(() => {
    if (active?.model?.id || active?.matched_model?.id) client.invalidateQueries({ queryKey: [mode, "llama", "models"] });
  }, [active?.model?.id, active?.matched_model?.id, client, mode]);
  async function refresh() { await Promise.all([status.refetch(), models.refetch(), defaults.refetch(), scan.refetch()]); }
  async function perform(label: string, run: () => Promise<unknown>) {
    setBusy(label); setActionError(null);
    try { await run(); await client.invalidateQueries({ queryKey: [mode, "llama"] }); }
    catch (error) { setActionError(error); }
    finally { setBusy(""); }
  }
  const start = async (model: SavedModel, runArgs: string, replace: boolean, save: boolean) => {
    await perform("start", async () => {
      if (mode === "demo") { demoStart(model.id, runArgs); if (save) demoSave({ alias: model.alias, gguf_path: model.gguf_path, mmproj_path: model.mmproj_path, args: runArgs, pass_alias: model.pass_alias, notes: model.notes }, model.id); }
      else await llamaRequest("start", "POST", { model_id: model.id, args: runArgs, replace, save_args: save });
    });
  };
  const requestStart = (model: SavedModel, runArgs: string, save: boolean) => {
    if (active?.external) return;
    if (active?.managed) setConfirm({ title: `Switch to ${model.alias}?`, message: `Switching will interrupt requests using ${active.matched_model?.alias ?? active.model?.alias ?? "the current model"}.`, action: () => start(model, runArgs, true, save) });
    else void start(model, runArgs, false, save);
  };
  const doStop = () => perform("stop", async () => { if (mode === "demo") demoStop(); else await llamaRequest("stop", "POST"); });
  const doRestart = () => perform("restart", async () => { if (mode === "demo") demoRestart(); else await llamaRequest("restart", "POST"); });
  if (status.isPending) return <Panel title="llama-server status"><p className="llama-pad">Loading status…</p></Panel>;
  if (status.error && !status.data) return <Panel title="llama-server status"><PanelError error={status.error} retry={() => void status.refetch()} /></Panel>;
  if (status.data?.enabled === false) return <Panel title="llama-server management is not enabled" scope={status.data.message}><div className="llama-pad"><p>Enable the manager in <code>config/gateway.yaml</code> and restart the gateway.</p><pre className="llama-code">{`server:\n  host: 127.0.0.1\nllama_server:\n  enabled: true\n  binary: 'S:\\llama.cpp\\build\\bin\\Release\\llama-server.exe'\n  host: 127.0.0.1\n  port: 8080\n  state_file: config/llama_models.json\n  scan_dirs: ['S:\\Code\\Models']\n  start_timeout_secs: 300\n  log_lines: 2000`}</pre><p>Use the real <code>.exe</code>, not a <code>.cmd</code> shim. Keep the gateway bind address on loopback and match the <code>llama</code> provider port.</p></div>{mode === "demo" && <ScenarioPicker value={scenario} onChange={setScenario} />}</Panel>;
  return <>
    {mode === "demo" && <ScenarioPicker value={scenario} onChange={setScenario} />}
    <Panel title="llama-server status" scope="Current process and live server probe" actions={<><Updated at={status.dataUpdatedAt} /><button onClick={() => void refresh()}><RefreshCw size={14} /> Refresh</button></>}>
      {status.error && <Notice>Stale status: {errorText(status.error)}</Notice>}
      <div className="llama-pad">
        {active && <>
          <div className="llama-status-top"><Badge tone={active.state === "ready" ? "good" : ["failed", "exited"].includes(active.state) ? "bad" : ["loading", "starting"].includes(active.state) ? "warn" : "neutral"}>{active.state}</Badge><span aria-live="polite">{active.state === "ready" ? `Model ${active.matched_model?.alias ?? active.model?.alias ?? "unknown"} is ready` : `llama-server is ${active.state}`}</span>{["loading", "starting"].includes(active.state) && active.started_at && <span>{Math.max(0, Math.floor((now - Date.parse(active.started_at)) / 1000))} s elapsed</span>}</div>
          <h3>{loaded ? active.matched_model?.alias ?? active.model?.alias ?? active.server?.model_id ?? "Unknown model" : "No model loaded"}</h3>
          {!loaded && active.model && <p className="llama-small">Last run: {active.model.alias} · {fileName(active.model.gguf_path)}</p>}
          {loaded && (active.server?.model_path ?? active.model?.gguf_path) && <div className="llama-path"><strong>{fileName(active.server?.model_path ?? active.model?.gguf_path)}</strong><button title={active.server?.model_path ?? active.model?.gguf_path ?? ""} onClick={() => void copy(active.server?.model_path ?? active.model?.gguf_path ?? "")}>Copy path</button></div>}
          <div className="llama-facts">
            {active.managed && <Badge tone="good">Started by gateway</Badge>}{active.external && <Badge tone="warn">External server</Badge>}
            {active.server?.n_ctx != null && <span>Context {active.server.n_ctx.toLocaleString()}{active.server.n_ctx_train != null && ` · trained for ${active.server.n_ctx_train.toLocaleString()}`}</span>}
            {active.server?.n_params != null && <span>{formatParams(active.server.n_params)} parameters</span>}
            {active.server?.model_size_bytes != null && <span>{formatBytes(active.server.model_size_bytes)}</span>}
            {active.server?.vision && <Badge>Vision</Badge>}{active.server?.audio && <Badge>Audio</Badge>}
            {active.server?.total_slots != null && <span>{active.server.total_slots} slot{active.server.total_slots === 1 ? "" : "s"}</span>}
            {active.server?.build_info && <span>Build {active.server.build_info}</span>}
            {active.uptime_secs != null && <span>Uptime {active.uptime_secs}s</span>}{active.pid != null && <span>PID {active.pid}</span>}
          </div>
          <div className="llama-actions"><button disabled={!active.managed || !!busy} onClick={() => active.state === "ready" ? setConfirm({ title: "Stop llama-server?", message: "Stopping llama-server will interrupt any requests currently using it.", action: doStop }) : void doStop()}>{busy === "stop" ? "Stopping…" : "Stop"}</button><button disabled={active.external || !active.model || !!busy} onClick={() => setConfirm({ title: "Restart llama-server?", message: active.managed ? `Restarting ${active.model?.alias ?? "the model"} will interrupt active requests. It will keep the same arguments.` : `Start ${active.model?.alias ?? "the last model"} again with the same arguments?`, action: doRestart })}>{busy === "restart" ? "Restarting…" : "Restart"}</button><button onClick={() => launcherRef.current?.scrollIntoView({ behavior: "smooth" })}>Switch…</button></div>
          <p className="llama-small">Endpoint: <code>{active.endpoint.base_url}</code> <button onClick={() => void copy(active.endpoint.base_url)}>Copy</button> · Point the gateway&apos;s <code>llama</code> provider at this port. Provider health refreshes every 30 s.</p>
          {active.external && <div className="llama-callout">A llama-server is already running on port {active.endpoint.port} (not started by this gateway). Stop it outside the gateway before starting a model here. <code>Get-NetTCPConnection -LocalPort {active.endpoint.port} | ForEach-Object {'{'} Stop-Process -Id $_.OwningProcess {'}'}</code> <button onClick={() => void copy(`Get-NetTCPConnection -LocalPort ${active.endpoint.port} | ForEach-Object { Stop-Process -Id $_.OwningProcess }`)}>Copy command</button></div>}
          {(["exited", "failed"].includes(active.state)) && <div className="llama-callout danger"><strong>{active.last_exit?.reason ?? active.failure ?? "llama-server stopped unexpectedly"}</strong>{active.last_exit?.code != null && <span> · Exit code {active.last_exit.code}</span>}{active.last_exit?.log_tail && <pre className="llama-code">{active.last_exit.log_tail.map((line) => line.text).join("\n")}</pre>}<button disabled={!active.model || !!busy} onClick={() => active.model && void doRestart()}>Start again</button><button onClick={() => logRef.current?.scrollIntoView({ behavior: "smooth" })}>View full log</button></div>}
          {active.args && <details><summary>Show command</summary><pre className="llama-code">llama-server.exe {active.args.argv.map(quoteArg).join(" ")}</pre><button onClick={() => void copy(`llama-server.exe ${active.args!.argv.map(quoteArg).join(" ")}`)}><Copy size={13} /> Copy command</button></details>}
        </>}
      </div>
    </Panel>
    <section ref={launcherRef} className="panel"><div className="panel-head"><div><h2>Launch a model</h2><p>Choose a saved model and review the effective arguments</p></div></div><div className="llama-pad">
      {models.error && !models.data ? <PanelError error={models.error} retry={() => void models.refetch()} /> : <>
        {models.error && <Notice>Stale model list: {errorText(models.error)}</Notice>}
        {models.data?.models.length ? <><label className="llama-field">Saved model<select aria-label="Saved model" value={selectedId} onChange={(event) => { setSelectedOverride(event.target.value); setSaveArgs(false); }}>{[...models.data.models].sort((a, b) => a.alias.localeCompare(b.alias)).map((model) => <option key={model.id} value={model.id}>{model.alias} · {fileName(model.gguf_path)}{model.mmproj_path ? " · vision" : ""}{models.data?.running_model_id === model.id ? " · running" : ""}</option>)}</select></label><label className="llama-field">Arguments<textarea aria-label="Launch arguments" value={args} spellCheck={false} rows={3} onChange={(event) => setArgsEdit({ id: selectedId, value: event.target.value })} /></label><p className="llama-small">{argsSource} · <button onClick={() => setArgsEdit(null)}>Reset to saved</button></p><ParsePreview parse={currentParse} error={parseError} /><label className="llama-check"><input type="checkbox" checked={saveArgs} onChange={(event) => setSaveArgs(event.target.checked)} /> Save these arguments to this model</label><div className="llama-actions"><button className="primary" disabled={!selected || !!busy || active?.external || !!parseError || !currentParse || currentParse.issues.length > 0} onClick={() => selected && requestStart(selected, args, saveArgs)}>{busy === "start" ? "Starting…" : active?.managed ? `Switch to ${selected?.alias}` : "Start"}</button><button onClick={() => setEditModel("new")}>New model…</button></div>{active?.external && <p className="llama-small">Start is disabled while an external server owns port {active.endpoint.port}.</p>}{selected && <details><summary>Show proposed command</summary><p className="llama-small">Model, mmproj, host, and port are managed by the gateway.</p><pre className="llama-code">llama-server.exe -m {quoteArg(selected.gguf_path)} {selected.mmproj_path ? `--mmproj ${quoteArg(selected.mmproj_path)} ` : ""}{currentParse?.argv.map(quoteArg).join(" ")} --host {active?.endpoint.host} --port {active?.endpoint.port}</pre></details>}</> : <><p>No saved models yet — add one below.</p><button onClick={() => setEditModel("new")}>New model…</button></>}
      </>}
      {!!actionError && <div className="llama-callout danger" role="alert">{errorText(actionError)}{actionError instanceof ApiError && actionError.code === "path_not_found" && <button onClick={() => selected && setEditModel(selected)}>Edit model</button>}</div>}
      <Updated at={models.dataUpdatedAt} />
    </div></section>
    <ModelsPanel models={models} scan={scan} editModel={editModel} setEditModel={setEditModel} onStart={(model) => { setSelectedOverride(model.id); requestStart(model, model.args ?? defaults.data?.args ?? "", false); }} busy={busy} active={active} onChanged={() => void client.invalidateQueries({ queryKey: [mode, "llama"] })} />
    <DefaultsPanel mode={mode} query={defaults} onChanged={() => void client.invalidateQueries({ queryKey: [mode, "llama"] })} />
    <section ref={logRef}><LogsPanel key={mode} mode={mode} enabled={enabled} visible={visible && logInView && !paused} state={active?.state} zone={zone} /></section>
    {confirm && <Confirm title={confirm.title} message={confirm.message} onCancel={() => setConfirm(null)} onConfirm={() => { const action = confirm.action; setConfirm(null); void action(); }} />}
  </>;
}

function ScenarioPicker({ value, onChange }: { value: string; onChange: (value: string) => void }) { return <label className="llama-scenario">Demo scenario <select value={value} onChange={(event) => onChange(event.target.value)}>{scenarios.map((item) => <option key={item}>{item}</option>)}</select></label>; }
export function Confirm({ title, message, onCancel, onConfirm }: { title: string; message: string; onCancel: () => void; onConfirm: () => void }) {
  return <Dialog.Root open onOpenChange={(open) => { if (!open) onCancel(); }}><Dialog.Portal><Dialog.Overlay className="llama-modal-backdrop" /><Dialog.Content className="llama-modal"><Dialog.Title>{title}</Dialog.Title><Dialog.Description>{message}</Dialog.Description><div className="llama-actions"><Dialog.Close asChild><button>Cancel</button></Dialog.Close><button className="primary" onClick={onConfirm}>Confirm</button></div></Dialog.Content></Dialog.Portal></Dialog.Root>;
}
export function ParsePreview({ parse, error }: { parse: ParseResult | null; error: unknown }) { return <div className="llama-preview">{!!error && <p role="alert">{errorText(error)}</p>}{parse?.issues.map((issue, i) => <p role="alert" key={i}>{issue.message} <small>({issue.code})</small></p>)}{parse && !parse.issues.length && <div className="llama-chips">{parse.argv.length ? parse.argv.map((token, i) => <code key={i}>{token}</code>) : <small>No extra arguments</small>}</div>}<details><summary>Argument rules</summary><p>Whitespace separates arguments. Single and double quotes group text; the other quote remains literal inside. Backslashes are literal, so Windows paths need no escaping. The gateway owns and rejects: <code>-m / --model</code>, <code>--mmproj / -mm</code>, <code>--mmproj-url</code>, <code>--model-url</code>, <code>--hf-repo / -hf</code>, <code>--hf-file</code>, <code>--docker-repo</code>, <code>--host</code>, <code>--port</code>, <code>--models-dir</code>, <code>--models-preset</code>, <code>-h / --help</code>, <code>--version</code>, <code>--license</code>, <code>--completion-bash</code>, and <code>--list-devices</code>. <code>--alias</code> is added when enabled unless explicitly supplied.</p></details></div>; }

function DefaultsPanel({ mode, query, onChanged }: { mode: "live" | "demo"; query: ReturnType<typeof useQuery<{ args: string }>>; onChanged: () => void }) {
  const [edited, setEdited] = useState<string | null>(null); const [parse, setParse] = useState<ParseResult | null>(null); const [parsedValue, setParsedValue] = useState<string | null>(null); const [error, setError] = useState<unknown>(null); const [busy, setBusy] = useState(false);
  const value = edited ?? query.data?.args ?? "";
  const currentParse = parsedValue === value ? parse : null;
  useEffect(() => { const controller = new AbortController(); const timer = setTimeout(async () => { try { setParse(mode === "demo" ? demoParse(value) : await llamaRequest("args/parse", "POST", { args: value }, controller.signal)); setParsedValue(value); setError(null); } catch (e) { if (!controller.signal.aborted) setError(e); } }, 250); return () => { clearTimeout(timer); controller.abort(); }; }, [mode, value]);
  async function save() { setBusy(true); setError(null); try { if (mode === "demo") demoDefaults(value); else await llamaRequest("defaults", "PUT", { args: value }); onChanged(); } catch (e) { setError(e); } finally { setBusy(false); } }
  return <Panel title="Default arguments" scope="Used by models without their own arguments" actions={<><Updated at={query.dataUpdatedAt} /><button onClick={() => void query.refetch()}>Refresh</button></>}><div className="llama-pad">{query.error && !query.data ? <PanelError error={query.error} retry={() => void query.refetch()} /> : <>{query.error && <Notice>Stale defaults: {errorText(query.error)}</Notice>}<label className="llama-field">Global default arguments<textarea value={value} onChange={(event) => setEdited(event.target.value)} rows={3} spellCheck={false} placeholder="-ngl all -fa on -ctk iq4_nl -ctv iq4_nl --tools all -np 1 --ui-mcp-proxy" /></label><ParsePreview parse={currentParse} error={error} /><button className="primary" disabled={busy || !!error || !currentParse || !!currentParse.issues.length} onClick={() => void save()}>{busy ? "Saving…" : "Save defaults"}</button></>}</div></Panel>;
}
