# llama-server manager — UI handoff

Everything the frontend agent needs to build the llama-server management UI. The backend is **finished and verified** (against a fake server and the real `llama-server.exe`); this document is the contract. Nothing here requires further backend changes; if you find a gap, record it in `docs/frontend/API_GAPS.md` in the existing style rather than working around it silently.

Source of truth for behavior: `src/llama_server/` (logic), `src/server/routes/llama_server.rs` (HTTP), `tests/llama_server_tests.rs` + `tests/llama_server_http/mod.rs` (executable examples of every flow below).

---

## 1. What the feature is

The user runs llama.cpp's `llama-server` locally, always with a long argument string such as

```
-ngl all -fa on -ctk iq4_nl -ctv iq4_nl --tools all -np 1 --ui-mcp-proxy
```

The gateway can now **own that process**: save models (a `.gguf`, optionally with an `mmproj` `.gguf`) under an alias the user picks, start/stop/restart the server with a chosen saved model and editable arguments, and report what is currently loaded. One llama-server process at a time (no multi-model/router mode).

User stories the UI must serve:

1. **Save a model** — give it an alias, paste (or pick) a `.gguf` path, optionally an `mmproj` path and per-model default arguments.
2. **Start a saved model** — choose an alias from a **dropdown of saved models**, review/edit the arguments, press Start.
3. **Change arguments** — edit the argument string before starting; optionally save it onto the model; keep a global default argument string.
4. **See what is loaded right now** — alias, file, context size, vision support, parameters, state, uptime; works even if the server was started outside the gateway.
5. **Switch / stop / restart** — switch models in one action, stop, restart with the same arguments.
6. **Diagnose** — see live server logs, and the reason and last log lines when it crashed or failed to start.

---

## 2. Hard rules (read before writing any code)

### 2.1 The feature can be disabled
It is **off by default** (`llama_server.enabled: false` in gateway config). `GET /llama-server/status` still answers `200` and returns `{"enabled": false, "state": "disabled", "message": "..."}`. The UI must render a clear **"llama-server management is not enabled"** state (with the config snippet from §9) instead of an error. Every other endpoint returns `404` `feature_disabled` in that case.

### 2.2 The existing proxy cannot be used — you need a new one
`frontend/src/app/api/gateway/[...path]/route.ts` (via `proxyRead`) forwards **GET only** to an allowlist. These endpoints need POST/PUT/DELETE, so add a dedicated same-origin proxy, e.g. `frontend/src/app/api/llama-server/[...path]/route.ts`, following the same discipline as the existing chat proxy:

- Forward **only** these paths (regexes, anchored) and methods; reject everything else with 404/405:

  | Method | Path |
  |---|---|
  | GET | `/llama-server/status`, `/llama-server/logs`, `/llama-server/models`, `/llama-server/models/scan`, `/llama-server/defaults` |
  | POST | `/llama-server/start`, `/llama-server/stop`, `/llama-server/restart`, `/llama-server/models`, `/llama-server/models/validate`, `/llama-server/args/parse` |
  | PUT | `/llama-server/models/{uuid}`, `/llama-server/defaults` |
  | DELETE | `/llama-server/models/{uuid}` |

- Validate `{uuid}` as a UUID. Validate query params (`after`: non-negative integer, `limit`: 1–2000).
- JSON bodies only (`content-type: application/json`), size cap ≤ 64 KiB, parse-and-reforward (do not pipe raw bytes).
- Same-origin only (check `Origin`/`Sec-Fetch-Site` on the *incoming* browser request against your own origin; refuse cross-site). This is important: it is the frontend's half of the security model (§2.3).
- Timeouts: 15 s (stop can legitimately take up to ~10 s; start/status probe up to ~2 s). No redirects. No cookies/Authorization forwarded. `Cache-Control: no-store`.
- Use `GATEWAY_URL` exactly like the existing proxy.

