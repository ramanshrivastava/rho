//! Safe provider HTTP error detail extraction (tau `tau_ai/http_errors.py`).
//!
//! Turns an HTTP failure body into a concise, secret-free message. The exact
//! precedence — `error.message`, then `error.code`, then the top-level
//! `message`/`detail`/`error` keys, then the trimmed raw body capped at 1000
//! chars — is reproduced verbatim so error strings match tau byte-for-byte
//! (the golden `error.events.jsonl` asserts one).

use serde_json::Value;

const MAX_ERROR_DETAIL_LENGTH: usize = 1000;

/// Return an actionable, secret-free HTTP error message for a provider response
/// (tau `provider_http_error_message`).
#[must_use]
pub fn provider_http_error_message(
    provider_name: &str,
    status_code: u16,
    body: &str,
    model: Option<&str>,
) -> String {
    let prefix = match model {
        Some(model) if !model.is_empty() => {
            format!("{provider_name} request failed with status {status_code} for model {model}")
        }
        _ => format!("{provider_name} request failed with status {status_code}"),
    };
    let detail = provider_http_error_detail(body);
    if detail.is_empty() {
        prefix
    } else {
        format!("{prefix}: {detail}")
    }
}

/// Extract a concise provider-supplied error detail from an HTTP body
/// (tau `provider_http_error_detail`).
#[must_use]
pub fn provider_http_error_detail(body: &str) -> String {
    if let Some(Value::Object(map)) = loads_object(body) {
        let detail = provider_error_detail_from_mapping(&map);
        if !detail.is_empty() {
            return detail;
        }
    }
    // tau: `body.strip()[:_MAX_ERROR_DETAIL_LENGTH]` — trim, then cap by *chars*.
    body.trim().chars().take(MAX_ERROR_DETAIL_LENGTH).collect()
}

/// Return the most useful message/code from a provider error object
/// (tau `provider_error_detail_from_mapping`).
#[must_use]
pub fn provider_error_detail_from_mapping(value: &serde_json::Map<String, Value>) -> String {
    if let Some(Value::Object(error)) = value.get("error") {
        if let Some(Value::String(message)) = error.get("message") {
            if !message.is_empty() {
                return message.clone();
            }
        }
        if let Some(Value::String(code)) = error.get("code") {
            if !code.is_empty() {
                return code.clone();
            }
        }
    }
    for key in ["message", "detail", "error"] {
        match value.get(key) {
            Some(Value::String(detail)) if !detail.is_empty() => return detail.clone(),
            Some(Value::Object(nested)) => {
                let nested = provider_error_detail_from_mapping(nested);
                if !nested.is_empty() {
                    return nested;
                }
            }
            _ => {}
        }
    }
    String::new()
}

fn loads_object(value: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(value) {
        Ok(v @ Value::Object(_)) => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_nested_error_message() {
        let body = r#"{"error":{"message":"bad request"}}"#;
        assert_eq!(
            provider_http_error_message("OpenAI-compatible provider", 400, body, Some("gpt-x")),
            "OpenAI-compatible provider request failed with status 400 for model gpt-x: bad request"
        );
    }

    #[test]
    fn falls_back_to_error_code() {
        let body = r#"{"error":{"code":"model_unavailable"}}"#;
        assert_eq!(provider_http_error_detail(body), "model_unavailable");
    }

    #[test]
    fn plain_body_is_trimmed_and_capped() {
        assert_eq!(provider_http_error_detail("  boom  "), "boom");
        let long = "x".repeat(2000);
        assert_eq!(
            provider_http_error_detail(&long).len(),
            MAX_ERROR_DETAIL_LENGTH
        );
    }

    #[test]
    fn omits_model_when_absent() {
        assert_eq!(
            provider_http_error_message("prov", 500, "", None),
            "prov request failed with status 500"
        );
    }
}
