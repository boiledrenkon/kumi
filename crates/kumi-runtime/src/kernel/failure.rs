use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use kumi_common::js::json::stringify;
use kumi_common::js::number::parse as js_number;
use kumi_common::js::string::{head, trim};
use regex::Regex;
use serde_json::Value;

use crate::ai::error::{ApiCallError, LanguageModelError};
use crate::core::errors::{FailureKind, KumiError};

/// How many times a model call is tried again (a busy or overloaded provider usually answers soon).
pub const MAX_RETRIES: u32 = 3;
/// The longest Kumi waits for a provider before giving up on the answer: a plan's limit resetting later
/// than this (a weekly window, say) stops it, saying when, so the producer can pick another way.
pub const LONGEST_WAIT_MS: f64 = 6.0 * 3600.0 * 1000.0;
/// How long after a plan's limit resets Kumi tries again: the provider's clock and Kumi's may differ.
pub const RESET_MARGIN_MS: f64 = 30_000.0;
/// Waits between tries while there's no connection: each a little longer, then every 30 s.
const OFFLINE_MAX_WAIT_MS: f64 = 30_000.0;
/// Error codes that mean a plan's credit or spend is used up: waiting won't help.
const QUOTA_CODES: [&str; 5] = [
    "insufficient_quota",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "organization_usage_limit_exceeded",
];

/// A plan's limit, as the provider reported it (ChatGPT's 429 body, a quota code).
#[derive(Debug, Clone, PartialEq)]
pub enum PlanLimit {
    /// The plan's usage is used up until it resets (epoch ms), when the provider says.
    Reached { resets_at_ms: Option<f64> },
    /// The plan doesn't include this model: waiting won't help.
    NotIncluded,
    /// Credit or a spend limit is used up: waiting won't help.
    Quota,
}

/// What Kumi does about a failed model call, when nothing of it has reached the producer yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wait {
    /// Try again after this many ms (a busy or overloaded provider, a request limit): counted against MAX_RETRIES.
    Again(f64),
    /// A plan's limit resets after this many ms (a margin included): then the same answer carries on.
    Reset(f64),
    /// There's no connection: try again after this many ms, for as long as the answer may run.
    Offline(f64),
}

impl Wait {
    pub fn ms(self) -> f64 {
        match self {
            Wait::Again(ms) | Wait::Reset(ms) | Wait::Offline(ms) => ms,
        }
    }
}

/// The failed call an error stands for: its API call's, or one the provider reported inside its stream (Anthropic's
/// overloaded_error, OpenAI's response.failed), with the status and retryability the provider gave it.
fn failed_call(error: &LanguageModelError) -> Option<Cow<'_, ApiCallError>> {
    match error {
        LanguageModelError::ApiCall(error) => Some(Cow::Borrowed(error)),
        LanguageModelError::ProviderStream(error) => {
            let status = error.get("statusCode").and_then(Value::as_u64).and_then(|status| u16::try_from(status).ok());
            let mut call = ApiCallError::new(error.get("message").and_then(Value::as_str).unwrap_or(""), "", None, status);
            call.is_retryable = error.get("isRetryable").and_then(Value::as_bool).unwrap_or(false);
            call.response_body = error.get("data").map(stringify);
            Some(Cow::Owned(call))
        }
        _ => None,
    }
}

/// Delay before retry number `attempt` (from 0), or None when the failure is not worth retrying: what the
/// provider asks for, or 0.75 s, 2.25 s, 6.75 s when it doesn't say; a plan's limit until it resets.
pub fn retry_delay_ms(error: &LanguageModelError, attempt: u32) -> Option<f64> {
    wait_for(error, attempt, attempt, kumi_common::time::now_ms_f64()).map(Wait::ms)
}

