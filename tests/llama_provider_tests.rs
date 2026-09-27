//! The `llama` provider and the llama-server manager acting as one: saved
//! models are listed, requests load the model they name, and a swap never
//! happens underneath another request. Uses `examples/fake_llama_server.rs`
//! (no real model or GPU) behind a recording stand-in for the HTTP adapter.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use uuid::Uuid;

use ai_gateway::config::LlamaServerConfig;
use ai_gateway::llama_server::store::ModelInput;
use ai_gateway::llama_server::{LlamaServerManager, State};
use ai_gateway::providers::llama_managed::ManagedLlamaProvider;
use ai_gateway::providers::{Provider, ProviderOutput};
use ai_gateway::types::{GatewayRequest, ModelCapabilities, ModelInfo, ProviderHealth};

// ─── Harness ─────────────────────────────────────────────────────────────────

fn fake_binary() -> String {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().unwrap().parent().unwrap();
        let path = profile_dir.join("examples").join(format!("fake_llama_server{}", std::env::consts::EXE_SUFFIX));
        if !path.exists() {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let status = std::process::Command::new(cargo)
                .args(["build", "--example", "fake_llama_server", "--offline"])
                .status()
                .expect("failed to run cargo to build the fake llama-server example");
            assert!(status.success(), "building examples/fake_llama_server.rs failed");
        }
        path.to_string_lossy().into_owned()
    })
    .clone()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("ai_gateway_llama_provider_{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self, name: &str) -> String {
        let p = self.0.join(name);
        std::fs::write(&p, b"gguf").unwrap();
        p.to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stands in for the HTTP adapter: records what it was asked to run and hands
/// back a stream that never yields, so a test controls exactly when the
/// "response" is finished by dropping it.
#[derive(Default)]
struct RecordingInner {
    executed: Mutex<Vec<String>>,
    caps: ModelCapabilities,
}

#[async_trait]
impl Provider for RecordingInner {
    fn name(&self) -> &str {
        "llama"
    }
    fn capabilities(&self) -> &ModelCapabilities {
        &self.caps
    }
    async fn models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        Ok(vec![])
    }
    async fn health(&self) -> ProviderHealth {
        ProviderHealth::Healthy
    }
    async fn execute(&self, _request: &GatewayRequest, model: &str) -> anyhow::Result<ProviderOutput> {
        self.executed.lock().unwrap().push(model.to_string());
        Ok(ProviderOutput::Stream(Box::pin(futures::stream::pending())))
    }
}

struct Env {
    scratch: Scratch,
    manager: Arc<LlamaServerManager>,
    inner: Arc<RecordingInner>,
    provider: ManagedLlamaProvider,
    port: u16,
}

async fn env_with(customize: impl FnOnce(&mut LlamaServerConfig)) -> Env {
    let scratch = Scratch::new();
    let port = free_port();
    let mut config = LlamaServerConfig {
        enabled: true,
        binary: Some(fake_binary()),
        port,
        state_file: scratch.0.join("models.json").to_string_lossy().into_owned(),
        start_timeout_secs: 30,
        swap_drain_timeout_secs: 30,
        ..Default::default()
    };
    customize(&mut config);
    let manager = Arc::new(LlamaServerManager::new(config).await.unwrap());
    let inner = Arc::new(RecordingInner::default());
    let provider = ManagedLlamaProvider::new(inner.clone(), manager.clone());
    Env { scratch, manager, inner, provider, port }
}

async fn env() -> Env {
    env_with(|_| {}).await
}

impl Env {
    async fn save(&self, alias: &str) -> Uuid {
        let gguf = self.scratch.file(&format!("{alias}.gguf"));
        self.manager
            .store()
            .create(ModelInput { alias: alias.into(), gguf_path: gguf, mmproj_path: None, args: None, pass_alias: None, notes: None })
            .await
            .unwrap()
            .id
    }
}

fn request(model: &str) -> GatewayRequest {
    GatewayRequest { model: model.to_string(), ..Default::default() }
}

