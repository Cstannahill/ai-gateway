// ─── Process supervisor ──────────────────────────────────────────────────────
//
// Owns at most one `llama-server` child process. Three background tasks per
// run:
//   * two readers drain stdout/stderr into the log ring buffer (they must keep
//     draining or a full pipe would block the child),
//   * a waiter owns the `Child`, reacts to "please kill" and to natural exit,
//   * a poller watches `/health` to move starting -> loading -> ready and to
//     enforce the start timeout.
// State lives in one small mutex-guarded struct that every task updates only
// while its `run_id` is still current, so a late task from a previous run can
// never overwrite the state of a newer one.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::LlamaError;
use super::args;
use super::logs::{LogBuffer, LogLine, LogPage};
use super::probe::{self, Health, ServerInfo};
use super::store::{ModelStore, SavedModel, canonical_string, validate_args, validate_model_file};
use crate::config::LlamaServerConfig;

/// How long `stop` waits for the process to be reaped before giving up.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Log lines attached to an unexpected-exit report.
const EXIT_TAIL_LINES: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Nothing running (never started, or stopped on request).
    Stopped,
    /// Process spawned, server not answering yet.
    Starting,
    /// Server answers but `/health` says it is still loading the model.
    Loading,
    /// `/health` is OK.
    Ready,
    /// The process ended on its own (crash, or the model failed to load).
    Exited,
    /// The start itself failed (spawn error or start timeout).
    Failed,
}

impl State {
    fn is_running(self) -> bool {
        matches!(self, State::Starting | State::Loading | State::Ready)
    }
}

