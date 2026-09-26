//! Manager tests against `examples/fake_llama_server.rs`, a stand-in that
//! speaks just enough of llama-server's HTTP surface. No real model or GPU is
//! needed. (`tests/live_llama_server_test.rs` covers the real binary.)

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ai_gateway::config::LlamaServerConfig;
use ai_gateway::llama_server::store::ModelInput;
use ai_gateway::llama_server::{LlamaError, LlamaServerManager, State};
use uuid::Uuid;

// HTTP-level tests live in a submodule so they can reuse this harness.
#[path = "llama_server_http/mod.rs"]
mod http;

// ─── Harness ─────────────────────────────────────────────────────────────────

/// Path to the fake server, building the example first if `cargo test` was
/// invoked in a way that skipped it (e.g. `--test llama_server_tests`).
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
        assert!(path.exists(), "fake llama-server not found at {}", path.display());
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
        let dir = std::env::temp_dir().join(format!("ai_gateway_llama_{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self, name: &str) -> String {
        let p = self.0.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, b"gguf").unwrap();
        p.to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Env {
    scratch: Scratch,
    manager: LlamaServerManager,
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
        ..Default::default()
    };
    customize(&mut config);
    let manager = LlamaServerManager::new(config).await.unwrap();
    Env { scratch, manager, port }
}

async fn env() -> Env {
    env_with(|_| {}).await
}

fn input(alias: &str, gguf: &str) -> ModelInput {
    ModelInput { alias: alias.into(), gguf_path: gguf.into(), mmproj_path: None, args: None, pass_alias: None, notes: None }
}