async fn wait_for_state(m: &LlamaServerManager, want: State) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.state() != want {
        assert!(Instant::now() < deadline, "timed out waiting for {want:?}; still {:?}", m.state());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn saved_models_are_listed_even_while_nothing_is_loaded() {
    let e = env().await;
    e.save("gemma").await;
    let gguf = e.scratch.file("vl.gguf");
    let mmproj = e.scratch.file("vl-mmproj.gguf");
    e.manager
        .store()
        .create(ModelInput { alias: "qwen-vl".into(), gguf_path: gguf, mmproj_path: Some(mmproj), args: None, pass_alias: None, notes: None })
        .await
        .unwrap();

    let models = e.provider.models().await.unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids.len(), 2, "{ids:?}");
    assert!(ids.contains(&"gemma") && ids.contains(&"qwen-vl"), "{ids:?}");

    let caps = |id: &str| models.iter().find(|m| m.id == id).unwrap().capabilities.clone().unwrap();
    assert!(caps("qwen-vl").vision, "an mmproj means the model takes images");
    assert!(!caps("gemma").vision);
    assert!(models.iter().all(|m| m.providers == ["llama"]));
}

#[tokio::test]
async fn health_is_degraded_when_cold_but_loadable_and_unhealthy_when_there_is_nothing_to_load() {
    let e = env().await;
    assert_eq!(e.provider.health().await, ProviderHealth::Unhealthy, "nothing saved, nothing running");

    let id = e.save("gemma").await;
    assert_eq!(
        e.provider.health().await,
        ProviderHealth::Degraded,
        "routing skips Unhealthy providers, and this one can load a model on demand"
    );

    e.manager.ensure_loaded(id).await.unwrap();
    assert_eq!(e.provider.health().await, ProviderHealth::Healthy, "once loaded, health is the live server's");
    e.manager.stop().await;
}

#[tokio::test]
async fn a_request_loads_the_model_it_names_then_swaps_when_another_is_requested() {
    let e = env().await;
    let a = e.save("alpha").await;
    let b = e.save("beta").await;
    assert_eq!(e.manager.state(), State::Stopped);

    // Cold start: nothing running, the request loads it.
    drop(e.provider.execute(&request("llama/alpha"), "alpha").await.ok().unwrap());
    assert_eq!(e.manager.state(), State::Ready);
    assert_eq!(e.manager.running_model_id(), Some(a));

    // Same model again: no restart.
    let pid_before = e.manager.status().await.pid;
    drop(e.provider.execute(&request("llama/ALPHA"), "ALPHA").await.ok().unwrap());
    assert_eq!(e.manager.status().await.pid, pid_before, "alias match is case-insensitive and must not restart");

    // Different model: swapped, then served.
    drop(e.provider.execute(&request("llama/beta"), "beta").await.ok().unwrap());
    assert_eq!(e.manager.running_model_id(), Some(b));
    assert_ne!(e.manager.status().await.pid, pid_before, "a different process now");

    assert_eq!(*e.inner.executed.lock().unwrap(), ["alpha", "ALPHA", "beta"]);
    e.manager.stop().await;
}

#[tokio::test]
async fn a_saved_models_gguf_path_selects_it_and_an_unsaved_path_is_refused() {
    let e = env().await;
    let id = e.save("gemma").await;
    let path = e.manager.store().get(id).await.unwrap().gguf_path;

    drop(e.provider.execute(&request("x"), &path).await.ok().unwrap());
    assert_eq!(e.manager.running_model_id(), Some(id));

    let stray = e.scratch.file("stray.gguf");
    let err = e.provider.execute(&request("x"), &stray).await.err().unwrap();
    assert!(err.to_string().contains("not a saved model"), "{err}");
    assert_eq!(e.manager.running_model_id(), Some(id), "a refused request must not disturb what is loaded");
    e.manager.stop().await;
}