### 2.3 Security model — and one rule you must not break
This feature can **launch a local executable**, and the gateway's CORS is permissive. So the backend refuses every `/llama-server/*` request (reads included) that looks like it came from a browser page: any request with an `Origin` header, a cross-site `Sec-Fetch-Site`, or a non-loopback `Host`. It returns `403` `origin_forbidden` / `host_forbidden`.

**Consequence for you: the Next.js server-side proxy must build its outgoing request from scratch and must NOT copy the incoming request's `Origin`, `Host`, `Referer`, `Cookie`, `Authorization` or any `Sec-Fetch-*` headers.** Send only `content-type: application/json` (when there is a body) and `accept: application/json`. Node's `fetch` sends no `Origin` by default, so this passes. If the UI ever gets a `403 origin_forbidden` from the backend, the proxy is leaking a browser header — fix the proxy, do not add the origin to `llama_server.allowed_origins`.

The browser must never call the gateway directly for these routes (no `NEXT_PUBLIC_` gateway URL, no CORS workaround).

### 2.4 Don't assume file paths can come from a browser file picker
`<input type="file">` never exposes absolute paths. Paths are typed/pasted, or chosen from the **scan** listing (§4.12). Paths are Windows paths (`S:\Code\Models\x.gguf`); forward slashes are accepted too. Always treat them as opaque strings.

### 2.5 Live vs demo mode
The app already has explicit, labelled **demo mode** (fixtures, no gateway calls). Provide demo fixtures for this view too (§8), clearly labelled; API errors must never silently activate demo data. **Live mode can start and stop a real process on the user's GPU** — confirmations in §7 apply.

---

## 3. Data model and states

### 3.1 TypeScript types (mirror these exactly)

```ts
type LlamaState =
  | "disabled"   // only from GET /status when the feature is off
  | "stopped"    // nothing running (never started, or stopped on request)
  | "starting"   // process spawned, server not answering yet
  | "loading"    // server answers, /health says the model is still loading
  | "ready"      // model loaded, serving
  | "exited"     // the process ended on its own (crash / model failed to load)
  | "failed";    // the start failed (could not spawn, or start timeout)

interface SavedModel {
  id: string;                 // UUID
  alias: string;              // unique case-insensitively; [A-Za-z0-9._-]{1,64}
  gguf_path: string;          // canonical absolute path
  mmproj_path: string | null; // canonical absolute path
  args: string | null;        // null = inherit global defaults; "" = explicitly no extra args
  pass_alias: boolean;        // pass `--alias <alias>` so the server reports the alias as its model id
  notes: string | null;
  created_at: string;         // RFC 3339 UTC
  updated_at: string;
}

interface LogLine {
  seq: number;                // monotonically increasing, never reused, survives restarts
  ts: string;
  stream: "stdout" | "stderr" | "gateway"; // "gateway" = markers written by the manager
  text: string;
}

interface ServerInfo {        // what the LIVE server says about itself; every field optional
  health: "ok" | "loading" | "error" | null;
  model_path: string | null;
  model_id: string | null;    // the --alias, or the file name
  n_ctx: number | null;       // context size it is running with
  n_ctx_train: number | null; // context the model was trained for
  n_params: number | null;
  model_size_bytes: number | null;
  vision: boolean | null;
  audio: boolean | null;
  build_info: string | null;  // e.g. "b10434-7e4c0a968"
  total_slots: number | null; // -np
}

interface ExitInfo {
  code: number | null;
  at: string;
  unexpected: boolean;        // false when we stopped it
  reason: string;
  log_tail: LogLine[];        // last ~30 lines at the time — usually the explanation
}

interface Status {
  enabled: true;
  state: Exclude<LlamaState, "disabled">;
  managed: boolean;           // the gateway has a live child process
  external: boolean;          // a server answers on the port but the gateway did not start it
  pid: number | null;
  started_at: string | null;
  ready_at: string | null;
  uptime_secs: number | null;
  model: { id: string; alias: string; gguf_path: string; mmproj_path: string | null } | null; // what the gateway started (snapshot)
  args: { effective: string; argv: string[] } | null;  // effective = the string that applied; argv = full argv passed to the exe
  server: ServerInfo | null;  // null = nothing answering
  matched_model: { id: string; alias: string } | null; // saved model whose file the server has loaded
  failure: string | null;
  last_exit: ExitInfo | null;
  endpoint: { host: string; port: number; base_url: string };
}
type StatusResponse = Status | { enabled: false; state: "disabled"; message: string };

interface ApiError { error: { message: string; type: string; code: string } }
```