/// The model a run was started with (a snapshot: editing or deleting the saved
/// model afterwards does not change what is running).
#[derive(Debug, Clone, Serialize)]
pub struct RunModel {
    pub id: Uuid,
    pub alias: String,
    pub gguf_path: String,
    pub mmproj_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub at: DateTime<Utc>,
    /// False when it ended because we stopped it.
    pub unexpected: bool,
    pub reason: String,
    /// Last log lines at the time of exit — usually the explanation.
    pub log_tail: Vec<LogLine>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EffectiveArgs {
    /// The argument string that applied (override > model > defaults).
    pub effective: String,
    /// The full argv passed to the executable, in order.
    pub argv: Vec<String>,
}

struct Shared {
    run_id: u64,
    state: State,
    model: Option<RunModel>,
    args: Option<EffectiveArgs>,
    pid: Option<u32>,
    started_at: Option<DateTime<Utc>>,
    started_instant: Option<Instant>,
    ready_at: Option<DateTime<Utc>>,
    last_exit: Option<ExitInfo>,
    failure: Option<String>,
}

struct Active {
    kill: Arc<Notify>,
    waiter: JoinHandle<()>,
    poller: JoinHandle<()>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub base_url: String,
}

/// Everything the status endpoint reports.
#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub enabled: bool,
    pub state: State,
    /// True while the gateway has a live child process.
    pub managed: bool,
    /// True when a server answers on the configured port but the gateway did
    /// not start it (or its run has ended).
    pub external: bool,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub ready_at: Option<DateTime<Utc>>,
    pub uptime_secs: Option<u64>,
    /// The model the gateway started (managed runs only).
    pub model: Option<RunModel>,
    pub args: Option<EffectiveArgs>,
    /// What the live server reports about itself (managed or external).
    pub server: Option<ServerInfo>,
    /// The saved model whose file matches what the server has loaded.
    pub matched_model: Option<MatchedModel>,
    pub failure: Option<String>,
    pub last_exit: Option<ExitInfo>,
    pub endpoint: Endpoint,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedModel {
    pub id: Uuid,
    pub alias: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScannedFile {
    pub path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub modified: Option<DateTime<Utc>>,
    pub already_saved: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScannedDir {
    pub dir: String,
    pub exists: bool,
    pub models: Vec<ScannedFile>,
    /// Files whose name contains `mmproj` — likely multimodal projectors.
    pub mmproj_candidates: Vec<ScannedFile>,
    /// True when the scan stopped at its file cap.
    pub truncated: bool,
}

pub struct LlamaServerManager {
    config: LlamaServerConfig,
    store: ModelStore,
    http: reqwest::Client,
    logs: Arc<LogBuffer>,
    shared: Arc<StdMutex<Shared>>,
    /// Serializes start/stop/restart; holds the handles of the live run.
    active: Mutex<Option<Active>>,
    next_run_id: AtomicU64,
}

impl LlamaServerManager {
    pub async fn new(config: LlamaServerConfig) -> Result<Self, LlamaError> {
        let store = ModelStore::open(&config.state_file).await?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| LlamaError::Internal(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            logs: Arc::new(LogBuffer::new(config.log_lines)),
            config,
            store,
            http,
            shared: Arc::new(StdMutex::new(Shared {
                run_id: 0,
                state: State::Stopped,
                model: None,
                args: None,
                pid: None,
                started_at: None,
                started_instant: None,
                ready_at: None,
                last_exit: None,
                failure: None,
            })),
            active: Mutex::new(None),
            next_run_id: AtomicU64::new(1),
        })
    }

    pub fn store(&self) -> &ModelStore {
        &self.store
    }

    pub fn config(&self) -> &LlamaServerConfig {
        &self.config
    }

    pub fn logs(&self, after: Option<u64>, limit: usize) -> LogPage {
        self.logs.read(after, limit)
    }

    fn base_url(&self) -> String {
        let host = &self.config.host;
        // A wildcard bind address isn't connectable; probe loopback instead.
        let host = if host == "0.0.0.0" || host == "::" { "127.0.0.1" } else { host.as_str() };
        if host.contains(':') && !host.starts_with('[') {
            format!("http://[{host}]:{}", self.config.port)
        } else {
            format!("http://{host}:{}", self.config.port)
        }
    }

    fn lock_shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current state without probing the server.
    pub fn state(&self) -> State {
        self.lock_shared().state
    }

    /// The id of the model the current (or last) run was started with.
    pub fn running_model_id(&self) -> Option<Uuid> {
        let s = self.lock_shared();
        if s.state.is_running() { s.model.as_ref().map(|m| m.id) } else { None }
    }

    // ─── Status ──────────────────────────────────────────────────────────

    pub async fn status(&self) -> StatusSnapshot {
        let server = probe::probe(&self.http, &self.base_url()).await;

        let (state, model, args, pid, started_at, ready_at, uptime, last_exit, failure) = {
            let s = self.lock_shared();
            (
                s.state,
                s.model.clone(),
                s.args.clone(),
                s.pid,
                s.started_at,
                s.ready_at,
                s.started_instant.filter(|_| s.state.is_running()).map(|i| i.elapsed().as_secs()),
                s.last_exit.clone(),
                s.failure.clone(),
            )
        };

        let managed = state.is_running();
        let external = !managed && server.is_some();

        let matched_model = match server.as_ref().and_then(|s| s.model_path.as_deref()) {
            Some(path) => self
                .store
                .list()
                .await
                .into_iter()
                .find(|m| probe::same_path(&m.gguf_path, path))
                .map(|m| MatchedModel { id: m.id, alias: m.alias }),
            None => None,
        };

        StatusSnapshot {
            enabled: true,
            state,
            managed,
            external,
            pid,
            started_at,
            ready_at,
            uptime_secs: uptime,
            model,
            args,
            server,
            matched_model,
            failure,
            last_exit,
            endpoint: Endpoint {
                host: self.config.host.clone(),
                port: self.config.port,
                base_url: self.base_url(),
            },
        }
    }

    // ─── Start / stop ────────────────────────────────────────────────────

    pub async fn start(
        &self,
        model_id: Uuid,
        args_override: Option<String>,
        replace: bool,
    ) -> Result<StatusSnapshot, LlamaError> {
        let mut active = self.active.lock().await;

        if self.state().is_running() {
            if !replace {
                return Err(LlamaError::conflict(
                    "already_running",
                    "llama-server is already running; stop it first or pass replace: true to switch".to_string(),
                ));
            }
            self.stop_locked(&mut active).await;
        } else if let Some(stale) = active.take() {
            stale.poller.abort();
            let _ = stale.waiter.await;
        }

        let model = self
            .store
            .get(model_id)
            .await
            .ok_or_else(|| LlamaError::NotFound(format!("no saved model with id {model_id}")))?;

        // Files can move between saving and starting.
        validate_model_file(&model.gguf_path, "model")?;
        if let Some(p) = &model.mmproj_path {
            validate_model_file(p, "mmproj")?;
        }

        let effective = match args_override {
            Some(a) => a,
            None => match &model.args {
                Some(a) => a.clone(),
                None => self.store.default_args().await,
            },
        };
        validate_args(&effective)?;
        let parsed = args::tokenize(&effective).map_err(|m| LlamaError::invalid("invalid_args", m))?;

        if probe::probe(&self.http, &self.base_url()).await.is_some() {
            return Err(LlamaError::conflict(
                "port_in_use",
                format!(
                    "something already answers on {} — stop that server first (it was not started by this gateway)",
                    self.base_url()
                ),
            ));
        }

        let argv = self.build_argv(&model, &parsed);
        let binary = self.config.binary.clone().unwrap_or_default();
        let run_id = self.next_run_id.fetch_add(1, Ordering::Relaxed);

        self.logs.push("gateway", format!("--- starting '{}' ({}) ---", model.alias, model.gguf_path));
        self.logs.push("gateway", format!("$ {binary} {}", argv.join(" ")));

        let mut cmd = tokio::process::Command::new(&binary);
        cmd.args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let message = format!("could not start '{binary}': {e}");
                self.logs.push("gateway", format!("--- {message} ---"));
                let mut s = self.lock_shared();
                let last_exit = s.last_exit.clone();
                *s = Shared {
                    run_id,
                    state: State::Failed,
                    model: Some(run_model(&model)),
                    args: Some(EffectiveArgs { effective, argv }),
                    pid: None,
                    started_at: None,
                    started_instant: None,
                    ready_at: None,
                    last_exit,
                    failure: Some(message.clone()),
                };
                return Err(LlamaError::Failed { code: "spawn_failed", message });
            }
        };

        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(spawn_reader(out, "stdout", self.logs.clone()));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(spawn_reader(err, "stderr", self.logs.clone()));
        }

