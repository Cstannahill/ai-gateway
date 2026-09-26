"use client";
import { useEffect, useState } from "react";
import type { UseQueryResult } from "@tanstack/react-query";
import * as Dialog from "@radix-ui/react-dialog";
import { Badge, Notice, Panel } from "@/components/ui";
import { useRuntime } from "@/components/runtime";
import { demoDelete, demoSave } from "@/lib/llama-demo";
import { fileName, formatBytes, llamaRequest, type ModelInput, type ModelList, type SavedModel, type ScanFile, type ScanResult, type Status, type Validation } from "@/lib/llama";
import { errorText } from "./llama";

const empty: ModelInput = { alias: "", gguf_path: "", mmproj_path: null, args: null, pass_alias: true, notes: null };
export function ModelsPanel({ models, scan, editModel, setEditModel, onStart, busy, active, onChanged }: {
  models: UseQueryResult<ModelList>; scan: UseQueryResult<ScanResult>; editModel: SavedModel | "new" | null;
  setEditModel: (value: SavedModel | "new" | null) => void; onStart: (model: SavedModel) => void;
  busy: string; active: Status | null; onChanged: () => void;
}) {
  const { mode, zone } = useRuntime();
  const [deleting, setDeleting] = useState<SavedModel | null>(null);
  const [deleteError, setDeleteError] = useState<unknown>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  async function remove() {
    if (!deleting) return;
    setDeleteBusy(true); setDeleteError(null);
    try { if (mode === "demo") demoDelete(deleting.id); else await llamaRequest(`models/${deleting.id}`, "DELETE"); setDeleting(null); onChanged(); }
    catch (error) { setDeleteError(error); } finally { setDeleteBusy(false); }
  }
  return <>
    <Panel title="Saved models" scope="Local model paths and their launch settings" actions={<><small className="llama-updated">{models.dataUpdatedAt ? `Updated ${new Date(models.dataUpdatedAt).toLocaleTimeString()}` : "Not updated"}</small><button onClick={() => void models.refetch()}>Refresh</button><button onClick={() => setEditModel("new")}>Add model</button></>}>
      {models.error && !models.data ? <div className="llama-pad" role="alert">{errorText(models.error)} <button onClick={() => void models.refetch()}>Retry</button></div> : <>
        {models.error && <Notice>Stale model list: {errorText(models.error)}</Notice>}
        {!models.data?.models.length ? <p className="llama-pad">No saved models yet. Add a model to launch it.</p> : <div className="table-scroll"><table className="llama-table"><thead><tr><th>Alias</th><th>Model file</th><th>mmproj</th><th>Arguments</th><th>Updated</th><th>Actions</th></tr></thead><tbody>{[...models.data.models].sort((a, b) => a.alias.localeCompare(b.alias)).map((model) => { const running = active?.managed && active.model?.id === model.id; return <tr key={model.id} className={running ? "llama-running" : ""}><td><strong>{model.alias}</strong>{running && <Badge tone="good">Running</Badge>}</td><td title={model.gguf_path}>{fileName(model.gguf_path)}</td><td>{model.mmproj_path ? <Badge>Yes</Badge> : "—"}</td><td title={model.args ?? "Inherits defaults"} className="llama-truncate">{model.args === null ? "Inherit defaults" : model.args || "No extra arguments"}</td><td>{new Date(model.updated_at).toLocaleString(undefined, { timeZone: zone === "UTC" ? "UTC" : undefined })}</td><td><div className="llama-row-actions"><button disabled={!!busy || !!active?.external} aria-label={`Start ${model.alias}`} onClick={() => onStart(model)}>Start</button><button aria-label={`Edit ${model.alias}`} onClick={() => setEditModel(model)}>Edit</button><button disabled={!!running} title={running ? "Stop the server first" : undefined} aria-label={`Delete ${model.alias}`} onClick={() => setDeleting(model)}>Delete</button></div></td></tr>; })}</tbody></table></div>}
      </>}
    </Panel>
    <ModelDialog key={editModel === "new" ? "new" : editModel?.id ?? "closed"} model={editModel} close={() => setEditModel(null)} scan={scan} mode={mode} onChanged={onChanged} />
    <Dialog.Root open={!!deleting} onOpenChange={(open) => { if (!open) { setDeleting(null); setDeleteError(null); } }}><Dialog.Portal><Dialog.Overlay className="llama-modal-backdrop" /><Dialog.Content className="llama-modal" aria-describedby="llama-delete-description"><Dialog.Title>Delete {deleting?.alias}?</Dialog.Title><Dialog.Description id="llama-delete-description">This removes the saved model entry. Files on disk are untouched.</Dialog.Description>{!!deleteError && <p role="alert">{errorText(deleteError)}</p>}<div className="llama-actions"><Dialog.Close asChild><button>Cancel</button></Dialog.Close><button disabled={deleteBusy} className="primary" onClick={() => void remove()}>{deleteBusy ? "Deleting…" : "Delete saved model"}</button></div></Dialog.Content></Dialog.Portal></Dialog.Root>
  </>;
}

