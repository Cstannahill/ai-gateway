//! Cost a provider reports about its own response.
//!
//! OpenRouter puts `usage.cost` (credits; 1 credit = 1 USD) on every response -
//! in the final usage chunk of a stream, or in the body otherwise - and
//! `usage.cost_details.upstream_inference_cost` next to it. Most providers
//! report nothing, so this is `None` far more often than not, and that is
//! never turned into a fabricated zero.

/// The USD the provider says this request cost, from its raw `usage` object.
///
/// With `is_byok` (you supplied your own upstream key) OpenRouter's `cost` is
/// only its own fee and the upstream provider bills the rest separately, so
/// `upstream_inference_cost` is added: the spend this request really caused.
/// Negative, NaN or non-numeric values are ignored.
pub fn reported_cost_usd(raw_usage: &serde_json::Value) -> Option<f64> {
    let number = |v: &serde_json::Value| v.as_f64().filter(|c| c.is_finite() && *c >= 0.0);
    let cost = number(raw_usage.get("cost")?)?;
    let byok = raw_usage.get("is_byok").and_then(|b| b.as_bool()).unwrap_or(false);
    let upstream = if byok {
        raw_usage
            .get("cost_details")
            .and_then(|d| d.get("upstream_inference_cost"))
            .and_then(number)
            .unwrap_or(0.0)
    } else {
        0.0
    };
    Some(cost + upstream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_cost() {
        assert_eq!(reported_cost_usd(&json!({"prompt_tokens": 5, "cost": 0.00123})), Some(0.00123));
        assert_eq!(reported_cost_usd(&json!({"cost": 0})), Some(0.0), "a reported zero is a real zero");
    }

    #[test]
    fn non_byok_ignores_upstream_cost() {
        let usage = json!({"cost": 0.95, "is_byok": false, "cost_details": {"upstream_inference_cost": 19}});
        assert_eq!(reported_cost_usd(&usage), Some(0.95));
    }

    #[test]
    fn byok_adds_the_upstream_charge() {
        let usage = json!({"cost": 0.005, "is_byok": true, "cost_details": {"upstream_inference_cost": 0.02}});
        assert_eq!(reported_cost_usd(&usage), Some(0.025));
        // BYOK without the detail still reports the fee.
        assert_eq!(reported_cost_usd(&json!({"cost": 0.005, "is_byok": true})), Some(0.005));
    }

    #[test]
    fn absent_or_malformed_cost_is_none_not_zero() {
        assert_eq!(reported_cost_usd(&json!({"prompt_tokens": 5})), None);
        assert_eq!(reported_cost_usd(&json!({"cost": "free"})), None);
        assert_eq!(reported_cost_usd(&json!({"cost": null})), None);
        assert_eq!(reported_cost_usd(&json!({"cost": -1.0})), None);
        assert_eq!(reported_cost_usd(&json!("cost")), None);
    }
}
