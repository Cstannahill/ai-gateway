//! Prometheus text exposition (format 0.0.4) for `GET /metrics/prometheus`.
//!
//! Written by hand rather than with a metrics crate: the gateway already keeps
//! the numbers (provider counters, circuit/cooldown state, cache and telemetry
//! health) and only needs to print them. Rendering is a pure function of a
//! [`MetricsSnapshot`] so the format can be tested without a running server.
//!
//! Deliberately absent: TTFT / tokens-per-second histograms. They are computed
//! in the telemetry pipeline and stored in Postgres, where
//! `/v1/analytics/providers/comparison` serves them; a second in-memory copy
//! would be a second source of truth.

use std::fmt::Write;

use crate::registry::provider_registry::{LATENCY_BUCKETS_MS, ProviderMetrics};
use crate::types::{ProviderHealth, ProviderStatus};

/// Everything the exposition needs, gathered by the handler.
pub struct MetricsSnapshot {
    pub version: &'static str,
    pub uptime_seconds: u64,
    pub gateway_requests: u64,
    /// `(status, registry metrics)` per provider, sorted by name by [`render`].
    pub providers: Vec<(ProviderStatus, ProviderMetrics)>,
    /// `(hits, misses, entries)`; `None` when response caching is off.
    pub cache: Option<(u64, u64, usize)>,
    pub telemetry_mode: &'static str,
    pub telemetry_dropped_events: u64,
    pub telemetry_last_flush_ok: bool,
    pub telemetry_consecutive_flush_failures: u32,
    /// `(reloads applied, last attempt ok)`; `None` when reloading is not set up.
    pub reload: Option<(u64, Option<bool>)>,
}

/// Escape a label value: backslash, double quote and newline.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// A float as the exposition format wants it. Never `NaN`/`inf` - those are
/// valid but only ever indicate a bug here, so they are printed as 0.
fn number(v: f64) -> String {
    if v.is_finite() { format!("{v}") } else { "0".to_string() }
}

struct Writer {
    out: String,
}

impl Writer {
    fn header(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.out, "# HELP {name} {help}");
        let _ = writeln!(self.out, "# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
        self.out.push_str(name);
        if !labels.is_empty() {
            self.out.push('{');
            for (i, (k, v)) in labels.iter().enumerate() {
                if i > 0 {
                    self.out.push(',');
                }
                let _ = write!(self.out, "{k}=\"{}\"", escape(v));
            }
            self.out.push('}');
        }
        let _ = writeln!(self.out, " {value}");
    }

    /// One metric family with a sample per provider.
    fn per_provider<T: std::fmt::Display>(
        &mut self,
        name: &str,
        kind: &str,
        help: &str,
        providers: &[(ProviderStatus, ProviderMetrics)],
        value: impl Fn(&ProviderStatus, &ProviderMetrics) -> T,
    ) {
        self.header(name, kind, help);
        for (status, metrics) in providers {
            self.sample(name, &[("provider", &status.name)], value(status, metrics));
        }
    }
}