/// What to do about a failed call before any of it reached the producer: try again soon (`attempt` tries so far),
/// wait for a plan's limit to reset, or keep trying while offline (`offline` tries so far); None to stop. A wait
/// past [`LONGEST_WAIT_MS`] is none.
pub fn wait_for(error: &LanguageModelError, attempt: u32, offline: u32, now_ms: f64) -> Option<Wait> {
    let call = failed_call(error)?;
    let wait = match plan_limit(error, now_ms) {
        Some(PlanLimit::Reached { resets_at_ms }) => {
            let until = resets_at_ms.map(|at| (at - now_ms).max(0.0)).or_else(|| requested(&call, now_ms))?;
            Wait::Reset(until + RESET_MARGIN_MS)
        }
        Some(PlanLimit::NotIncluded | PlanLimit::Quota) => return None,
        None if !call.is_retryable => return None,
        // No answer at all: the connection, not the provider.
        None if call.status_code.is_none() => Wait::Offline((750.0 * 3f64.powi(offline.min(8) as i32)).min(OFFLINE_MAX_WAIT_MS)),
        None => Wait::Again(requested(&call, now_ms).unwrap_or_else(|| 750.0 * 3f64.powi(attempt as i32)).max(250.0)),
    };
    (wait.ms() <= LONGEST_WAIT_MS).then_some(wait)
}

/// The wait the provider asked for: Retry-After in ms or seconds, or as a date.
fn requested(call: &ApiCallError, now_ms: f64) -> Option<f64> {
    let headers = call.response_headers.as_ref()?;
    let after_ms = headers.get("retry-after-ms").and_then(|value| js_number(value)).unwrap_or(f64::NAN);
    if after_ms.is_finite() && after_ms > 0.0 {
        return Some(after_ms);
    }
    crate::web::net::retry_after(headers, None, Some(now_ms)).filter(|ms| ms.is_finite() && *ms > 0.0)
}

/// The plan limit a failed call reports: ChatGPT's `usage_limit_reached` (with when it resets), a plan that doesn't
/// include the model, or used-up credit or spend.
pub fn plan_limit(error: &LanguageModelError, now_ms: f64) -> Option<PlanLimit> {
    let call = failed_call(error)?;
    let body = call.response_body.as_deref().and_then(|body| serde_json::from_str::<Value>(body).ok()).or_else(|| call.data.clone())?;
    let mut codes = Vec::new();
    collect_codes(&body, 0, &mut codes);
    if codes.contains(&"usage_limit_reached") {
        let seconds = |key: &str| find_number(&body, key, 0).filter(|seconds| seconds.is_finite() && *seconds > 0.0);
        let resets_at_ms = seconds("resets_at").map(|at| at * 1000.0).or_else(|| seconds("resets_in_seconds").map(|s| now_ms + s * 1000.0));
        return Some(PlanLimit::Reached { resets_at_ms });
    }
    if codes.contains(&"usage_not_included") {
        return Some(PlanLimit::NotIncluded);
    }
    codes.iter().any(|code| QUOTA_CODES.contains(code)).then_some(PlanLimit::Quota)
}

/// The values of `type` and `code` fields, a few levels down (`{"error":{"type":…}}`, a stream's frame).
fn collect_codes<'v>(value: &'v Value, depth: u32, out: &mut Vec<&'v str>) {
    let Some(object) = value.as_object().filter(|_| depth < 5) else { return };
    for (key, value) in object {
        match value {
            Value::String(code) if key == "type" || key == "code" => out.push(code),
            Value::Object(_) => collect_codes(value, depth + 1, out),
            _ => {}
        }
    }
}

fn find_number(value: &Value, key: &str, depth: u32) -> Option<f64> {
    let object = value.as_object().filter(|_| depth < 5)?;
    object.get(key).and_then(Value::as_f64).or_else(|| object.values().find_map(|value| find_number(value, key, depth + 1)))
}

/// Why a model call is being tried again, in a few words for the status line: "ChatGPT is busy (HTTP 429)",
/// "ChatGPT's plan limit".
pub fn retry_reason(error: &LanguageModelError, binding_id: &str) -> String {
    let name = provider_name(binding_id);
    if matches!(plan_limit(error, 0.0), Some(PlanLimit::Reached { .. })) {
        return format!("{name}'s plan limit");
    }
    match failed_call(error).and_then(|error| error.status_code) {
        // A stream that broke off after the response began keeps its 200.
        Some(200..=299) => format!("{name}'s answer broke off"),
        Some(429) => format!("{name} is busy (HTTP 429)"),
        Some(status @ (503 | 529)) => format!("{name} is overloaded (HTTP {status})"),
        Some(status) if status >= 500 => format!("{name} is having trouble (HTTP {status})"),
        Some(status) => format!("{name} asked Kumi to wait (HTTP {status})"),
        None => format!("{name} couldn't be reached"),
    }
}

/// A provider by the name producers know it ("ChatGPT"), from a binding id ("openai-codex/gpt-6-sol").
pub fn provider_name(binding_id: &str) -> &str {
    let provider = binding_id.split('/').next().unwrap_or(binding_id);
    PROVIDER_NAMES.get(provider).copied().unwrap_or(provider)
}