async fn wait_for_state(m: &LlamaServerManager, want: State, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let s = m.state();
        if s == want {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {want:?}; still {s:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn props(port: u16) -> serde_json::Value {
    reqwest::get(format!("http://127.0.0.1:{port}/props")).await.unwrap().json().await.unwrap()
}

async fn port_is_free(port: u16) -> bool {
    reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/health"))
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .is_err()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn start_goes_through_loading_to_ready_and_passes_the_right_argv() {
    let e = env().await;
    let gguf = e.scratch.file("qwen.gguf");
    let mmproj = e.scratch.file("mmproj-qwen.gguf");
    let mut inp = input("qwen-vl", &gguf);
    inp.mmproj_path = Some(mmproj.clone());
    inp.args = Some("-ngl all -fa on -np 2 --fake-load-ms 700".into());
    let saved = e.manager.store().create(inp).await.unwrap();

    let started = e.manager.start(saved.id, None, false).await.unwrap();
    assert!(started.managed);
    assert!(matches!(started.state, State::Starting | State::Loading), "{:?}", started.state);
    assert!(started.pid.is_some());

    wait_for_state(&e.manager, State::Loading, Duration::from_secs(10)).await;
    wait_for_state(&e.manager, State::Ready, Duration::from_secs(10)).await;

    let status = e.manager.status().await;
    assert_eq!(status.state, State::Ready);
    assert!(status.managed && !status.external);
    assert_eq!(status.model.as_ref().unwrap().alias, "qwen-vl");
    let server = status.server.as_ref().expect("server should be reachable");
    assert_eq!(server.vision, Some(true), "an mmproj was passed, so vision is on");
    assert_eq!(server.n_ctx, Some(4096));
    assert_eq!(server.total_slots, Some(2), "-np 2 reached the process");
    assert_eq!(status.matched_model.as_ref().unwrap().alias, "qwen-vl");

    // What the manager says it passed == what the process actually received.
    let argv = &status.args.as_ref().unwrap().argv;
    assert_eq!(&argv[..2], ["-m".to_string(), saved.gguf_path.clone()]);
    assert!(argv.windows(2).any(|w| w == ["--mmproj".to_string(), saved.mmproj_path.clone().unwrap()]));
    assert!(argv.windows(2).any(|w| w == ["--port".to_string(), e.port.to_string()]));
    assert!(argv.windows(2).any(|w| w == ["--alias".to_string(), "qwen-vl".to_string()]), "pass_alias defaults on");
    assert!(argv.windows(2).any(|w| w == ["-ngl".to_string(), "all".to_string()]));
    let seen = props(e.port).await;
    let seen_argv: Vec<String> = seen["fake_argv"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(&seen_argv, argv);

    e.manager.shutdown().await;
}

#[tokio::test]
async fn argument_precedence_is_override_then_model_then_defaults() {
    let e = env().await;
    let gguf = e.scratch.file("a.gguf");
    e.manager.store().set_default_args("-ngl 1".into()).await.unwrap();

    let no_args = e.manager.store().create(input("default-args", &gguf)).await.unwrap();
    let s = e.manager.start(no_args.id, None, false).await.unwrap();
    assert_eq!(s.args.unwrap().effective, "-ngl 1", "no model args -> store defaults");
    e.manager.stop().await;

    let mut inp = input("model-args", &e.scratch.file("b.gguf"));
    inp.args = Some("-ngl 2".into());
    let with_args = e.manager.store().create(inp).await.unwrap();
    let s = e.manager.start(with_args.id, None, false).await.unwrap();
    assert_eq!(s.args.unwrap().effective, "-ngl 2", "model args beat defaults");
    e.manager.stop().await;

    let s = e.manager.start(with_args.id, Some("-ngl 3 -np 4".into()), false).await.unwrap();
    assert_eq!(s.args.unwrap().effective, "-ngl 3 -np 4", "request override beats model args");
    e.manager.shutdown().await;
}

#[tokio::test]
async fn an_explicit_alias_flag_suppresses_the_automatic_alias() {
    let e = env().await;
    let m = e.manager.store().create(input("auto", &e.scratch.file("a.gguf"))).await.unwrap();
    let s = e.manager.start(m.id, Some("--alias custom".into()), false).await.unwrap();
    let argv = s.args.unwrap().argv;
    assert_eq!(argv.iter().filter(|a| *a == "--alias").count(), 1, "{argv:?}");
    assert!(argv.windows(2).any(|w| w == ["--alias", "custom"]));
    e.manager.shutdown().await;
}

#[tokio::test]
async fn starting_while_running_conflicts_unless_replace_switches_models() {
    let e = env().await;
    let a = e.manager.store().create(input("model-a", &e.scratch.file("a.gguf"))).await.unwrap();
    let b = e.manager.store().create(input("model-b", &e.scratch.file("b.gguf"))).await.unwrap();

    e.manager.start(a.id, None, false).await.unwrap();
    wait_for_state(&e.manager, State::Ready, Duration::from_secs(10)).await;

    let err = e.manager.start(b.id, None, false).await.unwrap_err();
    assert_eq!(err.code(), "already_running");
    assert_eq!(e.manager.status().await.model.unwrap().alias, "model-a", "a rejected start changes nothing");

    let switched = e.manager.start(b.id, None, true).await.unwrap();
    assert_eq!(switched.model.as_ref().unwrap().alias, "model-b");
    wait_for_state(&e.manager, State::Ready, Duration::from_secs(10)).await;
    let status = e.manager.status().await;
    assert_eq!(status.matched_model.unwrap().alias, "model-b");
    assert!(props(e.port).await["model_path"].as_str().unwrap().ends_with("b.gguf"));

    e.manager.shutdown().await;
}

#[tokio::test]
async fn stop_terminates_the_process_and_is_idempotent() {
    let e = env().await;
    let m = e.manager.store().create(input("m", &e.scratch.file("a.gguf"))).await.unwrap();
    e.manager.start(m.id, None, false).await.unwrap();
    wait_for_state(&e.manager, State::Ready, Duration::from_secs(10)).await;

    let stopped = e.manager.stop().await;
    assert_eq!(stopped.state, State::Stopped);
    assert!(!stopped.managed && stopped.pid.is_none());
    let exit = stopped.last_exit.expect("a stop is recorded");
    assert!(!exit.unexpected, "a requested stop is not a crash");
    assert!(port_is_free(e.port).await, "the process must actually be gone");

    assert_eq!(e.manager.stop().await.state, State::Stopped, "stopping again is a no-op");
}

#[tokio::test]
async fn shutdown_kills_a_running_server() {
    let e = env().await;
    let m = e.manager.store().create(input("m", &e.scratch.file("a.gguf"))).await.unwrap();
    e.manager.start(m.id, None, false).await.unwrap();
    wait_for_state(&e.manager, State::Ready, Duration::from_secs(10)).await;
    e.manager.shutdown().await;
    assert!(port_is_free(e.port).await);
}

#[tokio::test]
async fn an_unexpected_exit_is_reported_with_the_log_tail() {
    let e = env().await;
    let mut inp = input("crashy", &e.scratch.file("a.gguf"));
    inp.args = Some("--fake-crash-after-ms 400".into());
    let m = e.manager.store().create(inp).await.unwrap();
    e.manager.start(m.id, None, false).await.unwrap();

    wait_for_state(&e.manager, State::Exited, Duration::from_secs(10)).await;
    let status = e.manager.status().await;
    assert!(!status.managed);
    let exit = status.last_exit.unwrap();
    assert!(exit.unexpected);
    assert_eq!(exit.code, Some(2));
    assert!(exit.log_tail.iter().any(|l| l.text.contains("simulated crash")), "{:?}", exit.log_tail);
}

#[tokio::test]
async fn a_model_that_fails_to_load_exits_with_its_error_in_the_log() {
    let e = env().await;
    let mut inp = input("broken", &e.scratch.file("a.gguf"));
    inp.args = Some("--fake-exit-immediately".into());
    let m = e.manager.store().create(inp).await.unwrap();
    e.manager.start(m.id, None, false).await.unwrap();

    wait_for_state(&e.manager, State::Exited, Duration::from_secs(10)).await;
    let exit = e.manager.status().await.last_exit.unwrap();
    assert_eq!(exit.code, Some(3));
    assert!(exit.log_tail.iter().any(|l| l.text.contains("failed to load model")));
}

#[tokio::test]
async fn a_server_that_never_becomes_ready_times_out_and_is_killed() {
    let e = env_with(|c| c.start_timeout_secs = 1).await;
    let mut inp = input("slow", &e.scratch.file("a.gguf"));
    inp.args = Some("--fake-load-ms 60000".into());
    let m = e.manager.store().create(inp).await.unwrap();
    e.manager.start(m.id, None, false).await.unwrap();

    wait_for_state(&e.manager, State::Failed, Duration::from_secs(15)).await;
    let status = e.manager.status().await;
    assert!(status.failure.unwrap().contains("did not become ready"));
    assert!(port_is_free(e.port).await, "the timed-out process must be killed");
}

#[tokio::test]
async fn an_unspawnable_binary_fails_the_start_with_a_clear_error() {
    let e = env_with(|c| c.binary = Some("/definitely/not/a/binary".into())).await;
    let m = e.manager.store().create(input("m", &e.scratch.file("a.gguf"))).await.unwrap();
    let err = e.manager.start(m.id, None, false).await.unwrap_err();
    assert_eq!(err.code(), "spawn_failed");
    let status = e.manager.status().await;
    assert_eq!(status.state, State::Failed);
    assert!(status.failure.unwrap().contains("could not start"));
}

#[tokio::test]
async fn reserved_flags_and_missing_files_are_rejected_before_anything_is_spawned() {
    let e = env().await;
    let gguf = e.scratch.file("a.gguf");
    let m = e.manager.store().create(input("m", &gguf)).await.unwrap();

    let err = e.manager.start(m.id, Some("--port 9999".into()), false).await.unwrap_err();
    assert_eq!(err.code(), "reserved_flag");
    assert_eq!(e.manager.state(), State::Stopped);

    std::fs::remove_file(&gguf).unwrap();
    let err = e.manager.start(m.id, None, false).await.unwrap_err();
    assert_eq!(err.code(), "path_not_found", "a file deleted after saving is caught at start");

    let err = e.manager.start(Uuid::now_v7(), None, false).await.unwrap_err();
    assert!(matches!(err, LlamaError::NotFound(_)));
    assert!(port_is_free(e.port).await);
}

#[tokio::test]
async fn an_external_server_is_reported_and_blocks_a_start() {
    let e = env().await;
    let gguf = e.scratch.file("external.gguf");
    let saved = e.manager.store().create(input("ext", &gguf)).await.unwrap();

    // Someone else's server on our port.
    let mut external = std::process::Command::new(fake_binary())
        .args(["-m", &saved.gguf_path, "--host", "127.0.0.1", "--port", &e.port.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while port_is_free(e.port).await {
        assert!(Instant::now() < deadline, "external fake never came up");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let status = e.manager.status().await;
    assert!(status.external && !status.managed);
    assert_eq!(status.state, State::Stopped);
    assert_eq!(status.matched_model.unwrap().alias, "ext", "the loaded file is matched to the saved model");

    let err = e.manager.start(saved.id, None, false).await.unwrap_err();
    assert_eq!(err.code(), "port_in_use");

    let _ = external.kill();
    let _ = external.wait();
}

#[tokio::test]
async fn restart_reuses_the_last_model_and_effective_args() {
    let e = env().await;
    let m = e.manager.store().create(input("m", &e.scratch.file("a.gguf"))).await.unwrap();
    assert_eq!(e.manager.restart().await.unwrap_err().code(), "nothing_to_restart");

    let first = e.manager.start(m.id, Some("-np 3".into()), false).await.unwrap();
    wait_for_state(&e.manager, State::Ready, Duration::from_secs(10)).await;
    let second = e.manager.restart().await.unwrap();
    assert_ne!(first.pid, second.pid, "restart launches a new process");
    assert_eq!(second.args.unwrap().effective, "-np 3");
    wait_for_state(&e.manager, State::Ready, Duration::from_secs(10)).await;
    e.manager.shutdown().await;
}

#[tokio::test]
async fn logs_capture_process_output_and_support_cursor_tailing() {
    let e = env().await;
    let mut inp = input("m", &e.scratch.file("a.gguf"));
    inp.args = Some("--fake-log-lines 5".into());
    let m = e.manager.store().create(inp).await.unwrap();
    e.manager.start(m.id, None, false).await.unwrap();
    wait_for_state(&e.manager, State::Ready, Duration::from_secs(10)).await;

    let page = e.manager.logs(None, 1000);
    let text: Vec<&str> = page.lines.iter().map(|l| l.text.as_str()).collect();
    assert!(text.iter().any(|t| t.contains("starting 'm'")), "gateway marker: {text:?}");
    assert!(text.iter().any(|t| t.contains("fake log line 4")), "stderr captured: {text:?}");
    assert!(page.lines.iter().any(|l| l.stream == "stderr") && page.lines.iter().any(|l| l.stream == "stdout"));

    let after = e.manager.logs(Some(page.next_cursor), 1000);
    assert!(after.lines.is_empty(), "nothing new since the cursor");

    e.manager.stop().await;
    let ended = e.manager.logs(Some(page.next_cursor), 1000);
    assert!(ended.lines.iter().any(|l| l.text.contains("ended")), "the stop is logged after the cursor");
}

#[tokio::test]
async fn scan_lists_only_configured_dirs_and_flags_saved_and_mmproj_files() {
    let scan_root = Scratch::new();
    let saved_file = scan_root.file("keep/llama-8b.gguf");
    scan_root.file("keep/mmproj-llama.gguf");
    scan_root.file("deep/a/b/c/d/e/too-deep.gguf");
    scan_root.file("readme.txt");
    let root = scan_root.0.to_string_lossy().into_owned();
    let missing = scan_root.0.join("nope").to_string_lossy().into_owned();

    let e = env_with(|c| c.scan_dirs = vec![root.clone(), missing.clone()]).await;
    e.manager.store().create(input("llama", &saved_file)).await.unwrap();

    let result = e.manager.scan().await;
    assert_eq!(result.len(), 2);
    assert!(result[0].exists);
    let names: Vec<_> = result[0].models.iter().map(|f| f.file_name.as_str()).collect();
    assert_eq!(names, ["llama-8b.gguf"], "txt ignored, too-deep ignored, mmproj split out");
    assert!(result[0].models[0].already_saved);
    assert_eq!(result[0].mmproj_candidates.len(), 1);
    assert!(!result[0].mmproj_candidates[0].already_saved);
    assert!(!result[1].exists, "a configured-but-missing dir is reported, not an error");
}
