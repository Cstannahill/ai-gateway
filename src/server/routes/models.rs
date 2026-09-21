use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse, Json};

use crate::server::AppState;

/// GET /v1/models
///
/// Returns available models in OpenAI-compatible format, aggregating both
/// static configured models and dynamically discovered models from active providers.
pub async fn handle(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut model_map: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    // 1. Static / configured models from model_registry
    for m in state.model_registry.list_models() {
        model_map.insert(
            m.id.clone(),
            serde_json::json!({
                "id": m.id,
                "object": m.object,
                "created": m.created,
                "owned_by": m.owned_by,
                "capabilities": m.capabilities,
                "providers": m.providers,
            }),
        );
    }

    // 2. Query active providers dynamically with a 3-second timeout per provider
    let adapters = state.provider_registry.all_adapters();
    let query_futures = adapters.into_iter().map(|(provider_name, adapter)| {
        async move {
            match tokio::time::timeout(std::time::Duration::from_secs(3), adapter.models()).await {
                Ok(Ok(provider_models)) => Some((provider_name, provider_models)),
                _ => None,
            }
        }
    });

    let results = futures::future::join_all(query_futures).await;
    for (provider_name, models) in results.into_iter().flatten() {
        for m in models {
            let full_id = if m.id.starts_with(&format!("{}/", provider_name)) {
                m.id.clone()
            } else {
                format!("{}/{}", provider_name, m.id)
            };

            model_map
                .entry(full_id.clone())
                .and_modify(|entry| {
                    if let Some(arr) = entry.get_mut("providers").and_then(|p| p.as_array_mut()) {
                        let name_val = serde_json::Value::String(provider_name.clone());
                        if !arr.contains(&name_val) {
                            arr.push(name_val);
                        }
                    }
                })
                .or_insert_with(|| {
                    serde_json::json!({
                        "id": full_id,
                        "object": m.object,
                        "created": m.created,
                        "owned_by": if m.owned_by.is_empty() { provider_name.clone() } else { m.owned_by },
                        "capabilities": m.capabilities,
                        "providers": [provider_name.clone()],
                    })
                });
        }
    }

    let data: Vec<serde_json::Value> = model_map.into_values().collect();

    Json(serde_json::json!({
        "object": "list",
        "data": data,
    }))
}
