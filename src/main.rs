use std::sync::Arc;

use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use ai_gateway::config::GatewayConfig;
use ai_gateway::providers::{self, Provider};
use ai_gateway::registry::{ModelRegistry, ProviderRegistry};
use ai_gateway::routing::Router as RoutingEngine;
use ai_gateway::server::{self, AppState};
use ai_gateway::telemetry::TelemetryPipeline;
use ai_gateway::types;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    // ─── Load configuration ──────────────────────────────────────────────
    let config_path = std::env::args().nth(1);
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

        let adapter: Arc<dyn Provider> = match pconfig.r#type {
            types::ProviderType::LlamaCpp => {
                Arc::new(providers::llama_cpp::LlamaCppAdapter::new(&pconfig)?)
            }
            types::ProviderType::OpenRouter => {
                Arc::new(providers::openrouter::OpenRouterAdapter::new(&pconfig)?)
            }
            types::ProviderType::Ollama => {
                Arc::new(providers::ollama::OllamaAdapter::new(&pconfig)?)
            }
            types::ProviderType::OllamaCloud => {
                Arc::new(providers::ollama_cloud::OllamaCloudAdapter::new(&pconfig)?)
            }
            types::ProviderType::Vllm => {
                Arc::new(providers::vllm::VllmAdapter::new(&pconfig)?)
            }
            types::ProviderType::Gemini => {
                Arc::new(providers::gemini::GeminiAdapter::new(&pconfig)?)
            }
            types::ProviderType::HuggingFace => {
                Arc::new(providers::huggingface::HuggingFaceAdapter::new(&pconfig)?)
            }
            types::ProviderType::OpenAiCompatible => {
                Arc::new(providers::openai_compatible::OpenAICompatibleAdapter::new(
                    &pconfig,
                    types::ModelCapabilities::default(),
                )?)
            }
            _ => {
                tracing::warn!(
                    provider = %name,
                    r#type = ?pconfig.r#type,
                    "Unsupported provider type, using generic OpenAI-compatible adapter"
                );
                Arc::new(providers::openai_compatible::OpenAICompatibleAdapter::new(
                    &pconfig,
                    types::ModelCapabilities::default(),
                )?)
            }
        };

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
    let router = RoutingEngine::new(config.routing.default_policy.clone());

    // ─── Build application state ─────────────────────────────────────────
    let state = Arc::new(AppState {
        config: config.clone(),
        model_registry,
        provider_registry,
        router,
        telemetry_tx: telemetry.sender(),
        start_time: std::time::Instant::now(),
    });

    // ─── Spawn periodic provider health check worker ─────────────────────
    let health_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let adapters = health_state.provider_registry.all_adapters();
            for (name, adapter) in adapters {
                let start = std::time::Instant::now();
                let health = adapter.health().await;
                let latency_ms = start.elapsed().as_millis() as u64;
                health_state.provider_registry.update_health(&name, health);
                health_state.provider_registry.record_latency(&name, latency_ms);
                tracing::trace!(provider = %name, ?health, latency_ms, "Provider health check completed");
            }
        }
    });

    // ─── Build Axum application ──────────────────────────────────────────
    let app = server::build_router(state);

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

    tokio::select! {
        res = server_fut => {
            res?;
        }
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
        }
    }

    tracing::info!("Gateway shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install CTRL+C signal handler");
    tracing::info!("Shutdown signal received");
}