        {
            let mut s = self.lock_shared();
            let last_exit = s.last_exit.take();
            *s = Shared {
                run_id,
                state: State::Starting,
                model: Some(run_model(&model)),
                args: Some(EffectiveArgs { effective, argv }),
                pid: child.id(),
                started_at: Some(Utc::now()),
                started_instant: Some(Instant::now()),
                ready_at: None,
                last_exit,
                failure: None,
            };
        }

        let kill = Arc::new(Notify::new());
        let waiter = tokio::spawn(wait_for_exit(
            child,
            readers,
            kill.clone(),
            self.shared.clone(),
            self.logs.clone(),
            run_id,
        ));
        let poller = tokio::spawn(poll_readiness(
            self.http.clone(),
            self.base_url(),
            Duration::from_secs(self.config.start_timeout_secs),
            kill.clone(),
            self.shared.clone(),
            run_id,
        ));
        *active = Some(Active { kill, waiter, poller });
        drop(active);

        Ok(self.status().await)
    }

    /// Restart the last-started model with the same effective arguments.
    pub async fn restart(&self) -> Result<StatusSnapshot, LlamaError> {
        let (model_id, effective) = {
            let s = self.lock_shared();
            match (&s.model, &s.args) {
                (Some(m), Some(a)) => (m.id, a.effective.clone()),
                _ => {
                    return Err(LlamaError::conflict(
                        "nothing_to_restart",
                        "no previous run to restart; start a model first".to_string(),
                    ));
                }
            }
        };
        self.start(model_id, Some(effective), true).await
    }

    /// Stop the managed process. Idempotent.
    pub async fn stop(&self) -> StatusSnapshot {
        {
            let mut active = self.active.lock().await;
            self.stop_locked(&mut active).await;
        }
        self.status().await
    }

    /// Called during gateway shutdown.
    pub async fn shutdown(&self) {
        let mut active = self.active.lock().await;
        self.stop_locked(&mut active).await;
    }

    async fn stop_locked(&self, active: &mut Option<Active>) {
        let Some(run) = active.take() else { return };
        run.poller.abort();
        run.kill.notify_one();
        let mut waiter = run.waiter;
        if tokio::time::timeout(STOP_TIMEOUT, &mut waiter).await.is_err() {
            // The waiter owns the child (kill_on_drop), so aborting it drops
            // the child and forces termination.
            waiter.abort();
            let _ = waiter.await;
            let mut s = self.lock_shared();
            if s.state.is_running() {
                s.state = State::Stopped;
                s.pid = None;
            }
        }
    }

    fn build_argv(&self, model: &SavedModel, user: &[String]) -> Vec<String> {
        let mut argv = vec!["-m".to_string(), model.gguf_path.clone()];
        if let Some(p) = &model.mmproj_path {
            argv.push("--mmproj".to_string());
            argv.push(p.clone());
        }
        argv.push("--host".to_string());
        argv.push(self.config.host.clone());
        argv.push("--port".to_string());
        argv.push(self.config.port.to_string());
        argv.extend(user.iter().cloned());
        if model.pass_alias && !args::has_flag(user, &["-a", "--alias"]) {
            argv.push("--alias".to_string());
            argv.push(model.alias.clone());
        }
        argv
    }

    // ─── Model file discovery ────────────────────────────────────────────

    /// List `*.gguf` files under the configured `scan_dirs` (never anywhere
    /// else).
    pub async fn scan(&self) -> Vec<ScannedDir> {
        let saved: Vec<String> = self.store.list().await.into_iter().map(|m| m.gguf_path).collect();
        let dirs = self.config.scan_dirs.clone();
        tokio::task::spawn_blocking(move || dirs.iter().map(|d| scan_dir(d, &saved)).collect())
            .await
            .unwrap_or_default()
    }
}