/// When a plan's limit resets, in the producer's words: "14:20", "Tue 09:00" past a day away.
pub fn reset_words(resets_at_ms: f64, now_ms: f64) -> String {
    use chrono::{Local, TimeZone};
    let Some(at) = Local.timestamp_millis_opt(resets_at_ms as i64).single() else { return "later".into() };
    if resets_at_ms - now_ms < 20.0 * 3600.0 * 1000.0 {
        at.format("%H:%M").to_string()
    } else {
        at.format("%a %H:%M").to_string()
    }
}

/// Providers by the name producers know them; the binding id's prefix otherwise.
static PROVIDER_NAMES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("openai-codex", "ChatGPT"),
        ("openai", "OpenAI"),
        ("anthropic", "Anthropic"),
        ("opencode", "OpenCode Zen"),
        ("opencode-go", "OpenCode Go"),
    ])
});

/// Map any inference failure to a message Kumi wrote, tagged with the provider it concerns so the
/// app can offer the fix (sign in again, choose another model). Provider detail is bounded and never
/// includes credentials.
pub fn describe_failure(error: &LanguageModelError, binding_id: &str) -> KumiError {
    if let LanguageModelError::Kumi(error) = error {
        return error.clone();
    }
    let provider = binding_id.split('/').next().unwrap_or(binding_id);
    let model = binding_id.get(provider.len() + 1..).filter(|rest| !rest.is_empty()).unwrap_or(binding_id);
    let name = PROVIDER_NAMES.get(provider).copied().unwrap_or(provider);
    let now = kumi_common::time::now_ms_f64();
    match plan_limit(error, now) {
        Some(PlanLimit::Reached { resets_at_ms }) => {
            let when = resets_at_ms.map(|at| format!("; it resets at {}", reset_words(at, now))).unwrap_or_default();
            return KumiError::with_provider(
                FailureKind::UsageLimit,
                format!("{name}'s plan limit is reached{when}. Choose a fallback model (/model), another model, or sign in to another provider."),
                provider,
            );
        }
        Some(PlanLimit::NotIncluded) => {
            return KumiError::with_provider(
                FailureKind::Model,
                format!("{name}'s plan doesn't include {model}; choose another model."),
                provider,
            );
        }
        Some(PlanLimit::Quota) => {
            return KumiError::with_provider(
                FailureKind::Billing,
                format!("{name} has no credit left for this sign-in: add credit, or choose another model."),
                provider,
            );
        }
        None => {}
    }
    if let Some(error) = failed_call(error) {
        let status = error.status_code;
        let detail = provider_detail(&error);
        let with = |lead: &str| if detail.is_empty() { String::new() } else { format!("{lead}{detail}") };
        return match status {
            Some(401) => KumiError::with_provider(
                FailureKind::Auth,
                format!("{name} didn't accept Kumi's sign-in (HTTP 401): sign in again, or check the key."),
                provider,
            ),
            Some(403) => KumiError::with_provider(
                FailureKind::Auth,
                format!("{name} says this sign-in can't use {model} (HTTP 403){}.", with(": ")),
                provider,
            ),
            Some(402) => KumiError::with_provider(
                FailureKind::Billing,
                format!("{name} needs billing sorted before it answers (HTTP 402){}.", with(": ")),
                provider,
            ),
            Some(404) => KumiError::with_provider(
                FailureKind::Model,
                format!("{name} doesn't offer {model} to this sign-in (HTTP 404); choose another model."),
                provider,
            ),
            Some(429) => KumiError::with_provider(
                FailureKind::RateLimit,
                format!(
                    "{name}'s rate or usage limit was reached (HTTP 429); try again in a moment{}.",
                    if detail.is_empty() { String::new() } else { format!(" ({detail})") }
                ),
                provider,
            ),
            Some(status @ (529 | 503)) => KumiError::with_provider(
                FailureKind::Provider,
                format!("{name} is overloaded right now (HTTP {status}); try again in a moment."),
                provider,
            ),
            Some(status) if status >= 500 => {
                KumiError::with_provider(FailureKind::Provider, format!("{name} is having trouble (HTTP {status}); try again."), provider)
            }
            Some(status) => KumiError::with_provider(
                FailureKind::Request,
                format!("{name} turned the request down (HTTP {status}){}.", with(": ")),
                provider,
            ),
            None => KumiError::with_provider(FailureKind::Network, format!("Kumi couldn't reach {name}; check the connection."), provider),
        };
    }
    KumiError::with_provider(FailureKind::Provider, "The model didn't answer; check the model, sign-in and connection.", provider)
}