/// Render the snapshot as Prometheus text.
pub fn render(snapshot: &MetricsSnapshot) -> String {
    let mut w = Writer { out: String::new() };
    let mut providers: Vec<&(ProviderStatus, ProviderMetrics)> = snapshot.providers.iter().collect();
    providers.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    let providers: Vec<(ProviderStatus, ProviderMetrics)> = providers.into_iter().cloned().collect();

    w.header("gateway_info", "gauge", "Gateway build information; the value is always 1.");
    w.sample("gateway_info", &[("version", snapshot.version)], 1);
    w.header("gateway_uptime_seconds", "gauge", "Seconds since the gateway started.");
    w.sample("gateway_uptime_seconds", &[], snapshot.uptime_seconds);
    w.header("gateway_requests_total", "counter", "Requests received by the gateway across all endpoints.");
    w.sample("gateway_requests_total", &[], snapshot.gateway_requests);

    w.per_provider(
        "gateway_provider_requests_total",
        "counter",
        "Provider attempts made (retries and fallbacks each count).",
        &providers,
        |s, _| s.total_requests,
    );
    w.per_provider(
        "gateway_provider_errors_total",
        "counter",
        "Provider attempts that failed.",
        &providers,
        |s, _| s.total_errors,
    );
    w.per_provider(
        "gateway_provider_active_requests",
        "gauge",
        "Requests in flight to the provider.",
        &providers,
        |s, _| s.active_requests,
    );

    w.header(
        "gateway_provider_health",
        "gauge",
        "Provider health as a state set: the current state is 1, the others 0.",
    );
    for (status, _) in &providers {
        for (state, health) in [
            ("healthy", ProviderHealth::Healthy),
            ("degraded", ProviderHealth::Degraded),
            ("unhealthy", ProviderHealth::Unhealthy),
            ("unknown", ProviderHealth::Unknown),
        ] {
            w.sample(
                "gateway_provider_health",
                &[("provider", &status.name), ("state", state)],
                u8::from(status.health == health),
            );
        }
    }

    w.per_provider(
        "gateway_provider_circuit_open",
        "gauge",
        "1 while the provider's circuit breaker is open (routing skips it).",
        &providers,
        |s, _| u8::from(s.circuit_state == "open"),
    );
    w.per_provider(
        "gateway_provider_cooling_down",
        "gauge",
        "1 while the provider is skipped because it asked for a wait (Retry-After).",
        &providers,
        |_, m| u8::from(m.cooling_down),
    );
    w.per_provider(
        "gateway_provider_reported_cost_usd_total",
        "counter",
        "USD providers reported for requests served since startup (e.g. OpenRouter usage.cost). Providers that report no cost contribute nothing.",
        &providers,
        |_, m| number(m.reported_cost_usd),
    );

    let name = "gateway_provider_request_duration_seconds";
    w.header(name, "histogram", "Duration of successfully served provider requests.");
    for (status, metrics) in &providers {
        let mut cumulative = 0u64;
        for (i, bound_ms) in LATENCY_BUCKETS_MS.iter().enumerate() {
            cumulative += metrics.latency_buckets.get(i).copied().unwrap_or(0);
            w.sample(
                &format!("{name}_bucket"),
                &[("provider", &status.name), ("le", &number(*bound_ms as f64 / 1000.0))],
                cumulative,
            );
        }
        // Overflow bucket: `+Inf` must equal the total count.
        w.sample(
            &format!("{name}_bucket"),
            &[("provider", &status.name), ("le", "+Inf")],
            metrics.latency_count,
        );
        w.sample(
            &format!("{name}_sum"),
            &[("provider", &status.name)],
            number(metrics.latency_sum_ms as f64 / 1000.0),
        );
        w.sample(&format!("{name}_count"), &[("provider", &status.name)], metrics.latency_count);
    }

    if let Some((hits, misses, entries)) = snapshot.cache {
        w.header("gateway_cache_hits_total", "counter", "Response cache hits.");
        w.sample("gateway_cache_hits_total", &[], hits);
        w.header("gateway_cache_misses_total", "counter", "Response cache misses.");
        w.sample("gateway_cache_misses_total", &[], misses);
        w.header("gateway_cache_entries", "gauge", "Entries currently in the response cache.");
        w.sample("gateway_cache_entries", &[], entries);
    }

    w.header("gateway_telemetry_mode", "gauge", "Telemetry pipeline mode as a state set (1 = current).");
    w.sample("gateway_telemetry_mode", &[("mode", snapshot.telemetry_mode)], 1);
    w.header("gateway_telemetry_dropped_events_total", "counter", "Telemetry events dropped because the queue was full.");
    w.sample("gateway_telemetry_dropped_events_total", &[], snapshot.telemetry_dropped_events);
    w.header("gateway_telemetry_last_flush_ok", "gauge", "1 if the last telemetry flush to Postgres succeeded.");
    w.sample("gateway_telemetry_last_flush_ok", &[], u8::from(snapshot.telemetry_last_flush_ok));
    w.header(
        "gateway_telemetry_consecutive_flush_failures",
        "gauge",
        "Telemetry flushes that failed in a row.",
    );
    w.sample(
        "gateway_telemetry_consecutive_flush_failures",
        &[],
        snapshot.telemetry_consecutive_flush_failures,
    );

    if let Some((applied, last_ok)) = snapshot.reload {
        w.header("gateway_config_reloads_applied_total", "counter", "Config reloads that changed something.");
        w.sample("gateway_config_reloads_applied_total", &[], applied);
        if let Some(ok) = last_ok {
            w.header("gateway_config_last_reload_ok", "gauge", "1 if the last config reload attempt was accepted.");
            w.sample("gateway_config_last_reload_ok", &[], u8::from(ok));
        }
    }

    w.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProviderType;

    fn status(name: &str, health: ProviderHealth, circuit: &str) -> ProviderStatus {
        ProviderStatus {
            name: name.to_string(),
            provider_type: ProviderType::OpenAiCompatible,
            health,
            enabled: true,
            active_requests: 2,
            total_requests: 10,
            total_errors: 3,
            avg_latency_ms: None,
            avg_health_check_latency_ms: None,
            circuit_state: circuit.to_string(),
        }
    }

    fn metrics(name: &str) -> ProviderMetrics {
        // 3 requests: 40ms, 700ms, 400s.
        let mut buckets = vec![0u64; LATENCY_BUCKETS_MS.len() + 1];
        buckets[0] = 1; // <= 50ms
        buckets[4] = 1; // <= 1000ms
        buckets[LATENCY_BUCKETS_MS.len()] = 1; // overflow
        ProviderMetrics {
            name: name.to_string(),
            latency_buckets: buckets,
            latency_sum_ms: 40 + 700 + 400_000,
            latency_count: 3,
            reported_cost_usd: 0.0125,
            cooling_down: true,
        }
    }

    fn snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            version: "1.2.3",
            uptime_seconds: 42,
            gateway_requests: 7,
            providers: vec![
                (status("zeta", ProviderHealth::Degraded, "open"), metrics("zeta")),
                (status("alpha", ProviderHealth::Healthy, "closed"), metrics("alpha")),
            ],
            cache: Some((5, 6, 7)),
            telemetry_mode: "database",
            telemetry_dropped_events: 1,
            telemetry_last_flush_ok: true,
            telemetry_consecutive_flush_failures: 0,
            reload: Some((2, Some(false))),
        }
    }

    /// Parse the exposition back into `(name, labels, value)` lines.
    fn samples(text: &str) -> Vec<(String, String, String)> {
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .map(|l| {
                let (head, value) = l.rsplit_once(' ').expect("sample has a value");
                match head.split_once('{') {
                    Some((name, rest)) => (name.to_string(), rest.trim_end_matches('}').to_string(), value.to_string()),
                    None => (head.to_string(), String::new(), value.to_string()),
                }
            })
            .collect()
    }

    fn value(text: &str, name: &str, labels: &str) -> Option<String> {
        samples(text)
            .into_iter()
            .find(|(n, l, _)| n == name && l == labels)
            .map(|(_, _, v)| v)
    }

    #[test]
    fn every_family_has_help_and_type_and_every_sample_is_well_formed() {
        let text = render(&snapshot());
        let mut typed: Vec<String> = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("# TYPE ") {
                let mut it = rest.split(' ');
                typed.push(it.next().unwrap().to_string());
                assert!(matches!(it.next(), Some("counter" | "gauge" | "histogram")), "{line}");
            }
        }
        for (name, _, v) in samples(&text) {
            let family = name
                .trim_end_matches("_bucket")
                .trim_end_matches("_sum")
                .trim_end_matches("_count")
                .to_string();
            assert!(typed.contains(&name) || typed.contains(&family), "sample {name} has no # TYPE");
            assert!(v.parse::<f64>().is_ok(), "{name} value {v:?} is not a number");
        }
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn providers_are_sorted_and_carry_their_own_values() {
        let text = render(&snapshot());
        let alpha = text.find("provider=\"alpha\"").unwrap();
        let zeta = text.find("provider=\"zeta\"").unwrap();
        assert!(alpha < zeta);
        assert_eq!(value(&text, "gateway_provider_requests_total", "provider=\"alpha\"").as_deref(), Some("10"));
        assert_eq!(value(&text, "gateway_provider_circuit_open", "provider=\"zeta\"").as_deref(), Some("1"));
        assert_eq!(value(&text, "gateway_provider_circuit_open", "provider=\"alpha\"").as_deref(), Some("0"));
        assert_eq!(value(&text, "gateway_provider_cooling_down", "provider=\"alpha\"").as_deref(), Some("1"));
        assert_eq!(
            value(&text, "gateway_provider_reported_cost_usd_total", "provider=\"alpha\"").as_deref(),
            Some("0.0125")
        );
    }

    #[test]
    fn health_is_a_state_set_with_exactly_one_active_state() {
        let text = render(&snapshot());
        let active: Vec<_> = samples(&text)
            .into_iter()
            .filter(|(n, l, v)| n == "gateway_provider_health" && l.contains("provider=\"zeta\"") && v == "1")
            .collect();
        assert_eq!(active.len(), 1);
        assert!(active[0].1.contains("state=\"degraded\""));
    }

    #[test]
    fn histogram_buckets_are_cumulative_and_inf_equals_count() {
        let text = render(&snapshot());
        let name = "gateway_provider_request_duration_seconds_bucket";
        let buckets: Vec<(String, u64)> = samples(&text)
            .into_iter()
            .filter(|(n, l, _)| n == name && l.contains("provider=\"alpha\""))
            .map(|(_, l, v)| (l, v.parse().unwrap()))
            .collect();
        assert_eq!(buckets.len(), LATENCY_BUCKETS_MS.len() + 1);
        assert!(buckets.windows(2).all(|w| w[0].1 <= w[1].1), "cumulative: {buckets:?}");
        assert!(buckets.first().unwrap().0.contains("le=\"0.05\""));
        assert_eq!(buckets.first().unwrap().1, 1, "<= 50ms");
        let inf = buckets.last().unwrap();
        assert!(inf.0.contains("le=\"+Inf\""));
        assert_eq!(inf.1, 3);
        assert_eq!(
            value(&text, "gateway_provider_request_duration_seconds_count", "provider=\"alpha\"").as_deref(),
            Some("3")
        );
        assert_eq!(
            value(&text, "gateway_provider_request_duration_seconds_sum", "provider=\"alpha\"").as_deref(),
            Some("400.74")
        );
    }

    #[test]
    fn optional_sections_appear_only_when_configured() {
        let text = render(&snapshot());
        assert_eq!(value(&text, "gateway_cache_entries", "").as_deref(), Some("7"));
        assert_eq!(value(&text, "gateway_config_reloads_applied_total", "").as_deref(), Some("2"));
        assert_eq!(value(&text, "gateway_config_last_reload_ok", "").as_deref(), Some("0"));

        let mut bare = snapshot();
        bare.cache = None;
        bare.reload = None;
        let text = render(&bare);
        assert!(!text.contains("gateway_cache_"));
        assert!(!text.contains("gateway_config_"));
    }

    #[test]
    fn label_values_are_escaped() {
        let mut s = snapshot();
        s.providers = vec![(status("we\"ird\\na\nme", ProviderHealth::Healthy, "closed"), metrics("x"))];
        let text = render(&s);
        assert!(text.contains(r#"provider="we\"ird\\na\nme""#), "{text}");
        // The newline in the name must not have produced a broken line.
        assert!(samples(&text).iter().all(|(_, _, v)| v.parse::<f64>().is_ok()));
    }

    #[test]
    fn non_finite_costs_never_reach_the_output() {
        let mut s = snapshot();
        s.providers[0].1.reported_cost_usd = f64::NAN;
        let text = render(&s);
        assert!(
            samples(&text).iter().all(|(_, _, v)| v.parse::<f64>().is_ok_and(f64::is_finite)),
            "{text}"
        );
    }
}