#[tokio::test]
async fn names_that_are_not_saved_models_pass_through_without_loading_anything() {
    // e.g. the `qwen3.5-9b` in a config alias: whatever is loaded serves it.
    let e = env().await;
    e.save("gemma").await;
    drop(e.provider.execute(&request("coding-fast"), "qwen3.5-9b").await.ok().unwrap());
    assert_eq!(e.manager.state(), State::Stopped, "no saved model was named, so nothing may be started");
    assert_eq!(*e.inner.executed.lock().unwrap(), ["qwen3.5-9b"]);
}

#[tokio::test]
async fn auto_swap_off_never_touches_the_server() {
    let e = env_with(|c| c.auto_swap = false).await;
    e.save("gemma").await;
    drop(e.provider.execute(&request("gemma"), "gemma").await.ok().unwrap());
    assert_eq!(e.manager.state(), State::Stopped);
    assert_eq!(
        e.provider.health().await,
        ProviderHealth::Unhealthy,
        "with auto_swap off a stopped server is genuinely unavailable"
    );
}

#[tokio::test]
async fn a_swap_waits_for_in_flight_requests_and_gives_up_if_they_do_not_finish() {
    let e = env_with(|c| c.swap_drain_timeout_secs = 1).await;
    let a = e.save("alpha").await;
    e.save("beta").await;

    // Alpha's response is still "being produced" (never polled, never dropped).
    let in_flight = e.provider.execute(&request("alpha"), "alpha").await.ok().unwrap();
    assert_eq!(e.manager.running_model_id(), Some(a));

    let started = Instant::now();
    let err = e.provider.execute(&request("beta"), "beta").await.err().unwrap();
    assert!(err.to_string().contains("busy"), "{err}");
    assert!(started.elapsed() >= Duration::from_millis(900), "it must actually wait for the drain timeout");
    assert_eq!(e.manager.running_model_id(), Some(a), "the model in use must not be killed underneath its request");
    assert_eq!(e.manager.state(), State::Ready);

    // Once the response is finished, the swap goes through.
    drop(in_flight);
    drop(e.provider.execute(&request("beta"), "beta").await.ok().unwrap());
    assert_ne!(e.manager.running_model_id(), Some(a));
    e.manager.stop().await;
}

#[tokio::test]
async fn a_swap_that_fails_is_an_error_not_a_hang() {
    let e = env().await;
    let id = e.save("broken").await;
    // The fake exits immediately for this run, so the model never becomes ready.
    e.manager
        .store()
        .update(
            id,
            ModelInput {
                alias: "broken".into(),
                gguf_path: e.manager.store().get(id).await.unwrap().gguf_path,
                mmproj_path: None,
                args: Some("--fake-exit-immediately".into()),
                pass_alias: None,
                notes: None,
            },
        )
        .await
        .unwrap();

    let err = e.provider.execute(&request("broken"), "broken").await.err().unwrap();
    assert!(err.to_string().contains("did not become ready"), "{err}");
    wait_for_state(&e.manager, State::Exited).await;
    assert!(e.inner.executed.lock().unwrap().is_empty(), "the request must not be dispatched to a dead server");
}

#[tokio::test]
async fn a_server_the_gateway_did_not_start_is_left_alone() {
    let e = env().await;
    let id = e.save("gemma").await;
    let gguf = e.manager.store().get(id).await.unwrap().gguf_path;

    let mut external = std::process::Command::new(fake_binary())
        .args(["-m", &gguf, "--host", "127.0.0.1", "--port", &e.port.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !e.manager.external_server_present().await {
        assert!(Instant::now() < deadline, "external fake never came up");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Cannot swap what we do not own: the request just goes to it.
    drop(e.provider.execute(&request("gemma"), "gemma").await.ok().unwrap());
    assert_eq!(e.manager.state(), State::Stopped, "the gateway must not try to start over someone else's server");
    assert_eq!(e.provider.health().await, ProviderHealth::Healthy, "health is the external server's own");
    assert_eq!(*e.inner.executed.lock().unwrap(), ["gemma"]);

    let _ = external.kill();
    let _ = external.wait();
}