`model` is a **snapshot of what was started** — editing or deleting the saved model afterwards does not change it. `server` and `matched_model` come from **probing the live server** and are the truth for "what is loaded".

### 3.2 State machine

```
        start()                 /health 503               /health 200
stopped ───────► starting ─────────────► loading ─────────────► ready
   ▲                │  │                    │                     │
   │ stop()         │  └─ spawn error ──►  failed  ◄─ start timeout┘ (process is killed)
   │                ▼                                             │
   └────────────── (any running state) ── process ends by itself ─► exited
                    stop() from any running state ──► stopped
```

- `starting`/`loading` are transitions: poll (§6). `ready` is steady.
- `exited` and `failed` are **terminal but not errors of the UI** — they carry `last_exit`/`failure`; show them with the log tail and a "Start again" action.
- `external: true` can co-occur with `state: "stopped"` (or `exited`/`failed`): something is listening on the port that the gateway didn't start (or one from a previous gateway process that was force-killed). See §5.6.

### 3.3 What "currently loaded model" means (display rules)
Show, in priority order:
1. **Alias**: `matched_model.alias` if present; else `model.alias` (managed run); else `server.model_id`; else "unknown model".
2. **File**: `server.model_path`, else `model.gguf_path`. Show the file name prominently, the full path in a tooltip/copy button.
3. **Facts** (omit any that are null — never print "null" or "0"): context `n_ctx` (and "trained for `n_ctx_train`"), parameters `n_params` (format 9.0 B), file size `model_size_bytes`, vision/audio badges, slots `total_slots`, build `build_info`, uptime, pid.
4. **Source badge**: "Started by gateway" (`managed`) or "External server" (`external`).

---

## 4. API reference

Base path (through your proxy): `/api/llama-server/...` ⇄ gateway `/llama-server/...`. All bodies/responses JSON unless stated. **Errors** use the envelope `ApiError`; exceptions: malformed/missing JSON body or wrong `content-type` on a body route returns a plain-text `400/415/422` from the framework (not the envelope) — the proxy must pass it through and the client must not assume JSON on errors.

### 4.1 `GET /llama-server/status` → `StatusResponse` (200)
Cheap; safe to poll. It probes the server (≤ ~2 s worst case if the port is black-holed; normally ms).

### 4.2 `POST /llama-server/start` → `202` + `Status`
Body:
```json
{
  "model_id": "01a0df86-…",          // exactly one of model_id / alias
  "alias": "qwen-vl",
  "args": "-ngl all -fa on -np 1",   // optional; this run only. Omit → model.args → global defaults
  "replace": false,                  // true = stop a running server first (the "switch model" action)
  "save_args": false                 // true = also store `args` on the model (requires args)
}
```
The response is returned as soon as the process is spawned (`state` is `starting`/`loading`); **poll `/status`** until `ready` (or `exited`/`failed`). Errors: `400 invalid_request` (not exactly one selector, or `save_args` without `args`), `400 reserved_flag` / `invalid_args`, `400 path_not_found` (a model file was moved/deleted since saving), `404 not_found`, `409 already_running` (send `replace: true` to switch), `409 port_in_use` (something not started by the gateway is listening on the port), `500 spawn_failed`.

### 4.3 `POST /llama-server/stop` → `200` + `Status`
No body. Idempotent (stopping when stopped is a no-op). Takes up to ~10 s; show a busy state.

### 4.4 `POST /llama-server/restart` → `202` + `Status`
No body. Restarts the last-started model with the **same effective arguments**. `409 nothing_to_restart` if nothing was ever started in this gateway process.