static STATUS_TEXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(bad request|not found|unprocessable entity|unknown error)$").expect("regex"));

/// The provider's own short explanation of a rejected request (e.g. an unsupported parameter).
fn provider_detail(error: &ApiCallError) -> String {
    let mut detail = error.message.clone();
    if trim(&detail).is_empty() || STATUS_TEXT.is_match(trim(&detail)) {
        if let Ok(body) = serde_json::from_str::<Value>(error.response_body.as_deref().unwrap_or("")) {
            let record = body.as_object().cloned().unwrap_or_default();
            let nested = match record.get("error") {
                Some(Value::Object(error)) => error.get("message"),
                other => other,
            };
            let candidate = [record.get("detail"), nested, record.get("message")]
                .into_iter()
                .flatten()
                .find_map(|value| value.as_str().filter(|text| !trim(text).is_empty()));
            if let Some(candidate) = candidate {
                detail = candidate.to_string();
            }
        }
    }
    clean_detail(&detail)
}

static CONTROL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\x00-\x1f\x7f-\x9f]").expect("regex"));
static BEARER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?-u:\b)Bearer\s+\S+").expect("regex"));
static TOKEN_LIKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9._~+/=-]{32,}").expect("regex"));

/// A server's own words, safe to show: one line, no tokens or anything that looks like one, bounded.
pub fn clean_detail(detail: &str) -> String {
    let spaced = CONTROL.replace_all(detail, " ");
    let unbearered = BEARER.replace_all(&spaced, "Bearer [redacted]");
    let redacted = TOKEN_LIKE.replace_all(&unbearered, "[redacted]");
    head(trim(&redacted), 300)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stream_failure(frame: Value, status: u64) -> LanguageModelError {
        let fields = json!({"message":"failed","statusCode":status,"isRetryable":true,"data":frame});
        LanguageModelError::ProviderStream(fields.as_object().unwrap().clone())
    }

    #[test]
    fn a_plans_limit_is_read_from_a_streams_own_frame() {
        let now = 1_000_000.0;
        let frame =
            json!({"type":"response.failed","response":{"error":{"code":"usage_limit_reached","message":"Limit","resets_in_seconds":60}}});
        let failure = stream_failure(frame, 429);
        assert_eq!(plan_limit(&failure, now), Some(PlanLimit::Reached { resets_at_ms: Some(now + 60_000.0) }));
        assert_eq!(wait_for(&failure, 0, 0, now), Some(Wait::Reset(60_000.0 + RESET_MARGIN_MS)));
        // resets_at (seconds since the epoch) wins over resets_in_seconds.
        let both = json!({"error":{"type":"usage_limit_reached","resets_at":2_000,"resets_in_seconds":60}});
        assert_eq!(plan_limit(&stream_failure(both, 429), now), Some(PlanLimit::Reached { resets_at_ms: Some(2_000_000.0) }));
        // A limit that doesn't say when, with no Retry-After either: nothing to wait for.
        let unsaid = stream_failure(json!({"error":{"type":"usage_limit_reached"}}), 429);
        assert_eq!(wait_for(&unsaid, 0, 0, now), None);
    }

    #[test]
    fn busy_and_down_providers_wait_as_they_ask_or_a_growing_while() {
        let now = 1_000_000.0;
        let down = LanguageModelError::ApiCall(ApiCallError::new("down", "u", None, Some(502)));
        assert_eq!([0, 1, 2].map(|attempt| wait_for(&down, attempt, 0, now)), [750.0, 2250.0, 6750.0].map(|ms| Some(Wait::Again(ms))));
        let refused = LanguageModelError::ApiCall(ApiCallError::new("no", "u", None, Some(400)));
        assert_eq!(wait_for(&refused, 0, 0, now), None);
        // Past the longest wait, none.
        let mut later = ApiCallError::new("busy", "u", None, Some(429));
        later.response_headers = Some([("retry-after".to_string(), "86400".to_string())].into_iter().collect());
        assert_eq!(wait_for(&LanguageModelError::ApiCall(later), 0, 0, now), None);
    }
}
