use std::sync::Arc;

use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use ai_gateway::cli;
use ai_gateway::config::GatewayConfig;
use ai_gateway::providers::{self, Provider};
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{self, AppState};
use ai_gateway::telemetry::TelemetryPipeline;

/// Synchronous on purpose: the command line and the `.env` file are handled
/// before the multi-threaded runtime exists, because mutating the process
/// environment is only sound while no other thread is running.
fn main() -> anyhow::Result<()> {
    // ─── Parse the command line (before logging, so --help output is clean) ─
    let (config_path, env_file) = match cli::parse(std::env::args().skip(1)) {
        Ok(cli::Action::Run { config_path, env_file }) => (config_path, env_file),
        Ok(cli::Action::Help) => {
            print!("{}", cli::HELP);
            return Ok(());
        }
        Ok(cli::Action::Version) => {
            println!("{}", cli::version_line());
            return Ok(());
        }
        Err(message) => {
            eprintln!("error: {message}\n\nRun 'ai-gateway --help' for usage.");
            std::process::exit(2);
        }
    };

    // ─── Load .env (before logging, so RUST_LOG in it takes effect) ──────
    let loaded_env_file = match cli::load_env_file(env_file.as_deref()) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(config_path, loaded_env_file))
}

async fn run(
    config_path: Option<String>,
    loaded_env_file: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    // ─── Initialize logging ──────────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("ai_gateway=info,tower_http=info")),
        )
        .with_target(true)
        .with_thread_ids(false)
        .init();

    tracing::info!("AI Gateway v{}", env!("CARGO_PKG_VERSION"));
    if let Some(path) = &loaded_env_file {
        tracing::info!(file = %path.display(), "Loaded environment variables from file");
    }

    // ─── Load configuration ──────────────────────────────────────────────
    let config = GatewayConfig::load(config_path.as_deref())?;

    tracing::info!(
        host = %config.server.host,
        port = config.server.port,
        providers = config.providers.len(),
        models = config.models.len(),
        "Configuration loaded"
    );

    // ─── Initialize telemetry pipeline ───────────────────────────────────
    let db_url = config.database_url();
    let telemetry = TelemetryPipeline::new(
        &config.telemetry,
        db_url.as_deref(),
        config.pricing.clone(),
    )
    .await?;

    tracing::info!(
        enabled = config.telemetry.enabled,
        database = db_url.is_some(),
        "Telemetry pipeline initialized"
    );

    // ─── Initialize provider registry ────────────────────────────────────
    let mut provider_registry = ProviderRegistry::new();

    for (name, provider_config) in &config.providers {
        let mut pconfig = provider_config.clone();
        if pconfig.name.is_empty() {
            pconfig.name = name.clone();
        }

        let adapter: Arc<dyn Provider> = providers::build_adapter(&pconfig)?;

        tracing::info!(
            provider = %name,
            r#type = ?pconfig.r#type,
            base_url = %pconfig.base_url,
            "Registered provider"
        );

        provider_registry.register(pconfig, adapter);
    }

    // ─── Initialize model registry ───────────────────────────────────────
    let model_registry = ModelRegistry::new(config.models.clone());
    tracing::info!(
        models = config.models.len(),
        "Model registry initialized"
    );

    // ─── Initialize routing engine ───────────────────────────────────────
    let router = RoutingEngine::new(config.routing.default_policy.clone())
        .with_pricing(config.pricing.clone());

    // ─── Initialize the analytics read-pool ──────────────────────────────
    // A small, separate pool from the telemetry writer's own — analytics
    // queries are occasional, ad-hoc reads and shouldn't compete with (or
    // be coupled to the lifecycle of) the write-side batch pipeline.
    let analytics_pool = match &db_url {
        Some(url) => match sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(url)
            .await
        {
            Ok(pool) => Some(pool),
            Err(e) => {
                tracing::warn!(error = %e, "Analytics endpoints unavailable: could not connect to database");
                None
            }
        },
        None => None,
    };

    // ─── Build application state ─────────────────────────────────────────
    let response_cache = config.caching.enabled.then(|| {
        tracing::info!(
            ttl_secs = config.caching.ttl_secs,
            max_entries = config.caching.max_entries,
            "Response caching enabled (non-streaming requests only)"
        );
        Arc::new(ai_gateway::cache::ResponseCache::new(
            std::time::Duration::from_secs(config.caching.ttl_secs),
            config.caching.max_entries,
        ))
    });

    let llama_server = if config.llama_server.enabled {
        let manager = ai_gateway::llama_server::LlamaServerManager::new(config.llama_server.clone())
            .await
            .map_err(|e| anyhow::anyhow!("llama_server: {e}"))?;
        tracing::info!(
            binary = config.llama_server.binary.as_deref().unwrap_or(""),
            port = config.llama_server.port,
            "llama-server management enabled (/llama-server/*)"
        );
        Some(Arc::new(manager))
    } else {
        None
    };

    let state = Arc::new(AppState {
        config: config.clone(),
        model_registry,
        provider_registry,
        router,
        telemetry_tx: telemetry.sender(),
        start_time: std::time::Instant::now(),
        total_gateway_requests: std::sync::atomic::AtomicU64::new(0),
        analytics_pool,
        telemetry_health: telemetry.health(),
        response_cache,
        llama_server: llama_server.clone(),
    });

    // ─── Snapshot configured models once at startup ───────────────────────
    //
    // `model_snapshots` previously existed as schema with no writer at all
    // (Audit A15). Config is static for the life of the process, so one
    // snapshot per startup is sufficient provenance for now — a config
    // reload feature would need to re-emit this.
    for (name, definition) in &config.models {
        state.emit_telemetry(ai_gateway::telemetry::TelemetryEvent::ModelSnapshot {
            timestamp: chrono::Utc::now(),
            model_name: name.clone(),
            providers: definition.candidates.iter().map(|c| c.provider.clone()).collect(),
            capabilities: serde_json::to_value(&definition.capabilities).unwrap_or_default(),
        });
    }

    // ─── Spawn periodic provider health check worker ─────────────────────
    //
    // Holds its own Arc<AppState> clone (and therefore its own telemetry_tx
    // clone) for as long as it runs. Since this loop never exits on its own,
    // graceful shutdown must explicitly signal it to stop before the
    // telemetry channel can ever close (see the shutdown sequence below and
    // docs/REMEDIATION_PLAN.md Phase 0.5).
    let health_state = state.clone();
    let (health_shutdown_tx, mut health_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let health_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let adapters = health_state.provider_registry.all_adapters();
                    for (name, adapter) in adapters {
                        let start = std::time::Instant::now();
                        let health = adapter.health().await;
                        let latency_ms = start.elapsed().as_millis() as u64;
                        health_state.provider_registry.update_health(&name, health);
                        health_state.provider_registry.record_health_check_latency(&name, latency_ms);
                        tracing::trace!(provider = %name, ?health, latency_ms, "Provider health check completed");
                    }

                    // Same rationale as the model snapshot above, for
                    // `provider_snapshots` — taken on the existing health
                    // tick rather than a separate timer.
                    for status in health_state.provider_registry.all_statuses() {
                        health_state.emit_telemetry(ai_gateway::telemetry::TelemetryEvent::ProviderSnapshot {
                            timestamp: chrono::Utc::now(),
                            provider_name: status.name,
                            provider_type: format!("{:?}", status.provider_type),
                            health: format!("{:?}", status.health),
                            total_requests: status.total_requests,
                            total_errors: status.total_errors,
                            avg_latency_ms: status.avg_latency_ms,
                        });
                    }
                }
                _ = &mut health_shutdown_rx => {
                    tracing::info!("Health check worker shutting down");
                    return;
                }
            }
        }
    });

    // ─── Build Axum application ──────────────────────────────────────────
    // Keep our own Arc<AppState> clone alive (rather than moving the last
    // one into the router) so the shutdown sequence below can explicitly
    // drop it once the server and health worker have both stopped, letting
    // the telemetry channel close deterministically instead of relying on
    // process exit to clean it up.
    let app = server::build_router(state.clone());

    // ─── Start server ────────────────────────────────────────────────────
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&addr).await?;

    tracing::info!(
        address = %addr,
        "Gateway listening"
    );

    println!();
    println!("  ╔══════════════════════════════════════════╗");
    println!("  ║          AI Gateway v{}            ║", env!("CARGO_PKG_VERSION"));
    println!("  ║                                          ║");
    println!("  ║  API:  http://{:<28}  ║", addr);
    let base_addr = format!("{}/v1", addr);
    println!("  ║  Base: http://{:<25}  ║", base_addr);
    println!("  ╚══════════════════════════════════════════╝");
    println!();

    // Graceful shutdown: bound the in-flight connection drain period with configured timeout.
    let shutdown_timeout = std::time::Duration::from_secs(config.server.shutdown_timeout_secs);
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();

    let server_fut = axum::serve(listener, app).with_graceful_shutdown(async move {
        shutdown_signal().await;
        let _ = drain_tx.send(());
    });

    let server_result = tokio::select! {
        res = server_fut => res,
        _ = async {
            if drain_rx.await.is_ok() {
                tokio::time::sleep(shutdown_timeout).await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {
            tracing::warn!(
                timeout_secs = config.server.shutdown_timeout_secs,
                "Graceful shutdown drain timed out; terminating server"
            );
            Ok(())
        }
    };

    // ─── Drain telemetry before exiting ────────────────────────────────
    //
    // At this point `app`/`server_fut` have been dropped, releasing their
    // Arc<AppState> clone. Stop the health worker (it holds its own clone
    // and would otherwise keep the telemetry channel open forever), drop
    // our remaining clone, then close the pipeline's own sender and await
    // the writer task so any buffered batch is flushed rather than
    // abandoned when the process exits (docs/REMEDIATION_PLAN.md Phase 0.5).
    let _ = health_shutdown_tx.send(());
    if let Err(e) = health_task.await {
        tracing::warn!(error = %e, "Health check worker did not shut down cleanly");
    }
    // Stop the managed llama-server (if any) so the gateway never leaves an
    // orphaned model process holding GPU memory.
    if let Some(manager) = llama_server {
        manager.shutdown().await;
    }
    drop(state);
    telemetry.shutdown().await;

    server_result?;
    tracing::info!("Gateway shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install CTRL+C signal handler");
    tracing::info!("Shutdown signal received");
}