### 4.5 `GET /llama-server/logs?after=<seq>&limit=<n>`
`limit` default 500, max 2000.
```json
{ "lines": [LogLine], "next_cursor": 42, "truncated": false, "state": "ready" }
```
Tail with a cursor: first call without `after`; then pass the previous `next_cursor` as `after` and you get only newer lines (empty `lines`, unchanged cursor when nothing is new). `truncated: true` means lines between your cursor and the first returned line were evicted from the server's ring buffer (2000 lines by default) — show a "…older output was dropped" divider. Sequence numbers continue across restarts, so a stale cursor can never re-show or skip lines from a different run. Includes `stream: "gateway"` marker lines (`--- starting 'x' (…) ---`, the exact command line `$ llama-server.exe -m …`, `--- llama-server ended: … ---`).

### 4.6 Saved models

`GET /llama-server/models` →
```json
{ "models": [SavedModel], "default_args": "-ngl all -fa on", "running_model_id": "…" | null }
```

`POST /llama-server/models` → `201` + `SavedModel`. Body (`PUT` uses the same shape and **replaces** the record — omitted optionals are cleared, so always send the full form):
```json
{ "alias": "qwen-vl", "gguf_path": "S:\\Code\\Models\\qwen.gguf",
  "mmproj_path": "S:\\Code\\Models\\mmproj-qwen.gguf",   // optional
  "args": "-ngl all -fa on",                             // optional: omit/null = inherit global defaults
  "pass_alias": true,                                    // optional, default true
  "notes": "Vision model" }                              // optional
```
Errors: `400 invalid_alias`, `invalid_path` (not absolute), `path_not_found`, `path_not_file`, `not_gguf` (must end in `.gguf`), `reserved_flag`, `invalid_args`; `409 alias_taken`.

`PUT /llama-server/models/{id}` → `200` + `SavedModel`; `404 not_found`; same 400/409s. Editing the running model is allowed and does not affect the running process.

`DELETE /llama-server/models/{id}` → `204`; `404 not_found`; `409 model_in_use` if it is the running model (stop first).

### 4.7 `POST /llama-server/models/validate` → always `200`
Same body as create. Use for live form validation (debounced) without saving:
```json
{ "valid": true,  "gguf_path": "S:\\…canonical…", "mmproj_path": null, "alias_taken": false }
{ "valid": false, "error": { "code": "not_gguf", "message": "model file '…' must have a .gguf extension" } }
```
Only the first problem is reported. `alias_taken: true` is informational (fine when editing that same model; block on create).

### 4.8 `GET/PUT /llama-server/defaults`
`{ "args": "-ngl all -fa on" }` both ways. `PUT` validates (`400 reserved_flag`/`invalid_args`). These apply to any model whose own `args` is `null`.

### 4.9 `POST /llama-server/args/parse` → always `200` (works even when the feature is disabled)
Body `{ "args": "-ngl all --port 9 'oops" }` →
```json
{ "argv": ["-ngl","all","--port","9"],
  "issues": [ { "code": "reserved_flag", "message": "'--port' is managed by the gateway …", "token": "--port" } ] }
```
`issues[].code`: `unbalanced_quote` (then `argv` is empty) or `reserved_flag`. Use it for the live tokenization preview and inline errors while typing (debounce ~250 ms).

