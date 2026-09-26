//! Opt-in test of the llama-server manager against the REAL llama-server
//! binary. A silent no-op (prints SKIP) unless both variables are set:
//!
//!   LIVE_LLAMA_SERVER_BINARY   path to llama-server(.exe) — not a .cmd shim
//!   LIVE_LLAMA_MODEL_GGUF      path to a (small!) .gguf model
//!
//! Optional:
//!   LIVE_LLAMA_MMPROJ          matching mmproj .gguf (also checks vision is reported)
//!   LIVE_LLAMA_ARGS            extra arguments (default: "-ngl 0 -c 512", CPU only)
//!
//! Example (PowerShell):
//!   $env:LIVE_LLAMA_SERVER_BINARY='S:\llama.cpp\build\bin\Release\llama-server.exe'
//!   $env:LIVE_LLAMA_MODEL_GGUF='S:\Code\Models\MagicPrompt-SD\Gpt2-124M-Q8_0.gguf'
//!   cargo test --test live_llama_server_test -- --nocapture

use std::time::{Duration, Instant};

use ai_gateway::config::LlamaServerConfig;
use ai_gateway::llama_server::store::ModelInput;
use ai_gateway::llama_server::{LlamaServerManager, State};

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::test]
async fn live_start_inspect_and_stop_a_real_llama_server() {
    let (Some(binary), Some(gguf)) = (env_var("LIVE_LLAMA_SERVER_BINARY"), env_var("LIVE_LLAMA_MODEL_GGUF")) else {
        eprintln!("SKIP live_llama_server: LIVE_LLAMA_SERVER_BINARY / LIVE_LLAMA_MODEL_GGUF not set");
        return;
    };
    let mmproj = env_var("LIVE_LLAMA_MMPROJ");
    let args = env_var("LIVE_LLAMA_ARGS").unwrap_or_else(|| "-ngl 0 -c 512".to_string());

    let dir = std::env::temp_dir().join(format!("ai_gateway_live_llama_{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();

    let manager = LlamaServerManager::new(LlamaServerConfig {
        enabled: true,
        binary: Some(binary),
        port,
        state_file: dir.join("models.json").to_string_lossy().into_owned(),
        start_timeout_secs: 240,
        ..Default::default()
    })
    .await
    .unwrap();

    let saved = manager
        .store()
        .create(ModelInput {
            alias: "live-test".into(),
            gguf_path: gguf,
            mmproj_path: mmproj.clone(),
            args: Some(args),
            pass_alias: None,
            notes: None,
        })
        .await
        .expect("saving the model");

    let started = manager.start(saved.id, None, false).await.expect("start");
    eprintln!("started pid={:?} argv={:?}", started.pid, started.args.as_ref().unwrap().argv);

    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        match manager.state() {
            State::Ready => break,
            State::Exited | State::Failed => {
                let s = manager.status().await;
                let tail: Vec<_> = manager.logs(None, 2000).lines.into_iter().rev().take(25).collect();
                panic!("llama-server did not become ready: {:?} / {:?}\nlast log lines (newest first): {tail:#?}", s.state, s.failure);
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "timed out waiting for ready");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let status = manager.status().await;
    let server = status.server.as_ref().expect("a ready server must answer probes");
    eprintln!("server info: {server:#?}");
    eprintln!("matched: {:?}", status.matched_model);
    assert!(server.model_path.is_some(), "/props should report model_path on this build");
    assert_eq!(status.matched_model.as_ref().map(|m| m.alias.as_str()), Some("live-test"));
    assert!(server.n_ctx.is_some());
    if mmproj.is_some() {
        assert_eq!(server.vision, Some(true), "an mmproj was loaded, so vision should be reported");
    }

    // The server really works: one tiny completion.
    let resp: serde_json::Value = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/v1/completions"))
        .json(&serde_json::json!({"model": "live-test", "prompt": "Hello", "max_tokens": 4}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    eprintln!("completion: {resp}");
    assert!(resp["choices"][0].get("text").is_some(), "{resp}");

    let logs = manager.logs(None, 2000);
    assert!(!logs.lines.is_empty(), "the server's output must be captured");

    let stopped = manager.stop().await;
    assert_eq!(stopped.state, State::Stopped);
    let gone = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/health"))
        .timeout(Duration::from_secs(1))
        .send()
        .await
        .is_err();
    assert!(gone, "the real llama-server process must be gone after stop");

    let _ = std::fs::remove_dir_all(dir);
}