fn run_model(m: &SavedModel) -> RunModel {
    RunModel {
        id: m.id,
        alias: m.alias.clone(),
        gguf_path: m.gguf_path.clone(),
        mmproj_path: m.mmproj_path.clone(),
    }
}

// ─── Background tasks ────────────────────────────────────────────────────────

fn spawn_reader<R>(reader: R, stream: &'static str, logs: Arc<LogBuffer>) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    let line = line.trim_end_matches(['\n', '\r']);
                    if !line.is_empty() {
                        logs.push(stream, line);
                    }
                }
            }
        }
    })
}

async fn wait_for_exit(
    mut child: tokio::process::Child,
    readers: Vec<JoinHandle<()>>,
    kill: Arc<Notify>,
    shared: Arc<StdMutex<Shared>>,
    logs: Arc<LogBuffer>,
    run_id: u64,
) {
    let (status, requested) = tokio::select! {
        s = child.wait() => (s, false),
        _ = kill.notified() => {
            let _ = child.kill().await;
            (child.wait().await, true)
        }
    };

    // Give the readers a moment to flush the final lines (they end when the
    // pipes close; a grandchild holding a pipe open must not hang us).
    for r in readers {
        let _ = tokio::time::timeout(Duration::from_secs(2), r).await;
    }

    let code = status.ok().and_then(|s| s.code());
    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
    if s.run_id != run_id {
        return;
    }
    let (state, unexpected, reason) = if let Some(failure) = s.failure.clone() {
        (State::Failed, true, failure)
    } else if requested {
        (State::Stopped, false, "stopped by request".to_string())
    } else {
        (
            State::Exited,
            true,
            format!("process exited on its own (exit code {})", code.map_or("unknown".to_string(), |c| c.to_string())),
        )
    };
    logs.push("gateway", format!("--- llama-server ended: {reason} ---"));
    s.state = state;
    s.pid = None;
    s.last_exit = Some(ExitInfo {
        code,
        at: Utc::now(),
        unexpected,
        reason,
        log_tail: logs.tail(EXIT_TAIL_LINES),
    });
}