### 4.10 Argument rules (put this in a help popover)
- Whitespace separates arguments; `"…"` and `'…'` group text (the other quote is literal inside). **Backslashes are literal**, so `C:\dir\file` needs no escaping.
- The gateway owns and rejects: `-m/--model`, `--mmproj`/`-mm`, `--mmproj-url`, `--model-url`, `--hf-repo`/`-hf`, `--hf-file`, `--docker-repo`, `--host`, `--port`, `--models-dir`, `--models-preset`, `-h/--help`, `--version`, `--license`, `--completion-bash`, `--list-devices`. Model/mmproj come from the saved model; host/port from gateway config.
- `--alias <alias>` is added automatically (when the model's `pass_alias` is true) unless the arguments already contain `-a`/`--alias`.
- Precedence of the argument **string**: request `args` → the model's saved `args` → global `default_args`. `Status.args.effective` reports which string applied and `argv` the exact final command line.

### 4.11 Effective command line
`Status.args.argv` is the exact argv after the executable. Offer a "Show command" disclosure that prints `llama-server.exe` + argv with a Copy button (quote tokens containing spaces for display only).

### 4.12 `GET /llama-server/models/scan`
```json
{ "configured": true,
  "dirs": [ { "dir": "S:\\Code\\Models", "exists": true, "truncated": false,
      "models": [ { "path": "S:\\Code\\Models\\qwen\\qwen.gguf", "file_name": "qwen.gguf",
                    "size_bytes": 5123456789, "modified": "2026-08-16T08:01:45Z", "already_saved": false } ],
      "mmproj_candidates": [ { "path": "…\\mmproj-qwen.gguf", "file_name": "mmproj-qwen.gguf", "size_bytes": 1, "modified": null, "already_saved": false } ] } ] }
```
Lists `*.gguf` under the directories the operator configured (`llama_server.scan_dirs`), depth ≤ 4, ≤ 2000 files per directory. `mmproj_candidates` are files whose name contains `mmproj`. `configured: false` (empty `dirs`) means the operator has not configured any — hide the picker and show the manual path fields only. A directory with `exists: false` is reported, not an error. Paths are for display/selection; saving still goes through `POST /models`.

---

## 5. Screens and behavior

Suggested place: a new top-level view in the existing console navigation ("llama-server"), URL-addressable like the other views (`?view=llama`). Follow the existing design system (graphite/cyan, high-contrast light theme, tabular numerics, panels that fail independently, reduced motion, keyboard operable). The page has four panels; each loads independently and shows its own error/stale state.

### 5.1 Status panel (top, always visible)
- **State badge**: `stopped` (neutral), `starting`/`loading` (info, animated unless reduced-motion; show elapsed time since `started_at`), `ready` (success), `exited`/`failed` (danger), `disabled` (muted).
- **Loaded model block** per §3.3 (alias, file, facts, source badge, uptime, pid).
- **Actions**: **Stop**, **Restart** (enabled only when a previous run exists), **Switch…** (focuses the launcher). Disable and show a spinner while a request is in flight; Stop can take ~10 s.
- **Endpoint line**: `endpoint.base_url` with a copy button (and a note that the gateway's `llama` provider should point at the same port — see §9).
- **`exited`/`failed` banner**: `last_exit.reason` / `failure`, exit code, and the `last_exit.log_tail` in a monospace block, plus **Start again** (re-runs with the same model/args) and "View full log". This is the primary diagnostic path when a model fails to load.
- **`external` banner** (§5.6).
- Poll per §6.

### 5.2 Launcher panel
1. **Model dropdown**: a combobox/select over `GET /models` — label `alias`, secondary text file name + `vision` badge if `mmproj_path`; sorted by alias; the running model marked "running". Empty state: "No saved models yet — add one below." Disabled with explanation when the feature is disabled.
2. **Arguments textarea** (monospace, autosize, spellcheck off), prefilled on model selection from `model.args ?? defaults`, with a small caption showing which source applied ("Model arguments" / "Global defaults" / "Edited"). The user may edit freely.
   - Live preview: debounced `POST /args/parse`; render `argv` as chips underneath; render `issues` inline (red, `role="alert"`), and **disable Start** while there are issues.
   - **"Save these arguments to this model"** checkbox → sends `save_args: true`.
   - "Reset to saved" link.
3. **Start** button:
   - Nothing running → `POST /start { model_id, args, save_args }`.
   - Something running (`managed`) → the button reads **Switch to <alias>** and sends `replace: true`; requires a confirmation (§7).
   - When `external` → Start is disabled with the reason (§5.6).
4. **Show command** disclosure (§4.11) using the preview argv plus the fixed parts (`-m`, `--mmproj`, `--host`, `--port` are shown as "managed by gateway" chips, since the UI does not know the final host/port until `Status.endpoint`; you may build the display from `endpoint`).

### 5.3 Saved models panel
A table: alias · file name (full path on hover) · mmproj (yes/—) · args (truncated, full on hover) · updated · actions (**Start**, **Edit**, **Delete**). The running model's row is highlighted and its **Delete** is disabled with a tooltip ("Stop the server first"; the API would return `409 model_in_use`).

**Add/Edit dialog** (also reachable from the launcher via "New model…"):
- Fields: Alias, Model file path (`.gguf`), mmproj file path (optional), Arguments (optional; empty ⇒ inherit global defaults — make this distinction visible: an "Inherit global defaults" checkbox that maps to `args: null`, versus an empty textarea that maps to `args: ""`), "Pass `--alias`" checkbox (default on), Notes.
- Path fields: text inputs plus a **"Choose from scanned files…"** button when `scan.configured` (opens a searchable list of `models` / `mmproj_candidates`, showing size and an "already saved" tag; picking one fills the field; when a model is picked, suggest an mmproj candidate whose file name shares the model's stem).
- **Live validation** with `POST /models/validate` (debounced ~400 ms) → inline error from `error.message`; show the canonicalized path returned in `gguf_path` under the field. Block Save while `valid: false`. On create, also block when `alias_taken`.
- Save → `POST`/`PUT`; map server errors by `code` to the relevant field (`invalid_alias`/`alias_taken` → alias; `invalid_path`/`path_not_found`/`path_not_file`/`not_gguf` → the path field it names; `reserved_flag`/`invalid_args` → args). Fallback: dialog-level error message.
- A deleted-on-disk file surfaces on Start as `400 path_not_found`; show it on the launcher and offer "Edit model".
- **Delete** → confirmation dialog (§7). Deleting a saved model never deletes files on disk — say so in the dialog.

### 5.4 Defaults panel
Small card: label "Default arguments (used by models without their own)", textarea, the same live parse preview and issue display, **Save** (`PUT /defaults`). Show the user's typical example as placeholder text: `-ngl all -fa on -ctk iq4_nl -ctv iq4_nl --tools all -np 1 --ui-mcp-proxy`.

### 5.5 Log panel
- Monospace, virtualized/bounded (keep at most ~5,000 lines client-side; drop oldest).
- Tail with the cursor (§4.5): first fetch without `after`, then poll (§6). Append lines; `stream` decides style (`stderr` muted-warning, `gateway` accent/italic markers, `stdout` default). `truncated: true` → insert a "…older output dropped" divider.
- Controls: **Pause/Resume** auto-scroll (auto-pause when the user scrolls up; "Jump to latest" button), **Copy visible**, **Download .txt** (of what is loaded client-side), text filter (client-side), wrap toggle.
- Timestamps: honor the existing UTC/local display preference.
- Do not persist logs in browser storage.

### 5.6 External server (important edge case)
`external: true` and not `managed` means a server answers on the configured port that this gateway process did not start. Typical causes: the user started llama-server by hand, or a previous gateway process was **force-killed** (a normal Ctrl+C/graceful shutdown stops the child; a hard kill cannot).
- Show an info banner: "A llama-server is already running on port N (not started by this gateway)." with what it reports (`server` facts, `matched_model.alias` if any).
- **The gateway cannot stop it** (it does not know its PID). Do not offer a Stop for it. Tell the user how to stop it, with copyable commands for the port shown in `endpoint.port` (PowerShell): `Get-NetTCPConnection -LocalPort <port> | ForEach-Object { Stop-Process -Id $_.OwningProcess }`.
- Disable Start with the reason; starting anyway would return `409 port_in_use`.
- The "currently loaded model" block still works from `server`/`matched_model`.

### 5.7 Disabled / unreachable states
- `disabled` (from status): full-panel explanation + config snippet (§9). Do not render the other panels.
- Proxy/network failure (`502` from your proxy): treat like the rest of the console — panel-level "gateway unreachable" with retry; keep last-known data marked stale.

---

## 6. Polling and refresh

| Situation | Poll `GET /status` | Poll `GET /logs` |
|---|---|---|
| Tab hidden | pause | pause |
| `starting` / `loading` | every 1 s | every 1 s |
| `ready` / `stopped` / `exited` / `failed` | every 5 s | every 2 s while the log panel is visible, otherwise off |
| After a mutating call | refetch status immediately | — |

- After **Start/Restart** (202), switch to 1 s polling until `ready`, `exited`, or `failed`; show elapsed time. Loading a large model can take minutes; do not time out client-side (the gateway enforces `start_timeout_secs`, default 300 s, and reports `failed` with `failure` text).
- Refetch `GET /models` after any model mutation and when the status `matched_model`/`model` changes.
- Avoid overlapping polls (skip a tick if the previous request is in flight).
- Manual **Refresh** button, and a "last updated" stamp per panel, consistent with the rest of the console.

---

## 7. Confirmations, safety, and copy

- **Stop** while the gateway's chat/inference may be in use: confirm — "Stopping llama-server will interrupt any requests currently using it." (Cheap heuristic: always confirm Stop when state is `ready`.)
- **Switch model** (`replace: true`) and **Restart**: same confirmation, naming the model being unloaded and loaded.
- **Delete saved model**: confirm; note that files on disk are untouched.
- **Start** is not confirmed when nothing is running.
- Never auto-start anything on page load. Never re-run a start automatically after a failure.
- Show the exact command (§4.11) because arguments are powerful (`--tools`, `--ui-mcp-proxy` enable capabilities on the server).
- All user-visible errors show `error.message` verbatim plus the `code` in a small secondary label (useful for bug reports).
- Buttons that trigger process actions need visible busy states and must be disabled while their request is in flight (prevents double-start).
- Accessibility: state changes announced via a polite `aria-live` region ("Model qwen-vl is ready"); the args preview issues use `role="alert"`; dialogs trap focus; table actions are real buttons with accessible names including the alias.

---

## 8. Demo mode fixtures (labelled, no gateway calls)

Provide at least: (a) disabled; (b) stopped with 3 saved models incl. one with an mmproj; (c) `loading` (progress over ~6 s, then `ready`); (d) `ready` with a vision model (`vision: true`, `n_ctx: 32768`, `n_ctx_train: 262144`, `n_params: 9000000000`, `total_slots: 1`); (e) `exited` with a `log_tail` ending in `error: failed to load model`; (f) `external`; (g) scan results with `already_saved` variety; (h) a `truncated` log page. Demo start/stop should mutate an in-memory fixture so the flows can be exercised. Every fixture-driven view carries the existing "demo data" label.

---

## 9. Operator setup (show this when disabled; also for the user's docs)

`config/gateway.yaml`:
```yaml
server:
  host: 127.0.0.1                 # required: this feature refuses a network-facing bind (see below)
llama_server:
  enabled: true
  binary: 'S:\llama.cpp\build\bin\Release\llama-server.exe'   # the real .exe, NOT the S:\bin\llama-server.cmd shim
  host: 127.0.0.1
  port: 8080                      # keep equal to the `llama` provider's base_url port
  state_file: config/llama_models.json
  scan_dirs: ['S:\Code\Models']   # optional: enables the "choose from scanned files" picker
  start_timeout_secs: 300
  log_lines: 2000
```
Startup validation rejects: missing/nonexistent `binary`, a `.cmd`/`.bat` shim (stopping a shim would orphan the real process), `port: 0`, and — because this feature launches a process — `server.host` not being loopback (unless `llama_server.allow_non_loopback: true`). Saved models live in `state_file` (JSON, human-editable while the gateway is stopped; a corrupt file makes startup fail with a clear message rather than being overwritten).

Relationship to routing: whatever the manager runs at `llama_server.port` is what the gateway's `llama` provider (`http://localhost:8080` in the sample config) talks to. The gateway's provider health check runs every 30 s, so the provider can appear unhealthy for up to ~30 s after the model becomes `ready`; mention this in the UI ("Provider health refreshes every 30 s") next to the endpoint line.

Lifecycle: a graceful gateway shutdown (Ctrl+C) stops the managed llama-server. A hard kill of the gateway cannot, and leaves an orphan that shows up as `external` (§5.6).

---

## 10. Error code reference

| HTTP | `error.code` | Meaning / UI handling |
|---|---|---|
| 403 | `origin_forbidden` | Browser origin reached the backend — proxy bug (§2.3) |
| 403 | `host_forbidden` | Non-loopback `Host` — proxy/config bug |
| 404 | `feature_disabled` | Feature off (only `/status` is tolerant) |
| 404 | `not_found` | Unknown model id / alias |
| 400 | `invalid_request` | Bad start selector or `save_args` without `args` |
| 400 | `invalid_alias` | Alias not `[A-Za-z0-9._-]{1,64}` |
| 400 | `invalid_path` | Path not absolute / empty |
| 400 | `path_not_found` | File doesn't exist (at save time, or moved since — seen on Start) |
| 400 | `path_not_file` | Path is not a file |
| 400 | `not_gguf` | Missing `.gguf` extension |
| 400 | `reserved_flag` | Argument owned by the gateway (§4.10) |
| 400 | `invalid_args` | e.g. unbalanced quote |
| 409 | `alias_taken` | Alias already used (case-insensitive) |
| 409 | `already_running` | Send `replace: true` to switch |
| 409 | `model_in_use` | Deleting the running model |
| 409 | `port_in_use` | Something else answers on the port |
| 409 | `nothing_to_restart` | No previous run |
| 500 | `spawn_failed` | The executable couldn't be started (also in `Status.failure`) |
| 500 | `internal_error` | e.g. can't write the state file |
| 4xx text | (none) | Framework rejection of a malformed JSON body — plain text |

---

## 11. Acceptance checklist

Manual (live, against the real gateway with `llama_server.enabled: true`; a tiny model such as `S:\Code\Models\MagicPrompt-SD\Gpt2-124M-Q8_0.gguf` with args `-ngl 0 -c 512` is enough and uses no GPU):
- [ ] Disabled gateway shows the "not enabled" state with the config snippet; no other panel renders.
- [ ] Add a model via the scan picker and via a typed path; invalid paths/aliases/args show inline field errors from `validate`.
- [ ] Dropdown lists saved models; selecting one prefills the arguments; the preview chips match; a reserved flag disables Start with an inline message.
- [ ] Start → badge goes `starting`/`loading` → `ready`; loaded-model block shows alias, file, context, params, slots, build; logs stream.
- [ ] Switch model (confirmation) unloads A and loads B; Restart keeps the arguments; Stop leaves state `stopped`.
- [ ] Editing arguments and starting with "save to model" persists them (reload the page: the saved args are prefilled).
- [ ] A model whose file was deleted on disk fails Start with a clear `path_not_found` message.
- [ ] A start with an unknown flag (e.g. `--definitely-not-a-flag`) shows `exited` (exit code 1) with the log tail containing `error: invalid argument: --definitely-not-a-flag` (verified against the real binary).
- [ ] Start a llama-server by hand on the configured port → external banner, Start disabled, loaded-model block still populated.
- [ ] Stop the gateway with Ctrl+C while a model is running → no `llama-server.exe` remains (`tasklist`).
- [ ] `403` never appears in normal use (if it does, the proxy is forwarding browser headers).
- [ ] Keyboard-only run-through works; reduced-motion respected; light/dark themes both legible.

Automated (Playwright, using route mocks for the proxy, in the existing `tests/` style): status-state matrix rendering (all `LlamaState` values + external); launcher preview/validation; start/switch/stop request bodies (assert `replace`, `save_args`, exact JSON); log cursor tailing appends without duplicates and honors `truncated`; model dialog error mapping per `error.code`; proxy unit tests (allowlist, UUID validation, header hygiene: assert no `Origin`/`Cookie`/`Host`/`Authorization`/`Sec-Fetch-*` on the outgoing request, body cap, timeout).

Backend regression tests to run when in doubt: `cargo test --test llama_server_tests`; the opt-in real-binary test is `tests/live_llama_server_test.rs` (env vars documented in its header).
