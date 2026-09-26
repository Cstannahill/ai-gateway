//! A stand-in for `llama-server`, used only by `tests/llama_server_tests.rs`.
//!
//! It accepts the flags the manager passes (`-m`, `--mmproj`, `--host`,
//! `--port`, `--alias`, `-np`, ignoring anything else) and serves just enough
//! HTTP for the manager: `/health` (503 while "loading", then 200), `/props`
//! and `/v1/models`. `/props` also echoes the full argv as `fake_argv`, so a
//! test can assert exactly what the manager passed.
//!
//! Test-only behavior switches (passed like any other user argument):
//!   --fake-load-ms N          report "loading" (503) for the first N ms
//!   --fake-crash-after-ms N   exit with code 2 after N ms
//!   --fake-exit-immediately   print an error and exit with code 3 at once
//!   --fake-log-lines N        print N extra log lines on startup

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn json_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

fn respond(mut stream: TcpStream, status: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let value_of = |flag: &str| -> Option<String> {
        argv.iter().position(|a| a == flag).and_then(|i| argv.get(i + 1)).cloned()
    };

    let host = value_of("--host").unwrap_or_else(|| "127.0.0.1".into());
    let port = value_of("--port").unwrap_or_else(|| "8080".into());
    let model = value_of("-m").unwrap_or_default();
    let mmproj = value_of("--mmproj");
    let alias = value_of("--alias");
    let slots: u64 = value_of("-np").and_then(|v| v.parse().ok()).unwrap_or(1);
    let load_ms: u64 = value_of("--fake-load-ms").and_then(|v| v.parse().ok()).unwrap_or(0);
    let crash_after_ms: Option<u64> = value_of("--fake-crash-after-ms").and_then(|v| v.parse().ok());
    let log_lines: u64 = value_of("--fake-log-lines").and_then(|v| v.parse().ok()).unwrap_or(0);

    eprintln!("fake llama-server: starting with args: {}", argv.join(" "));
    for i in 0..log_lines {
        eprintln!("fake log line {i}");
    }

    if argv.iter().any(|a| a == "--fake-exit-immediately") {
        eprintln!("error: failed to load model '{model}'");
        std::process::exit(3);
    }

    let listener = match TcpListener::bind(format!("{host}:{port}")) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("fake llama-server: cannot bind {host}:{port}: {e}");
            std::process::exit(1);
        }
    };
    println!("fake llama-server: listening on {host}:{port}, loading {model}");

    if let Some(ms) = crash_after_ms {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            eprintln!("fake llama-server: simulated crash");
            std::process::exit(2);
        });
    }

    let started = Instant::now();
    let argv_json = argv.iter().map(|a| format!("\"{}\"", json_escape(a))).collect::<Vec<_>>().join(",");
    let model_id = alias.clone().unwrap_or_else(|| {
        std::path::Path::new(&model)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    for stream in listener.incoming().flatten() {
        let mut stream = stream;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
        let loaded = started.elapsed() >= Duration::from_millis(load_ms);

        match path.as_str() {
            "/health" if loaded => respond(stream, "200 OK", r#"{"status":"ok"}"#),
            "/health" => respond(stream, "503 Service Unavailable", r#"{"error":{"code":503,"message":"Loading model"}}"#),
            "/props" if loaded => respond(
                stream,
                "200 OK",
                &format!(
                    r#"{{"model_path":"{}","total_slots":{slots},"build_info":"fake-b0","modalities":{{"vision":{},"audio":false}},"default_generation_settings":{{"n_ctx":4096}},"fake_argv":[{argv_json}]}}"#,
                    json_escape(&model),
                    mmproj.is_some()
                ),
            ),
            "/v1/models" if loaded => respond(
                stream,
                "200 OK",
                &format!(
                    r#"{{"object":"list","data":[{{"id":"{}","meta":{{"n_ctx_train":32768,"n_params":1000,"size":2048}}}}]}}"#,
                    json_escape(&model_id)
                ),
            ),
            _ => respond(stream, "404 Not Found", r#"{"error":"not found"}"#),
        }
    }
}