async fn poll_readiness(
    http: reqwest::Client,
    base_url: String,
    timeout: Duration,
    kill: Arc<Notify>,
    shared: Arc<StdMutex<Shared>>,
    run_id: u64,
) {
    let deadline = Instant::now() + timeout;
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;
        {
            let s = shared.lock().unwrap_or_else(|e| e.into_inner());
            if s.run_id != run_id || !s.state.is_running() {
                return;
            }
        }
        match probe::health(&http, &base_url).await {
            Some(Health::Ok) => {
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                if s.run_id == run_id && s.state.is_running() {
                    s.state = State::Ready;
                    s.ready_at = Some(Utc::now());
                }
                return;
            }
            Some(Health::Loading) => {
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                if s.run_id == run_id && s.state == State::Starting {
                    s.state = State::Loading;
                }
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            {
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                if s.run_id == run_id {
                    s.failure = Some(format!(
                        "server did not become ready within {} s (start_timeout_secs)",
                        timeout.as_secs()
                    ));
                }
            }
            kill.notify_one();
            return;
        }
    }
}

fn scan_dir(dir: &str, saved: &[String]) -> ScannedDir {
    const MAX_DEPTH: usize = 4;
    const MAX_FILES: usize = 2000;

    let root = std::path::PathBuf::from(dir);
    let mut out = ScannedDir {
        dir: dir.to_string(),
        exists: root.is_dir(),
        models: vec![],
        mmproj_candidates: vec![],
        truncated: false,
    };
    if !out.exists {
        return out;
    }

    let mut stack = vec![(root, 0usize)];
    let mut seen = 0usize;
    while let Some((path, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else { continue };
        for entry in entries.flatten() {
            // `file_type` does not follow symlinks, so a symlinked directory
            // is neither descended into nor listed — no escaping the root.
            let Ok(ft) = entry.file_type() else { continue };
            let p = entry.path();
            if ft.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if !ft.is_file() || !name.to_ascii_lowercase().ends_with(".gguf") {
                continue;
            }
            if seen >= MAX_FILES {
                out.truncated = true;
                break;
            }
            seen += 1;
            let meta = entry.metadata().ok();
            let path_str = p.to_string_lossy().into_owned();
            // Saved paths are stored canonicalized; compare like with like.
            let canonical = canonical_string(&p).unwrap_or_else(|| path_str.clone());
            let file = ScannedFile {
                already_saved: saved.iter().any(|s| probe::same_path(s, &canonical)),
                path: path_str,
                file_name: name.clone(),
                size_bytes: meta.as_ref().map_or(0, |m| m.len()),
                modified: meta.and_then(|m| m.modified().ok()).map(DateTime::<Utc>::from),
            };
            if name.to_ascii_lowercase().contains("mmproj") {
                out.mmproj_candidates.push(file);
            } else {
                out.models.push(file);
            }
        }
    }
    out.models.sort_by(|a, b| a.path.to_ascii_lowercase().cmp(&b.path.to_ascii_lowercase()));
    out.mmproj_candidates.sort_by(|a, b| a.path.to_ascii_lowercase().cmp(&b.path.to_ascii_lowercase()));
    out
}