function ModelDialog({ model, close, scan, mode, onChanged }: { model: SavedModel | "new" | null; close: () => void; scan: UseQueryResult<ScanResult>; mode: "live" | "demo"; onChanged: () => void }) {
  const editing = model && model !== "new" ? model : null;
  const [form, setForm] = useState<ModelInput>(editing ? { alias: editing.alias, gguf_path: editing.gguf_path, mmproj_path: editing.mmproj_path, args: editing.args, pass_alias: editing.pass_alias, notes: editing.notes } : empty);
  const [inherit, setInherit] = useState(form.args === null);
  const [validation, setValidation] = useState<Validation | null>(null);
  const [validating, setValidating] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [fieldError, setFieldError] = useState("");
  const [busy, setBusy] = useState(false);
  const [picker, setPicker] = useState<"gguf" | "mmproj" | null>(null);
  const [search, setSearch] = useState("");
  const patch = (value: Partial<ModelInput>) => { setForm((old) => ({ ...old, ...value })); setValidation(null); setValidating(true); setFieldError(""); setError(null); };
  useEffect(() => {
    if (!model) return;
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      try {
        const result = mode === "demo" ? { valid: !!form.alias && form.gguf_path.toLowerCase().endsWith(".gguf"), gguf_path: form.gguf_path, mmproj_path: form.mmproj_path, alias_taken: false, error: !form.alias ? { code: "invalid_alias", message: "Alias is required" } : !form.gguf_path.toLowerCase().endsWith(".gguf") ? { code: "not_gguf", message: "Model path must end in .gguf" } : undefined } : await llamaRequest<Validation>("models/validate", "POST", form, controller.signal);
        setValidation(result); setValidating(false);
      } catch (e) { if (!controller.signal.aborted) { setError(e); setValidating(false); } }
    }, 400);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [form, model, mode]);
  const aliasTaken = validation?.alias_taken && (!editing || editing.alias.toLowerCase() !== form.alias.toLowerCase());
  const validationCode = validation?.valid ? "" : validation?.error?.code ?? "";
  const validationMessage = validation?.error ? `${validation.error.message} (${validation.error.code})` : "";
  const validationField = validationCode === "invalid_alias" ? "alias" : ["invalid_path", "path_not_found", "path_not_file", "not_gguf"].includes(validationCode) ? (validationMessage.toLowerCase().includes("mmproj") ? "mmproj" : "gguf") : ["reserved_flag", "invalid_args"].includes(validationCode) ? "args" : "";
  async function save() {
    setBusy(true); setError(null); setFieldError("");
    try {
      if (mode === "demo") demoSave(form, editing?.id);
      else await llamaRequest(editing ? `models/${editing.id}` : "models", editing ? "PUT" : "POST", form);
      onChanged(); close();
    } catch (e) {
      setError(e);
      const code = e && typeof e === "object" && "code" in e ? String(e.code) : "";
      if (["invalid_alias", "alias_taken"].includes(code)) setFieldError("alias");
      else if (["invalid_path", "path_not_found", "path_not_file", "not_gguf"].includes(code)) setFieldError(errorText(e).toLowerCase().includes("mmproj") ? "mmproj" : "gguf");
      else if (["reserved_flag", "invalid_args"].includes(code)) setFieldError("args");
    } finally { setBusy(false); }
  }
  const options = scan.data?.dirs.flatMap((dir) => picker === "mmproj" ? dir.mmproj_candidates : dir.models) ?? [];
  const filtered = options.filter((file) => `${file.file_name} ${file.path}`.toLowerCase().includes(search.toLowerCase())).slice(0, 200);
  const choose = (file: ScanFile) => {
    if (picker === "gguf") {
      const stem = fileName(file.path).replace(/\.gguf$/i, "").toLowerCase();
      const suggestion = scan.data?.dirs.flatMap((dir) => dir.mmproj_candidates).find((item) => item.file_name.toLowerCase().includes(stem));
      patch({ gguf_path: file.path, ...(!form.mmproj_path && suggestion ? { mmproj_path: suggestion.path } : {}) });
    } else patch({ mmproj_path: file.path });
    setPicker(null); setSearch("");
  };
  return <Dialog.Root open={!!model} onOpenChange={(open) => { if (!open) close(); }}><Dialog.Portal><Dialog.Overlay className="llama-modal-backdrop" /><Dialog.Content className="llama-modal llama-model-dialog"><Dialog.Title>{editing ? `Edit ${editing.alias}` : "Add a saved model"}</Dialog.Title><Dialog.Description>Paths are on the gateway machine. A browser file picker cannot provide absolute paths.</Dialog.Description>
    <div className="llama-dialog-body">
      <label className="llama-field">Alias<input aria-label="Model alias" value={form.alias} onChange={(e) => patch({ alias: e.target.value })} autoFocus />{fieldError === "alias" && !!error && <small role="alert">{errorText(error)}</small>}{validationField === "alias" && <small role="alert">{validationMessage}</small>}</label>
      <label className="llama-field">Model file path (.gguf)<input aria-label="Model file path" value={form.gguf_path} onChange={(e) => patch({ gguf_path: e.target.value })} spellCheck={false} />{fieldError === "gguf" && !!error && <small role="alert">{errorText(error)}</small>}{validationField === "gguf" && <small role="alert">{validationMessage}</small>}{validation?.valid && validation.gguf_path && validation.gguf_path !== form.gguf_path && <small>Canonical path: {validation.gguf_path}</small>}</label>
      {scan.data?.configured && <button onClick={() => setPicker(picker === "gguf" ? null : "gguf")}>Choose model from scanned files…</button>}
      <label className="llama-field">mmproj file path (optional)<input aria-label="mmproj file path" value={form.mmproj_path ?? ""} onChange={(e) => patch({ mmproj_path: e.target.value || null })} spellCheck={false} />{fieldError === "mmproj" && !!error && <small role="alert">{errorText(error)}</small>}{validationField === "mmproj" && <small role="alert">{validationMessage}</small>}</label>
      {scan.data?.configured && <button onClick={() => setPicker(picker === "mmproj" ? null : "mmproj")}>Choose mmproj from scanned files…</button>}
      {scan.error && <p className="llama-small">Scan unavailable: {errorText(scan.error)} <button onClick={() => void scan.refetch()}>Retry</button></p>}
      {picker && <div className="llama-picker"><label className="llama-field">Search scanned files<input value={search} onChange={(e) => setSearch(e.target.value)} /></label>{scan.data?.dirs.filter((dir) => !dir.exists).map((dir) => <p key={dir.dir}>{dir.dir} does not exist</p>)}{scan.data?.dirs.filter((dir) => dir.truncated).map((dir) => <p key={dir.dir}>{dir.dir} scan reached its file limit; type a path if it is missing.</p>)}{filtered.map((file) => <button key={file.path} title={file.path} onClick={() => choose(file)}><strong>{file.file_name}</strong><small>{formatBytes(file.size_bytes)}{file.already_saved ? " · already saved" : ""}</small></button>)}{!filtered.length && <p>No scanned files match.</p>}</div>}
      <label className="llama-check"><input type="checkbox" checked={inherit} onChange={(e) => { setInherit(e.target.checked); patch({ args: e.target.checked ? null : "" }); }} /> Inherit global defaults</label>
      {!inherit && <label className="llama-field">Model arguments<textarea aria-label="Model arguments" value={form.args ?? ""} onChange={(e) => patch({ args: e.target.value })} rows={3} spellCheck={false} />{fieldError === "args" && !!error && <small role="alert">{errorText(error)}</small>}{validationField === "args" && <small role="alert">{validationMessage}</small>}<small>Leave empty to pass no extra arguments. Check inherit to use global defaults.</small></label>}
      <label className="llama-check"><input type="checkbox" checked={form.pass_alias} onChange={(e) => patch({ pass_alias: e.target.checked })} /> Pass --alias to llama-server</label>
      <label className="llama-field">Notes<textarea aria-label="Model notes" value={form.notes ?? ""} onChange={(e) => patch({ notes: e.target.value || null })} rows={2} /></label>
      {validating && <p className="llama-small">Validating…</p>}
      {validation && !validation.valid && !validationField && <p role="alert" className="llama-field-error">{validation.error?.message} <small>({validation.error?.code})</small></p>}
      {aliasTaken && <p role="alert" className="llama-field-error">Alias is already saved. <small>(alias_taken)</small></p>}
      {!!error && !fieldError && <p role="alert" className="llama-field-error">{errorText(error)}</p>}
    </div>
    <div className="llama-actions"><Dialog.Close asChild><button>Cancel</button></Dialog.Close><button className="primary" disabled={busy || validating || !validation?.valid || !!aliasTaken || !!error} onClick={() => void save()}>{busy ? "Saving…" : "Save model"}</button></div>
  </Dialog.Content></Dialog.Portal></Dialog.Root>;
}
