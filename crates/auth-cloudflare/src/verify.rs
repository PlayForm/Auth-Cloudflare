//! Verify - the Phase-3 live conformance smoke suite, sanitized failure
//! evidence, and the versioned `model-health.json` store (task sa-0).
//!
//! Three cheap, non-destructive checks exercise the OpenAI-compatible Chat
//! Completions surface exactly as the Hermes plugin uses it:
//!
//! 1. **text_completion** - a non-streaming completion must return the exact
//!    string `CF_HERMES_OK` in under 30 seconds (feedback 02
//!    `10-inference` acceptance: `content_exact` + `max_elapsed_ms`).
//! 2. **streaming** - a streaming completion must emit its first valid SSE
//!    event within 20 seconds, deliver at least one content delta, reach a
//!    terminal chunk (`finish_reason` non-null), close without a protocol
//!    error, and never emit a duplicate terminal event (feedback 02
//!    `20-streaming` acceptance). SSE is hand-rolled (line-based `data:`
//!    parsing) because the workspace forbids extra dependencies.
//! 3. **tool_call** - a single call to the harmless `get_project_sentinel`
//!    function with `scope=provider-conformance` exactly once (feedback 02
//!    `30-tool-calling` acceptance).
//!
//! Live gate: **every HTTP-calling function refuses to run unless the
//! environment variable `AUTH_CLOUDFLARE_LIVE_TESTS` is exactly `1`** -
//! `live_tests_enabled()` - returning a typed
//! [`CloudflareError::MissingEnv`] refusal that names the variable. This is
//! the guardrail against accidental paid inference (feedback 02: paid runs
//! are opt-in). `AUTH_CLOUDFLARE_MAX_COST_USD` is an optional *documented*
//! conformance budget: when present, the run report carries
//! `cost_estimate_usd`; the value is reported and documented but **never
//! enforced** - enforcement is the operator's job.
//!
//! Exit-code contract (feedback 02, binding table):
//! - `0` - all three checks passed;
//! - `1` - the live gate is closed (operational refusal);
//! - `2` - credentials missing (resolved by the CLI before this module);
//! - `3` - the live API was unreachable at send time (no HTTP response was
//!   obtained - connect/DNS/TLS/overall-timeout), i.e. a *remote Cloudflare
//!   API failure*, not a conformance verdict;
//! - `6` - the suite ran to completion and at least one check failed
//!   acceptance (non-2xx status or a 200 that failed acceptance), with
//!   sanitized [`FailureEvidence`] persisted.
//!
//! Security contract (feedback 02, CI-tested): the token travels exclusively
//! through `crate::fetch::auth_header`; every error text and every response
//! excerpt is scrubbed with `redact_token`; [`FailureEvidence::new`] caps
//! excerpts at 512 characters; prompts, tool outputs, Authorization headers
//! and tokens are **never** persisted. The health store records only
//! sanitized evidence.
//!
//! Health store layout (under `Config::cache_dir()`):
//! `model-health.json` - `{ version: 1, updated_at, records: { <model_id>:
//! ModelVerification } }`, written atomically (sibling `.tmp` + rename,
//! `0o600`) via `crate::cache::atomic_write`.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cache::atomic_write;
use crate::config::Config;
use crate::error::CloudflareError;
use crate::fetch::auth_header;
use crate::health::{
	CONFORMANCE_SUITE_VERSION, FailureClass, FailureEvidence, ModelVerification, VerificationConfidence,
	VerificationStatus,
};
use crate::tool_loop::ToolLoopOutcome;

/// Env var gating live inference: the suite runs only when this is exactly
/// `"1"` (feedback 02: paid runs are opt-in).
pub const LIVE_TESTS_ENV: &str = "AUTH_CLOUDFLARE_LIVE_TESTS";

/// Optional conformance budget env var. When present (a positive finite
/// number), the run report carries `cost_estimate_usd`; the budget is
/// documented and reported, never enforced.
pub const MAX_COST_ENV: &str = "AUTH_CLOUDFLARE_MAX_COST_USD";

/// File name of the health store, inside `Config::cache_dir()`.
pub const HEALTH_STORE_FILE: &str = "model-health.json";

/// Schema version of the health store file layout. Bump on layout change so
/// older stores are rejected loudly instead of misread.
pub const HEALTH_STORE_VERSION: u32 = 1;

/// The fixed user message for the text and streaming checks (feedback 02
/// `10-inference`).
pub const TEXT_PROMPT: &str = "Reply with exactly: CF_HERMES_OK";

/// The exact content the non-streaming check must deliver.
pub const EXACT_TEXT: &str = "CF_HERMES_OK";

/// The tool-calling check prompt (feedback 02 `30-tool-calling`).
pub const TOOL_PROMPT: &str = "Call get_project_sentinel exactly once with scope=provider-conformance. Do not answer with prose before calling the tool.";

/// The harmless sentinel tool name (feedback 02 `30-tool-calling`).
pub const TOOL_NAME: &str = "get_project_sentinel";

/// The only permitted `scope` argument value.
pub const TOOL_SCOPE: &str = "provider-conformance";

/// Non-streaming acceptance: `elapsed_ms` must stay under this (feedback 02).
pub const TEXT_COMPLETION_MAX_ELAPSED_MS: u64 = 30_000;

/// Streaming acceptance: first valid SSE event within this many ms.
pub const STREAM_FIRST_EVENT_MAX_MS: u64 = 20_000;

/// Overall agent timeout for the non-streaming text check (enforces the
/// 30 s acceptance at the transport level).
pub const TEXT_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Overall agent timeout for the streaming check - the "configured timeout"
/// capping total stream duration.
pub const STREAM_CHECK_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-read timeout for the streaming check (a stalled upstream must not
/// hold the first event past the 20 s acceptance window).
pub const STREAM_FIRST_EVENT_TIMEOUT: Duration = Duration::from_secs(20);

/// Overall agent timeout for the tool-call check.
pub const TOOL_CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// Conservative upper-bound cost estimate for one smoke run (three tiny
/// requests, roughly 80 tokens round-trip at the most expensive in-catalog
/// price). Reported as `cost_estimate_usd` only when `AUTH_CLOUDFLARE_MAX_COST_USD`
/// is set; the budget itself is documented, never enforced.
pub const SMOKE_SUITE_ESTIMATED_COST_USD: f64 = 0.001;

/// The suites this runner knows. `smoke` and `tool-loop` exist in Phase 3;
/// the CLI rejects anything else as a usage error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuiteKind {
	/// Cheap live smoke suite: exact text completion, streaming completion,
	/// one tool call.
	Smoke,
	/// Multi-turn fake-tool conformance loop (feedback 02 suite 40): the
	/// model must read a fixture, run its test, write the patch, and deliver
	/// a final answer within the turn budget.
	ToolLoop,
}

impl SuiteKind {
	/// Parse a `--suite` value; `None` defaults to `smoke`.
	pub fn parse(value: Option<&str>) -> Result<Self, String> {
		match value {
			None | Some("smoke") => Ok(Self::Smoke),
			Some("tool-loop") => Ok(Self::ToolLoop),
			Some(other) => Err(format!("unsupported suite {other}: only 'smoke' and 'tool-loop' are available")),
		}
	}

	/// The suite's machine-readable name (`"smoke"` / `"tool-loop"`).
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Smoke => "smoke",
			Self::ToolLoop => "tool-loop",
		}
	}
}

/// Outcome of one check: passed + elapsed, with sanitized failure evidence
/// when the check failed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CheckOutcome {
	pub passed: bool,
	pub elapsed_ms: u64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub failure: Option<FailureEvidence>,
}

/// The three smoke checks, keyed for the CLI report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ChecksReport {
	pub text_completion: CheckOutcome,
	pub streaming: CheckOutcome,
	pub tool_call: CheckOutcome,
}

/// Full smoke run report (the CLI adds `gate` and `exit_code`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SmokeRunReport {
	pub model_id: String,
	pub suite: String,
	pub status: VerificationStatus,
	pub passed: bool,
	pub checks: ChecksReport,
	pub verification: ModelVerification,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cost_estimate_usd: Option<f64>,
}

/// One parsed SSE event: a JSON `data:` payload, the `[DONE]` sentinel, or a
/// malformed `data:` line (a protocol error).
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
	Data(serde_json::Value),
	Done,
	Malformed(String),
}

/// True when the live gate is open: `AUTH_CLOUDFLARE_LIVE_TESTS` is exactly
/// `"1"`. Pure and testable; every HTTP-calling function consults this first.
pub fn live_tests_enabled() -> bool {
	std::env::var(LIVE_TESTS_ENV).is_ok_and(|value| value == "1")
}

/// The typed refusal returned when the live gate is closed.
pub fn gate_refusal() -> CloudflareError {
	CloudflareError::MissingEnv {
		env_var: LIVE_TESTS_ENV,
		hint: "live tests disabled (set AUTH_CLOUDFLARE_LIVE_TESTS=1 to allow paid inference)".to_string(),
	}
}

/// Optional `AUTH_CLOUDFLARE_MAX_COST_USD` budget: a positive finite number,
/// when present. Reported as `cost_estimate_usd`; never enforced.
pub fn max_cost_usd_env() -> Option<f64> {
	std::env::var(MAX_COST_ENV)
		.ok()
		.and_then(|value| value.trim().parse::<f64>().ok())
		.filter(|value| value.is_finite() && *value > 0.0)
}

/// Run the full smoke suite against the model id **exactly as given** - the
/// catalog is never consulted, so a model absent from the catalog is still
/// verified (the CLI contract: fall back to the id as passed).
///
/// The suite always completes when the three requests could be attempted:
/// per-check failures (non-2xx statuses, acceptance failures, body-read
/// failures) become sanitized evidence, never aborts. The only `Err` paths
/// are the closed live gate (refusal) and a send-phase transport failure
/// (the live API was unreachable - no HTTP response was obtained).
pub fn run_smoke_suite(config: &Config, model_id: &str) -> Result<SmokeRunReport, CloudflareError> {
	gate_check()?;
	let text = check_text_completion(config, model_id)?;
	let streaming = check_streaming_completion(config, model_id)?;
	let tool = check_tool_call(config, model_id)?;
	let verification = aggregate_verification(model_id, &text, &streaming, &tool);
	let passed = text.passed && streaming.passed && tool.passed;
	let report = SmokeRunReport {
		model_id: model_id.to_string(),
		suite: SuiteKind::Smoke.as_str().to_string(),
		status: verification.status,
		passed,
		checks: ChecksReport { text_completion: text, streaming, tool_call: tool },
		verification,
		cost_estimate_usd: if max_cost_usd_env().is_some() {
			Some(SMOKE_SUITE_ESTIMATED_COST_USD)
		} else {
			None
		},
	};
	Ok(report)
}

/// Check (a): non-streaming exact-text completion. Acceptance: content
/// exactly `CF_HERMES_OK`, elapsed under 30 000 ms.
pub fn check_text_completion(config: &Config, model_id: &str) -> Result<CheckOutcome, CloudflareError> {
	gate_check()?;
	let started = Instant::now();
	let request_id = next_request_id();
	let body = serde_json::json!({
		"model": model_id,
		"messages": [{ "role": "user", "content": TEXT_PROMPT }],
		"stream": false,
	});
	let response = send_completion(config, &body, "application/json", TEXT_CHECK_TIMEOUT, None)?;
	let cf_ray = response.header("cf-ray").map(str::to_string);
	let status = response.status();
	let elapsed_ms = started.elapsed().as_millis() as u64;
	if status != 200 {
		return Ok(non_streaming_failure(
			config,
			model_id,
			&request_id,
			status,
			cf_ray,
			elapsed_ms,
			response,
		));
	}
	let body = match response.into_string() {
		Ok(body) => body,
		Err(_) => {
			return Ok(failed_outcome(
				FailureClass::ReadTimeout,
				Some(200),
				cf_ray,
				elapsed_ms,
				model_id,
				&request_id,
				None,
			));
		},
	};
	match inspect_text_completion(&body) {
		Ok(()) if elapsed_ms < TEXT_COMPLETION_MAX_ELAPSED_MS => {
			Ok(CheckOutcome { passed: true, elapsed_ms, failure: None })
		},
		Ok(()) => {
			// The acceptance caps latency; the taxonomy has no dedicated
			// class, so Unknown is the honest verdict (never guess).
			Ok(failed_outcome(
				FailureClass::Unknown,
				Some(200),
				cf_ray,
				elapsed_ms,
				model_id,
				&request_id,
				Some(redact_token(&body, config.api_token().as_ref())),
			))
		},
		Err(class) => Ok(failed_outcome(
			class,
			Some(200),
			cf_ray,
			elapsed_ms,
			model_id,
			&request_id,
			Some(redact_token(&body, config.api_token().as_ref())),
		)),
	}
}

/// Check (b): streaming completion. Acceptance (feedback 02 `20-streaming`):
/// first valid SSE event within 20 s, at least one content delta, a terminal
/// chunk with non-null `finish_reason`, clean close with no protocol error,
/// no duplicate terminal event, total duration under the configured timeout.
pub fn check_streaming_completion(config: &Config, model_id: &str) -> Result<CheckOutcome, CloudflareError> {
	gate_check()?;
	let started = Instant::now();
	let request_id = next_request_id();
	let body = serde_json::json!({
		"model": model_id,
		"messages": [{ "role": "user", "content": TEXT_PROMPT }],
		"stream": true,
	});
	let response = send_completion(
		config,
		&body,
		"text/event-stream",
		STREAM_CHECK_TIMEOUT,
		Some(STREAM_FIRST_EVENT_TIMEOUT),
	)?;
	let cf_ray = response.header("cf-ray").map(str::to_string);
	let status = response.status();
	let elapsed_ms = started.elapsed().as_millis() as u64;
	if status != 200 {
		return Ok(non_streaming_failure(
			config,
			model_id,
			&request_id,
			status,
			cf_ray,
			elapsed_ms,
			response,
		));
	}

	// Hand-rolled SSE: read lines, keep `data:` payloads, parse each as JSON.
	let reader = response.into_reader();
	let mut lines = std::io::BufReader::new(reader).lines();
	let mut events: Vec<StreamEvent> = Vec::new();
	let mut first_event_elapsed_ms: Option<u64> = None;
	let mut last_raw: Option<String> = None;
	let mut read_error: Option<String> = None;
	loop {
		match lines.next() {
			Some(Ok(line)) => {
				let Some(payload) = line.strip_prefix("data:") else { continue };
				let payload = payload.trim();
				last_raw = Some(redact_token(payload, config.api_token().as_ref()));
				if payload == "[DONE]" {
					events.push(StreamEvent::Done);
				} else {
					match serde_json::from_str::<serde_json::Value>(payload) {
						Ok(value) => {
							if first_event_elapsed_ms.is_none() {
								first_event_elapsed_ms = Some(started.elapsed().as_millis() as u64);
							}
							events.push(StreamEvent::Data(value));
						},
						Err(_) => {
							events.push(StreamEvent::Malformed(redact_token(payload, config.api_token().as_ref())));
						},
					}
				}
			},
			Some(Err(error)) => {
				// A mid-stream read failure (timeout/reset) means the stream
				// did not close cleanly - an acceptance failure with evidence,
				// not an abort.
				read_error = Some(redact_token(&error.to_string(), config.api_token().as_ref()));
				break;
			},
			None => break,
		}
	}
	let elapsed_ms = started.elapsed().as_millis() as u64;
	if let Some(message) = read_error {
		return Ok(failed_outcome(
			classify_transport_message(&message),
			Some(200),
			cf_ray,
			elapsed_ms,
			model_id,
			&request_id,
			last_raw,
		));
	}
	match classify_stream_failure(&events, first_event_elapsed_ms, elapsed_ms) {
		None => Ok(CheckOutcome { passed: true, elapsed_ms, failure: None }),
		Some(class) => Ok(failed_outcome(
			class,
			Some(200),
			cf_ray,
			elapsed_ms,
			model_id,
			&request_id,
			last_raw,
		)),
	}
}

/// Check (c): one harmless tool call. Acceptance (feedback 02
/// `30-tool-calling`): exactly one standard tool call, name exactly
/// `get_project_sentinel`, argument JSON parses, `scope` exactly
/// `provider-conformance`.
pub fn check_tool_call(config: &Config, model_id: &str) -> Result<CheckOutcome, CloudflareError> {
	gate_check()?;
	let started = Instant::now();
	let request_id = next_request_id();
	let body = serde_json::json!({
		"model": model_id,
		"messages": [{ "role": "user", "content": TOOL_PROMPT }],
		"stream": false,
		"tools": [{
			"type": "function",
			"function": {
				"name": TOOL_NAME,
				"description": "Return the configured test sentinel. Use this tool before answering.",
				"parameters": {
					"type": "object",
					"properties": {
						"scope": { "type": "string", "enum": [TOOL_SCOPE] }
					},
					"required": ["scope"],
					"additionalProperties": false
				}
			}
		}],
	});
	let response = send_completion(config, &body, "application/json", TOOL_CHECK_TIMEOUT, None)?;
	let cf_ray = response.header("cf-ray").map(str::to_string);
	let status = response.status();
	let elapsed_ms = started.elapsed().as_millis() as u64;
	if status != 200 {
		return Ok(non_streaming_failure(
			config,
			model_id,
			&request_id,
			status,
			cf_ray,
			elapsed_ms,
			response,
		));
	}
	let body = match response.into_string() {
		Ok(body) => body,
		Err(_) => {
			return Ok(failed_outcome(
				FailureClass::ReadTimeout,
				Some(200),
				cf_ray,
				elapsed_ms,
				model_id,
				&request_id,
				None,
			));
		},
	};
	match classify_tool_response(Some(body.as_str())) {
		None => Ok(CheckOutcome { passed: true, elapsed_ms, failure: None }),
		Some(class) => Ok(failed_outcome(
			class,
			Some(200),
			cf_ray,
			elapsed_ms,
			model_id,
			&request_id,
			Some(redact_token(&body, config.api_token().as_ref())),
		)),
	}
}

// ---------------------------------------------------------------------------
// HTTP transport
// ---------------------------------------------------------------------------

/// POST a Chat Completions body and return the response for the caller to
/// classify. Non-2xx statuses are returned as responses too (ureq surfaces
/// them as `Error::Status`) so the caller can read the error envelope and
/// build evidence. The only hard `Err` is a send-phase transport failure
/// (no HTTP response obtained) or a closed live gate.
fn send_completion(
	config: &Config,
	body: &serde_json::Value,
	accept: &str,
	overall_timeout: Duration,
	read_timeout: Option<Duration>,
) -> Result<ureq::Response, CloudflareError> {
	gate_check()?;
	let base_url = config
		.base_url()
		.map_err(|error| CloudflareError::Http(format!("resolve base url: {error}")))?;
	let url = format!("{base_url}/chat/completions");
	let mut builder = ureq::AgentBuilder::new().timeout(overall_timeout);
	if let Some(read_timeout) = read_timeout {
		builder = builder.timeout_read(read_timeout);
	}
	let agent = builder.build();
	let request = agent
		.post(&url)
		.set("Authorization", &auth_header(config.api_token()))
		.set("Accept", accept)
		.set("Content-Type", "application/json");
	match request.send_string(&body.to_string()) {
		Ok(response) => Ok(response),
		Err(ureq::Error::Status(_, response)) => Ok(response),
		Err(transport) => {
			let message = redact_token(&transport.to_string(), config.api_token().as_ref());
			Err(CloudflareError::Http(message))
		},
	}
}

/// Failure path for a non-2xx response: read the error envelope body (best
/// effort), classify by HTTP status + envelope + cf-ray, and build evidence.
fn non_streaming_failure(
	config: &Config,
	model_id: &str,
	request_id: &str,
	status: u16,
	cf_ray: Option<String>,
	elapsed_ms: u64,
	response: ureq::Response,
) -> CheckOutcome {
	let body = match response.into_string() {
		Ok(body) => redact_token(&body, config.api_token().as_ref()),
		Err(_) => String::new(),
	};
	let class = if body.is_empty() {
		FailureClass::Unknown
	} else {
		classify_http_failure(status, Some(body.as_str()), cf_ray.is_some())
	};
	let excerpt = if body.is_empty() { None } else { Some(body) };
	failed_outcome(class, Some(status), cf_ray, elapsed_ms, model_id, request_id, excerpt)
}

/// The live gate: refuse with a typed `MissingEnv` naming
/// `AUTH_CLOUDFLARE_LIVE_TESTS` when it is not exactly `1`.
fn gate_check() -> Result<(), CloudflareError> {
	if live_tests_enabled() { Ok(()) } else { Err(gate_refusal()) }
}

/// Monotonic request id: chrono millis + a process-local counter (no uuid
/// dependency - the workspace forbids new deps).
fn next_request_id() -> String {
	static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
	let sequence = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
	format!("verify-{}-{sequence}", Utc::now().timestamp_millis())
}

/// Build sanitized failure evidence for a failed check.
fn failed_outcome(
	class: FailureClass,
	http_status: Option<u16>,
	cf_ray: Option<String>,
	elapsed_ms: u64,
	model_id: &str,
	request_id: &str,
	excerpt: Option<String>,
) -> CheckOutcome {
	CheckOutcome {
		passed: false,
		elapsed_ms,
		failure: Some(FailureEvidence::new(
			class,
			http_status,
			cf_ray,
			Some(elapsed_ms),
			model_id.to_string(),
			Some(request_id.to_string()),
			excerpt,
		)),
	}
}

// ---------------------------------------------------------------------------
// pure, unit-testable failure classification
// ---------------------------------------------------------------------------

/// Classify a non-2xx (or envelope-carrying 200) response: 401 → AuthRejected,
/// 403 → AuthRejected or AccountNotFound per the Cloudflare error envelope,
/// 429 → RateLimited, 5xx → ProviderServerError, and an envelope with
/// `success: false` plus a cf-ray → CloudflareEdgeError. Everything else is
/// Unknown - never guess a specific class without evidence.
pub fn classify_http_failure(status: u16, body: Option<&str>, has_cf_ray: bool) -> FailureClass {
	match status {
		401 => FailureClass::AuthRejected,
		403 => {
			if account_not_found(body) {
				FailureClass::AccountNotFound
			} else {
				FailureClass::AuthRejected
			}
		},
		429 => FailureClass::RateLimited,
		500..=599 => FailureClass::ProviderServerError,
		_ => {
			if has_cf_ray && envelope_success_false(body) {
				FailureClass::CloudflareEdgeError
			} else {
				FailureClass::Unknown
			}
		},
	}
}

/// True when the 403 envelope names a missing account: Cloudflare error code
/// 9103 (`Account not found`) or a message saying so. Every other 403 is
/// treated as rejected credentials.
fn account_not_found(body: Option<&str>) -> bool {
	let Some(body) = body else { return false };
	let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else { return false };
	let first = value
		.get("errors")
		.and_then(|errors| errors.as_array())
		.and_then(|array| array.first());
	if first.and_then(|f| f.get("code")).and_then(|code| code.as_u64()) == Some(9103) {
		return true;
	}
	let message = first.and_then(|f| f.get("message")).and_then(|m| m.as_str()).unwrap_or("");
	let lower = message.to_lowercase();
	lower.contains("account") && lower.contains("not found")
}

/// True when the body is a Cloudflare envelope with `"success": false`.
fn envelope_success_false(body: Option<&str>) -> bool {
	body.and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok())
		.is_some_and(|value| value.get("success").and_then(|s| s.as_bool()) == Some(false))
}

/// Classify a ureq transport message into the failure taxonomy by keyword:
/// timeouts → ReadTimeout, connection failures → ConnectTimeout, resets →
/// ConnectionReset, TLS → TlsFailure, else Unknown.
pub fn classify_transport_message(message: &str) -> FailureClass {
	let lower = message.to_lowercase();
	if lower.contains("timed out") || lower.contains("timeout") {
		return FailureClass::ReadTimeout;
	}
	// "connection reset" must be checked before the generic "connect" test.
	if lower.contains("reset") {
		return FailureClass::ConnectionReset;
	}
	if lower.contains("connection refused") || lower.contains("connection failed") || lower.contains("connect") {
		return FailureClass::ConnectTimeout;
	}
	if lower.contains("tls") || lower.contains("certificate") {
		return FailureClass::TlsFailure;
	}
	FailureClass::Unknown
}

/// Classify a non-streaming completion body that failed acceptance: empty
/// content → EmptyCompletion, truncated (`finish_reason` "length") →
/// TruncatedCompletion, invalid JSON → InvalidJson, wrong shape →
/// InvalidChatCompletionShape, missing/`null` finish reason →
/// MissingFinishReason, else Unknown.
pub fn classify_completion_failure(body: Option<&str>) -> FailureClass {
	let Some(body) = body else { return FailureClass::EmptyCompletion };
	if body.trim().is_empty() {
		return FailureClass::EmptyCompletion;
	}
	let value = match serde_json::from_str::<serde_json::Value>(body) {
		Ok(value) => value,
		Err(_) => return FailureClass::InvalidJson,
	};
	let Some(first) = value
		.get("choices")
		.and_then(|choices| choices.as_array())
		.and_then(|array| array.first())
	else {
		return FailureClass::InvalidChatCompletionShape;
	};
	let content = match first.get("message").and_then(|message| message.get("content")) {
		Some(serde_json::Value::String(content)) => content.as_str(),
		Some(_) => return FailureClass::InvalidChatCompletionShape,
		None => return FailureClass::EmptyCompletion,
	};
	if content.is_empty() {
		return FailureClass::EmptyCompletion;
	}
	match first.get("finish_reason") {
		Some(serde_json::Value::String(reason)) if reason == "length" => FailureClass::TruncatedCompletion,
		Some(serde_json::Value::Null) | None => FailureClass::MissingFinishReason,
		_ => FailureClass::Unknown,
	}
}

/// Pure acceptance for the non-streaming text check: content must be exactly
/// `CF_HERMES_OK` in a well-formed completion. Returns the failure class, or
/// `Ok(())` when the content criterion holds (latency is checked by the
/// caller).
pub fn inspect_text_completion(body: &str) -> Result<(), FailureClass> {
	let value: serde_json::Value = serde_json::from_str(body).map_err(|_| FailureClass::InvalidJson)?;
	let first = value
		.get("choices")
		.and_then(|choices| choices.as_array())
		.and_then(|array| array.first())
		.ok_or(FailureClass::InvalidChatCompletionShape)?;
	let content = match first.get("message").and_then(|message| message.get("content")) {
		Some(serde_json::Value::String(content)) => content.as_str(),
		Some(_) => return Err(FailureClass::InvalidChatCompletionShape),
		None => return Err(FailureClass::EmptyCompletion),
	};
	if content.is_empty() {
		return Err(FailureClass::EmptyCompletion);
	}
	if content != EXACT_TEXT {
		return Err(FailureClass::Unknown);
	}
	Ok(())
}

/// Classify a tool-call response that failed acceptance: no tool call →
/// NoToolCall, wrong name → InvalidToolName, unparsable/wrong arguments →
/// InvalidToolArguments, more than one call → DuplicateToolCall, malformed
/// envelope → InvalidJson/InvalidChatCompletionShape. `None` means the
/// response is a valid single `get_project_sentinel(scope=provider-conformance)`
/// call - `FailureClass::Unknown` must never be conflated with "valid".
pub fn classify_tool_response(body: Option<&str>) -> Option<FailureClass> {
	let Some(body) = body else { return Some(FailureClass::NoToolCall) };
	let value: serde_json::Value = match serde_json::from_str(body) {
		Ok(value) => value,
		Err(_) => return Some(FailureClass::InvalidJson),
	};
	let Some(first) = value
		.get("choices")
		.and_then(|choices| choices.as_array())
		.and_then(|array| array.first())
	else {
		return Some(FailureClass::InvalidChatCompletionShape);
	};
	let Some(calls) = first
		.get("message")
		.and_then(|message| message.get("tool_calls"))
		.and_then(|calls| calls.as_array())
	else {
		return Some(FailureClass::NoToolCall);
	};
	if calls.is_empty() {
		return Some(FailureClass::NoToolCall);
	}
	if calls.len() > 1 {
		return Some(FailureClass::DuplicateToolCall);
	}
	let call = &calls[0];
	let name = call
		.get("function")
		.and_then(|function| function.get("name"))
		.and_then(|name| name.as_str())
		.unwrap_or("");
	if name != TOOL_NAME {
		return Some(FailureClass::InvalidToolName);
	}
	let args_raw = call
		.get("function")
		.and_then(|function| function.get("arguments"))
		.and_then(|arguments| arguments.as_str())
		.unwrap_or("");
	let args: serde_json::Value = match serde_json::from_str(args_raw) {
		Ok(args) => args,
		Err(_) => return Some(FailureClass::InvalidToolArguments),
	};
	if args.get("scope").and_then(|scope| scope.as_str()) != Some(TOOL_SCOPE) {
		return Some(FailureClass::InvalidToolArguments);
	}
	None
}

/// Pure streaming acceptance over a fully read event stream. `None` means
/// every acceptance criterion holds: first valid event within 20 s, at least
/// one content delta, exactly one terminal chunk, no protocol error, total
/// duration under the configured timeout.
pub fn classify_stream_failure(
	events: &[StreamEvent],
	first_event_elapsed_ms: Option<u64>,
	total_elapsed_ms: u64,
) -> Option<FailureClass> {
	// Protocol errors take precedence over everything else.
	for event in events {
		if let StreamEvent::Malformed(_) = event {
			return Some(FailureClass::InvalidSseEvent);
		}
	}
	if total_elapsed_ms >= STREAM_CHECK_TIMEOUT.as_millis() as u64 {
		return Some(FailureClass::ReadTimeout);
	}
	match first_event_elapsed_ms {
		None => return Some(FailureClass::StreamIdleTimeout),
		Some(elapsed) if elapsed > STREAM_FIRST_EVENT_MAX_MS => return Some(FailureClass::StreamIdleTimeout),
		Some(_) => {},
	}
	let mut content_delta = false;
	let mut terminal_seen = false;
	for event in events {
		match event {
			StreamEvent::Data(value) => {
				let Some(first) = value
					.get("choices")
					.and_then(|choices| choices.as_array())
					.and_then(|array| array.first())
				else {
					// A data event without a completion-shaped choices array
					// is a protocol error for this acceptance.
					return Some(FailureClass::InvalidSseEvent);
				};
				if let Some(delta) = first
					.get("delta")
					.and_then(|delta| delta.get("content"))
					.and_then(|content| content.as_str())
				{
					if !delta.is_empty() {
						content_delta = true;
					}
				}
				if let Some(serde_json::Value::String(_)) = first.get("finish_reason") {
					if terminal_seen {
						// A second terminal chunk is a protocol violation.
						return Some(FailureClass::InvalidSseEvent);
					}
					terminal_seen = true;
				}
			},
			StreamEvent::Done => break,
			StreamEvent::Malformed(_) => continue,
		}
	}
	if !content_delta {
		return Some(FailureClass::EmptyCompletion);
	}
	if !terminal_seen {
		return Some(FailureClass::MissingFinishReason);
	}
	None
}

// ---------------------------------------------------------------------------
// aggregation into ModelVerification
// ---------------------------------------------------------------------------

/// Aggregate the three check outcomes into the canonical
/// [`ModelVerification`] (feedback 02). Status formula: 0 failures →
/// Passing, 1 failure → Degraded, 2+ failures → Failing. Each check counts
/// as one run (`total_runs` = 3), so `delivery_success_rate()` is the
/// fraction of smoke checks that delivered - the picker's delivery metric.
/// `last_failure` is the first failed check's evidence (text, stream, tool
/// order); failure counters map from [`FailureClass`].
pub fn aggregate_verification(
	model_id: &str,
	text: &CheckOutcome,
	streaming: &CheckOutcome,
	tool: &CheckOutcome,
) -> ModelVerification {
	let outcomes = [text, streaming, tool];
	let failures = outcomes.iter().filter(|outcome| !outcome.passed).count();
	let status = match failures {
		0 => VerificationStatus::Passing,
		1 => VerificationStatus::Degraded,
		_ => VerificationStatus::Failing,
	};
	let (
		timeout_failures,
		transport_failures,
		provider_5xx_failures,
		malformed_response_failures,
		malformed_tool_call_failures,
	) = failure_counter_map(&outcomes);
	let mut latencies = [text.elapsed_ms, streaming.elapsed_ms, tool.elapsed_ms];
	latencies.sort_unstable();
	let last_failure = outcomes
		.iter()
		.find(|outcome| !outcome.passed)
		.and_then(|outcome| outcome.failure.clone());
	ModelVerification {
		model_id: model_id.to_string(),
		latest_run_at: Some(Utc::now()),
		expires_at: None,
		suite_version: CONFORMANCE_SUITE_VERSION.to_string(),
		runner_version: env!("CARGO_PKG_VERSION").to_string(),
		status,
		agent_eligible: true,
		confidence: VerificationConfidence::SmokeTested,
		total_runs: 3,
		successful_runs: 3 - failures as u32,
		text_completion_success_rate: Some(rate_of(text.passed)),
		stream_completion_success_rate: Some(rate_of(streaming.passed)),
		single_tool_success_rate: Some(rate_of(tool.passed)),
		multi_turn_tool_success_rate: None,
		structured_output_success_rate: None,
		median_latency_ms: Some(latencies[1]),
		p95_latency_ms: None,
		total_failures: failures as u32,
		timeout_failures,
		transport_failures,
		provider_5xx_failures,
		malformed_response_failures,
		malformed_tool_call_failures,
		tool_loop_failures: 0,
		last_failure,
	}
}

/// 1.0 for a passed check, 0.0 for a failed one (each check is one run).
fn rate_of(passed: bool) -> f64 {
	if passed { 1.0 } else { 0.0 }
}

/// Aggregate one tool-loop outcome into a [`ModelVerification`] (feedback 02
/// suite 40). The loop counts as one run: `total_runs` = 1,
/// `multi_turn_tool_success_rate` is 1.0 when the loop converged, 0.0
/// otherwise; a non-converged loop is `Failing` with `tool_loop_failures` =
/// 1 and the loop's own [`FailureClass`] as sanitized evidence (no HTTP
/// status, no excerpt - the loop carries no response body).
pub fn verification_from_tool_loop(model_id: &str, outcome: &ToolLoopOutcome) -> ModelVerification {
	let converged = outcome.converged;
	let status = if converged {
		VerificationStatus::Passing
	} else {
		VerificationStatus::Failing
	};
	let last_failure = outcome
		.failure_class
		.map(|class| FailureEvidence::new(class, None, None, None, model_id, None, None));
	ModelVerification {
		model_id: model_id.to_string(),
		latest_run_at: Some(Utc::now()),
		expires_at: None,
		suite_version: CONFORMANCE_SUITE_VERSION.to_string(),
		runner_version: env!("CARGO_PKG_VERSION").to_string(),
		status,
		agent_eligible: true,
		confidence: VerificationConfidence::SmokeTested,
		total_runs: 1,
		successful_runs: if converged { 1 } else { 0 },
		text_completion_success_rate: None,
		stream_completion_success_rate: None,
		single_tool_success_rate: None,
		multi_turn_tool_success_rate: Some(if converged { 1.0 } else { 0.0 }),
		structured_output_success_rate: None,
		median_latency_ms: None,
		p95_latency_ms: None,
		total_failures: if converged { 0 } else { 1 },
		timeout_failures: 0,
		transport_failures: 0,
		provider_5xx_failures: 0,
		malformed_response_failures: 0,
		malformed_tool_call_failures: 0,
		tool_loop_failures: if converged { 0 } else { 1 },
		last_failure,
	}
}

/// Map failed-check evidence classes onto the five counter buckets.
fn failure_counter_map(outcomes: &[&CheckOutcome]) -> (u32, u32, u32, u32, u32) {
	let mut timeout = 0u32;
	let mut transport = 0u32;
	let mut fivexx = 0u32;
	let mut malformed_response = 0u32;
	let mut malformed_tool = 0u32;
	for outcome in outcomes {
		let Some(class) = outcome.failure.as_ref().map(|evidence| evidence.failure_class) else { continue };
		match class {
			FailureClass::ReadTimeout | FailureClass::StreamIdleTimeout | FailureClass::ConnectTimeout => timeout += 1,
			FailureClass::ConnectionReset | FailureClass::TlsFailure => transport += 1,
			FailureClass::ProviderServerError => fivexx += 1,
			FailureClass::EmptyCompletion
			| FailureClass::TruncatedCompletion
			| FailureClass::InvalidJson
			| FailureClass::InvalidChatCompletionShape
			| FailureClass::InvalidSseEvent
			| FailureClass::MissingFinishReason => malformed_response += 1,
			FailureClass::NoToolCall
			| FailureClass::InvalidToolName
			| FailureClass::InvalidToolArguments
			| FailureClass::DuplicateToolCall => malformed_tool += 1,
			_ => {},
		}
	}
	(timeout, transport, fivexx, malformed_response, malformed_tool)
}

// ---------------------------------------------------------------------------
// model-health.json store (atomic, versioned, account-scoped)
// ---------------------------------------------------------------------------

/// The versioned health store: one `ModelVerification` per model id, keyed
/// by `model_id`. Lives at `Config::cache_dir()/model-health.json`, written
/// atomically (sibling `.tmp` + rename, `0o600`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HealthStore {
	/// File layout version (`HEALTH_STORE_VERSION`).
	pub version: u32,
	/// Last write time (RFC 3339).
	pub updated_at: DateTime<Utc>,
	/// Verification records keyed by model id.
	pub records: BTreeMap<String, ModelVerification>,
}

impl HealthStore {
	/// An empty store at the current version.
	pub fn new() -> Self {
		Self { version: HEALTH_STORE_VERSION, updated_at: Utc::now(), records: BTreeMap::new() }
	}

	/// Insert or replace one model's verification record and bump
	/// `updated_at`. Other models' records are preserved.
	pub fn upsert(&mut self, verification: ModelVerification) {
		self.records.insert(verification.model_id.clone(), verification);
		self.updated_at = Utc::now();
	}

	/// Read one model's record.
	pub fn get(&self, model_id: &str) -> Option<&ModelVerification> {
		self.records.get(model_id)
	}
}

impl Default for HealthStore {
	/// [`Self::new`] - a store with no records is the natural default state.
	fn default() -> Self {
		Self::new()
	}
}

/// Load the health store from `dir/model-health.json`. A missing file is an
/// empty store; a present-but-corrupt file or an unsupported schema version
/// is a typed error (never a panic).
pub fn load_health_store(dir: &Path) -> Result<HealthStore, CloudflareError> {
	let path = dir.join(HEALTH_STORE_FILE);
	if !path.exists() {
		return Ok(HealthStore::new());
	}
	let raw = std::fs::read_to_string(&path)
		.map_err(|error| CloudflareError::Http(format!("read {}: {error}", path.display())))?;
	let store: HealthStore = serde_json::from_str(&raw)
		.map_err(|error| CloudflareError::Http(format!("parse {}: {error}", path.display())))?;
	if store.version != HEALTH_STORE_VERSION {
		return Err(CloudflareError::Http(format!(
			"{} has unsupported schema version {} (expected {HEALTH_STORE_VERSION})",
			path.display(),
			store.version
		)));
	}
	Ok(store)
}

/// Persist the health store atomically under `dir` (create dirs, sibling
/// `.tmp` + rename, `0o600`). Never leaves a half-written store behind.
pub fn save_health_store(dir: &Path, store: &HealthStore) -> Result<(), CloudflareError> {
	std::fs::create_dir_all(dir)
		.map_err(|error| CloudflareError::Http(format!("create {}: {error}", dir.display())))?;
	let json = serde_json::to_string_pretty(store)
		.map_err(|error| CloudflareError::Http(format!("serialize {HEALTH_STORE_FILE}: {error}")))?;
	atomic_write(&dir.join(HEALTH_STORE_FILE), &json)
}

/// Read-modify-write upsert of one verification record after a run: load the
/// existing store (preserving every other model's record), upsert, save.
pub fn save_verification(dir: &Path, verification: &ModelVerification) -> Result<(), CloudflareError> {
	let mut store = load_health_store(dir)?;
	store.upsert(verification.clone());
	save_health_store(dir, &store)
}

// ---------------------------------------------------------------------------
// token scrubbing (mirrors fetch.rs; kept private to this module)
// ---------------------------------------------------------------------------

/// Replace the token with `<redacted>` in any text that could reach an error
/// string or an evidence excerpt. The token value never survives.
fn redact_token(text: &str, token: &str) -> String {
	if token.is_empty() {
		text.to_string()
	} else {
		text.replace(token, "<redacted>")
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::config::ConfigBuilder;

	/// Synthetic Cloudflare-shaped account id (32 hex digits) - never real.
	const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
	/// Synthetic token - never a real credential.
	const TOKEN: &str = "cfut_test_synthetic_token_0001";
	/// First synthetic model id.
	const MODEL_A: &str = "@cf/deepseek-ai/deepseek-v4-flash-0731";
	/// Second synthetic model id.
	const MODEL_B: &str = "@cf/zai-org/glm-5.3-flash";

	/// Every env var this module reads, saved/restored for isolation.
	const ALL_VARS: &[&str] = &[LIVE_TESTS_ENV, MAX_COST_ENV];

	/// `std::env` is process-global and tests run in parallel - serialize env
	/// mutation through a static mutex and restore prior values after.
	static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	fn with_env<F, R>(vars: &[(&str, Option<&str>)], f: F) -> R
	where
		F: FnOnce() -> R,
	{
		let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
		let saved: Vec<(String, Option<String>)> = ALL_VARS
			.iter()
			.map(|key| ((*key).to_string(), std::env::var(key).ok()))
			.collect();
		for key in ALL_VARS {
			std::env::remove_var(key);
		}
		for (key, value) in vars {
			match value {
				Some(value) => std::env::set_var(key, value),
				None => std::env::remove_var(key),
			}
		}
		let result = f();
		for (key, value) in saved {
			match value {
				Some(value) => std::env::set_var(&key, value),
				None => std::env::remove_var(&key),
			}
		}
		result
	}

	/// Unique scratch dir per test - tests run in parallel.
	fn scratch_dir(name: &str) -> std::path::PathBuf {
		std::env::temp_dir().join(format!("auth-cloudflare-verify-test-{}-{name}", std::process::id()))
	}

	/// A config that resolves without touching the environment beyond the
	/// vars `with_env` controls.
	fn test_config() -> Config {
		ConfigBuilder::new()
			.account_id(ACCOUNT)
			.api_token(TOKEN)
			.cache_dir(scratch_dir("cfg"))
			.build()
			.expect("test config resolves")
	}

	/// A minimal valid verification record for store tests.
	fn verification_record(model_id: &str, status: VerificationStatus) -> ModelVerification {
		ModelVerification {
			model_id: model_id.to_string(),
			latest_run_at: Some(Utc::now()),
			expires_at: None,
			suite_version: CONFORMANCE_SUITE_VERSION.to_string(),
			runner_version: "test".to_string(),
			status,
			agent_eligible: true,
			confidence: VerificationConfidence::SmokeTested,
			total_runs: 3,
			successful_runs: 3,
			text_completion_success_rate: Some(1.0),
			stream_completion_success_rate: Some(1.0),
			single_tool_success_rate: Some(1.0),
			multi_turn_tool_success_rate: None,
			structured_output_success_rate: None,
			median_latency_ms: Some(100),
			p95_latency_ms: None,
			total_failures: 0,
			timeout_failures: 0,
			transport_failures: 0,
			provider_5xx_failures: 0,
			malformed_response_failures: 0,
			malformed_tool_call_failures: 0,
			tool_loop_failures: 0,
			last_failure: None,
		}
	}

	fn passed(elapsed_ms: u64) -> CheckOutcome {
		CheckOutcome { passed: true, elapsed_ms, failure: None }
	}

	fn failed(elapsed_ms: u64, class: FailureClass) -> CheckOutcome {
		CheckOutcome {
			passed: false,
			elapsed_ms,
			failure: Some(FailureEvidence::new(
				class,
				Some(200),
				Some("ray-test".to_string()),
				Some(elapsed_ms),
				MODEL_A,
				Some("req-test".to_string()),
				Some("excerpt".to_string()),
			)),
		}
	}

	/// A streaming `data:` event with an optional content delta and optional
	/// string finish reason.
	fn data_event(delta: Option<&str>, finish: Option<&str>) -> StreamEvent {
		let mut delta_object = serde_json::Map::new();
		if let Some(delta) = delta {
			delta_object.insert("content".to_string(), serde_json::json!(delta));
		}
		let mut choice = serde_json::Map::new();
		choice.insert("delta".to_string(), serde_json::Value::Object(delta_object));
		if let Some(finish) = finish {
			choice.insert("finish_reason".to_string(), serde_json::json!(finish));
		}
		StreamEvent::Data(serde_json::json!({ "choices": [choice] }))
	}

	// ------------------------------------------------------------------
	// live gate
	// ------------------------------------------------------------------

	#[test]
	fn live_gate_requires_exactly_one() {
		with_env(&[(LIVE_TESTS_ENV, Some("1"))], || {
			assert!(live_tests_enabled(), "exactly '1' opens the gate")
		});
		with_env(&[(LIVE_TESTS_ENV, Some("0"))], || assert!(!live_tests_enabled()));
		with_env(&[(LIVE_TESTS_ENV, Some("yes"))], || assert!(!live_tests_enabled()));
		with_env(&[(LIVE_TESTS_ENV, Some(" 1 "))], || {
			assert!(!live_tests_enabled(), "no trimming - exactly '1'")
		});
		with_env(&[(LIVE_TESTS_ENV, None)], || assert!(!live_tests_enabled()));
	}

	#[test]
	fn gate_closed_refuses_every_http_function_without_network() {
		with_env(&[(LIVE_TESTS_ENV, None)], || {
			let config = test_config();
			let error = run_smoke_suite(&config, MODEL_A).unwrap_err();
			assert!(
				matches!(error, CloudflareError::MissingEnv { env_var: LIVE_TESTS_ENV, .. }),
				"refusal must name AUTH_CLOUDFLARE_LIVE_TESTS: {error}"
			);
			assert!(error.to_string().contains("AUTH_CLOUDFLARE_LIVE_TESTS=1"));
			assert!(
				matches!(
					check_text_completion(&config, MODEL_A).unwrap_err(),
					CloudflareError::MissingEnv { env_var: LIVE_TESTS_ENV, .. }
				),
				"every HTTP-calling function gates first"
			);
			assert!(matches!(
				check_streaming_completion(&config, MODEL_A).unwrap_err(),
				CloudflareError::MissingEnv { env_var: LIVE_TESTS_ENV, .. }
			));
			assert!(matches!(
				check_tool_call(&config, MODEL_A).unwrap_err(),
				CloudflareError::MissingEnv { env_var: LIVE_TESTS_ENV, .. }
			));
		});
	}

	// ------------------------------------------------------------------
	// pure failure classification
	// ------------------------------------------------------------------

	#[test]
	fn http_401_and_403_classify_auth_rejected_or_account_not_found() {
		assert_eq!(
			classify_http_failure(401, Some(r#"{"success":false,"errors":[{"code":9109}]}"#), false),
			FailureClass::AuthRejected
		);
		// Cloudflare code 9103 = Account not found.
		assert_eq!(
			classify_http_failure(
				403,
				Some(r#"{"success":false,"errors":[{"code":9103,"message":"Account not found"}]}"#),
				true
			),
			FailureClass::AccountNotFound
		);
		// Any other 403 is rejected credentials.
		assert_eq!(
			classify_http_failure(403, Some(r#"{"success":false,"errors":[{"code":10000}]}"#), false),
			FailureClass::AuthRejected
		);
		assert_eq!(classify_http_failure(403, None, false), FailureClass::AuthRejected);
	}

	#[test]
	fn http_429_5xx_and_edge_envelope_classify() {
		assert_eq!(
			classify_http_failure(429, Some("rate limited"), false),
			FailureClass::RateLimited
		);
		for status in [500u16, 502, 503, 504] {
			assert_eq!(
				classify_http_failure(status, Some("boom"), false),
				FailureClass::ProviderServerError
			);
		}
		// success:false envelope + cf-ray on a 200 -> edge error.
		assert_eq!(
			classify_http_failure(200, Some(r#"{"success":false,"errors":[]}"#), true),
			FailureClass::CloudflareEdgeError
		);
		// ... but only with a cf-ray, and only with a success:false envelope.
		assert_eq!(
			classify_http_failure(200, Some(r#"{"success":false,"errors":[]}"#), false),
			FailureClass::Unknown
		);
		assert_eq!(
			classify_http_failure(200, Some(r#"{"success":true}"#), true),
			FailureClass::Unknown
		);
		assert_eq!(classify_http_failure(400, Some("bad request"), false), FailureClass::Unknown);
	}

	#[test]
	fn transport_messages_classify_by_keyword() {
		assert_eq!(
			classify_transport_message("request timed out after 30s"),
			FailureClass::ReadTimeout
		);
		assert_eq!(classify_transport_message("timed out"), FailureClass::ReadTimeout);
		assert_eq!(classify_transport_message("connection refused"), FailureClass::ConnectTimeout);
		assert_eq!(classify_transport_message("connection failed"), FailureClass::ConnectTimeout);
		// "reset" wins over the generic "connect" test.
		assert_eq!(
			classify_transport_message("connection reset by peer"),
			FailureClass::ConnectionReset
		);
		assert_eq!(classify_transport_message("tls handshake failed"), FailureClass::TlsFailure);
		assert_eq!(
			classify_transport_message("certificate verify failed"),
			FailureClass::TlsFailure
		);
		assert_eq!(classify_transport_message("weird mystery error"), FailureClass::Unknown);
	}

	#[test]
	fn completion_failure_classification_covers_the_taxonomy() {
		assert_eq!(classify_completion_failure(None), FailureClass::EmptyCompletion);
		assert_eq!(classify_completion_failure(Some("")), FailureClass::EmptyCompletion);
		assert_eq!(
			classify_completion_failure(Some(r#"{"choices":[{"message":{"content":""}}]}"#)),
			FailureClass::EmptyCompletion
		);
		assert_eq!(classify_completion_failure(Some("not json")), FailureClass::InvalidJson);
		assert_eq!(
			classify_completion_failure(Some(r#"{"choices":[]}"#)),
			FailureClass::InvalidChatCompletionShape
		);
		assert_eq!(
			classify_completion_failure(Some(r#"{"choices":[{"message":{"content":"hi"},"finish_reason":null}]}"#)),
			FailureClass::MissingFinishReason
		);
		assert_eq!(
			classify_completion_failure(Some(r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"length"}]}"#)),
			FailureClass::TruncatedCompletion
		);
		assert_eq!(
			classify_completion_failure(Some(r#"{"choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]}"#)),
			FailureClass::Unknown
		);
	}

	#[test]
	fn text_completion_acceptance_is_exact() {
		let ok = format!(r#"{{"choices":[{{"message":{{"content":"{EXACT_TEXT}"}},"finish_reason":"stop"}}]}}"#);
		assert_eq!(inspect_text_completion(&ok), Ok(()));
		assert_eq!(
			inspect_text_completion(r#"{"choices":[{"message":{"content":""}}]}"#),
			Err(FailureClass::EmptyCompletion)
		);
		assert_eq!(
			inspect_text_completion(r#"{"choices":[{"message":{"content":"WRONG_ANSWER"}}]}"#),
			Err(FailureClass::Unknown)
		);
		assert_eq!(inspect_text_completion("not json"), Err(FailureClass::InvalidJson));
		assert_eq!(
			inspect_text_completion(r#"{"choices":[]}"#),
			Err(FailureClass::InvalidChatCompletionShape)
		);
	}

	#[test]
	fn tool_response_classification_covers_the_taxonomy() {
		let valid = r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_project_sentinel","arguments":"{\"scope\":\"provider-conformance\"}"}}]}}]}"#;
		assert_eq!(
			classify_tool_response(Some(valid)),
			None,
			"a valid single call is not a failure"
		);

		let no_call = r#"{"choices":[{"message":{"content":"no tool call"}}]}"#;
		assert_eq!(classify_tool_response(Some(no_call)), Some(FailureClass::NoToolCall));
		assert_eq!(classify_tool_response(None), Some(FailureClass::NoToolCall));

		let wrong_name =
			r#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"other_tool","arguments":"{}"}}]}}]}"#;
		assert_eq!(classify_tool_response(Some(wrong_name)), Some(FailureClass::InvalidToolName));

		let bad_args = r#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"get_project_sentinel","arguments":"not json"}}]}}]}"#;
		assert_eq!(classify_tool_response(Some(bad_args)), Some(FailureClass::InvalidToolArguments));

		let wrong_scope = r#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"get_project_sentinel","arguments":"{\"scope\":\"other\"}"}}]}}]}"#;
		assert_eq!(
			classify_tool_response(Some(wrong_scope)),
			Some(FailureClass::InvalidToolArguments)
		);

		let duplicate = r#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"get_project_sentinel","arguments":"{}"}},{"function":{"name":"get_project_sentinel","arguments":"{}"}}]}}]}"#;
		assert_eq!(classify_tool_response(Some(duplicate)), Some(FailureClass::DuplicateToolCall));

		assert_eq!(classify_tool_response(Some("not json")), Some(FailureClass::InvalidJson));
		assert_eq!(
			classify_tool_response(Some(r#"{"no":"choices"}"#)),
			Some(FailureClass::InvalidChatCompletionShape)
		);
	}

	#[test]
	fn stream_acceptance_passes_a_clean_stream() {
		let events = vec![
			data_event(Some("CF_HERMES"), None),
			data_event(Some("_OK"), None),
			data_event(None, Some("stop")),
		];
		assert_eq!(classify_stream_failure(&events, Some(300), 1_500), None);
	}

	#[test]
	fn stream_acceptance_rejects_protocol_and_terminal_errors() {
		let malformed = vec![
			StreamEvent::Malformed("garbage".to_string()),
			data_event(Some("x"), Some("stop")),
		];
		assert_eq!(
			classify_stream_failure(&malformed, Some(100), 500),
			Some(FailureClass::InvalidSseEvent)
		);

		let no_terminal = vec![data_event(Some("x"), None)];
		assert_eq!(
			classify_stream_failure(&no_terminal, Some(100), 500),
			Some(FailureClass::MissingFinishReason)
		);

		let no_delta = vec![data_event(None, Some("stop"))];
		assert_eq!(
			classify_stream_failure(&no_delta, Some(100), 500),
			Some(FailureClass::EmptyCompletion)
		);

		let duplicate_terminal = vec![
			data_event(Some("x"), None),
			data_event(None, Some("stop")),
			data_event(None, Some("stop")),
		];
		assert_eq!(
			classify_stream_failure(&duplicate_terminal, Some(100), 500),
			Some(FailureClass::InvalidSseEvent),
			"a second terminal chunk is a protocol violation"
		);

		let done_without_terminal = vec![data_event(Some("x"), None), StreamEvent::Done];
		assert_eq!(
			classify_stream_failure(&done_without_terminal, Some(100), 500),
			Some(FailureClass::MissingFinishReason)
		);
	}

	#[test]
	fn stream_acceptance_enforces_timing() {
		let happy = vec![data_event(Some("x"), None), data_event(None, Some("stop"))];
		// No valid event at all -> StreamIdleTimeout.
		assert_eq!(classify_stream_failure(&[], None, 1_000), Some(FailureClass::StreamIdleTimeout));
		// First event past 20 s -> StreamIdleTimeout.
		assert_eq!(
			classify_stream_failure(&happy, Some(STREAM_FIRST_EVENT_MAX_MS + 1), STREAM_FIRST_EVENT_MAX_MS + 1),
			Some(FailureClass::StreamIdleTimeout)
		);
		// Total duration at/over the configured timeout -> ReadTimeout.
		assert_eq!(
			classify_stream_failure(&happy, Some(100), STREAM_CHECK_TIMEOUT.as_millis() as u64),
			Some(FailureClass::ReadTimeout)
		);
	}

	// ------------------------------------------------------------------
	// aggregation
	// ------------------------------------------------------------------

	#[test]
	fn all_passing_checks_aggregate_to_passing() {
		let verification = aggregate_verification(MODEL_A, &passed(100), &passed(200), &passed(150));
		assert_eq!(verification.status, VerificationStatus::Passing);
		assert_eq!(verification.confidence, VerificationConfidence::SmokeTested);
		assert!(verification.agent_eligible);
		assert_eq!(verification.suite_version, CONFORMANCE_SUITE_VERSION);
		assert_eq!(verification.runner_version, env!("CARGO_PKG_VERSION"));
		assert_eq!(verification.total_runs, 3);
		assert_eq!(verification.successful_runs, 3);
		assert_eq!(verification.total_failures, 0);
		assert_eq!(verification.text_completion_success_rate, Some(1.0));
		assert_eq!(verification.stream_completion_success_rate, Some(1.0));
		assert_eq!(verification.single_tool_success_rate, Some(1.0));
		assert_eq!(verification.median_latency_ms, Some(150), "median of 100/200/150");
		assert_eq!(verification.last_failure, None);
	}

	#[test]
	fn one_failure_is_degraded_with_evidence_and_counters() {
		let verification =
			aggregate_verification(MODEL_A, &failed(30_000, FailureClass::ReadTimeout), &passed(200), &passed(150));
		assert_eq!(verification.status, VerificationStatus::Degraded);
		assert_eq!(verification.successful_runs, 2);
		assert_eq!(verification.total_failures, 1);
		assert_eq!(verification.timeout_failures, 1);
		assert_eq!(verification.transport_failures, 0);
		assert_eq!(verification.text_completion_success_rate, Some(0.0));
		let evidence = verification.last_failure.expect("last_failure set");
		assert_eq!(evidence.failure_class, FailureClass::ReadTimeout);
		assert_eq!(evidence.model_id, MODEL_A);
	}

	#[test]
	fn two_failures_are_failing() {
		let verification = aggregate_verification(
			MODEL_A,
			&failed(100, FailureClass::EmptyCompletion),
			&failed(200, FailureClass::MissingFinishReason),
			&passed(150),
		);
		assert_eq!(verification.status, VerificationStatus::Failing);
		assert_eq!(verification.successful_runs, 1);
		assert_eq!(verification.malformed_response_failures, 2);
	}

	#[test]
	fn failure_counters_map_classes_to_buckets() {
		let verification = aggregate_verification(
			MODEL_A,
			&failed(100, FailureClass::ProviderServerError),
			&failed(200, FailureClass::ConnectionReset),
			&failed(150, FailureClass::InvalidToolArguments),
		);
		assert_eq!(verification.status, VerificationStatus::Failing);
		assert_eq!(verification.provider_5xx_failures, 1);
		assert_eq!(verification.transport_failures, 1);
		assert_eq!(verification.malformed_tool_call_failures, 1);
		assert_eq!(verification.timeout_failures, 0);
		assert_eq!(verification.malformed_response_failures, 0);
	}

	#[test]
	fn tool_failures_count_as_malformed_tool_calls() {
		let verification =
			aggregate_verification(MODEL_A, &passed(100), &passed(200), &failed(150, FailureClass::NoToolCall));
		assert_eq!(verification.status, VerificationStatus::Degraded);
		assert_eq!(verification.malformed_tool_call_failures, 1);
		assert_eq!(verification.single_tool_success_rate, Some(0.0));
	}

	// ------------------------------------------------------------------
	// health store
	// ------------------------------------------------------------------

	#[test]
	fn health_store_roundtrip_and_atomic_write_leaves_no_tmp() {
		let dir = scratch_dir("roundtrip");
		let _ = std::fs::remove_dir_all(&dir);
		let mut store = HealthStore::new();
		store.upsert(verification_record(MODEL_A, VerificationStatus::Passing));
		save_health_store(&dir, &store).expect("save");
		let loaded = load_health_store(&dir).expect("load");
		// `ModelVerification` deliberately has no `PartialEq` (health.rs), so
		// compare through serialization.
		assert_eq!(
			serde_json::to_value(&loaded.records).expect("records serialize"),
			serde_json::to_value(&store.records).expect("records serialize")
		);
		assert_eq!(loaded.version, HEALTH_STORE_VERSION);
		// The atomic write leaves no stray temp file behind.
		assert!(!dir.join("model-health.tmp").exists(), "no .tmp may survive a save");
		assert!(dir.join(HEALTH_STORE_FILE).exists());
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn missing_health_store_loads_empty() {
		let dir = scratch_dir("missing");
		let _ = std::fs::remove_dir_all(&dir);
		let store = load_health_store(&dir).expect("missing store is an empty store");
		assert_eq!(store.version, HEALTH_STORE_VERSION);
		assert!(store.records.is_empty());
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn save_verification_preserves_other_models() {
		let dir = scratch_dir("read-modify-write");
		let _ = std::fs::remove_dir_all(&dir);
		save_verification(&dir, &verification_record(MODEL_A, VerificationStatus::Passing)).expect("seed A");
		save_verification(&dir, &verification_record(MODEL_B, VerificationStatus::Failing)).expect("upsert B");
		let store = load_health_store(&dir).expect("load");
		assert!(store.get(MODEL_A).is_some(), "model A's record must survive model B's upsert");
		assert_eq!(
			store.get(MODEL_B).map(|record| record.status),
			Some(VerificationStatus::Failing)
		);
		assert_eq!(store.records.len(), 2);
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn corrupt_health_store_errors_without_panic() {
		let dir = scratch_dir("corrupt");
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("create scratch dir");
		std::fs::write(dir.join(HEALTH_STORE_FILE), b"not json at all{").expect("write corrupt store");
		assert!(load_health_store(&dir).is_err(), "corrupt JSON must be an error, not a panic");
		// Unsupported schema version is also a typed error.
		std::fs::write(
			dir.join(HEALTH_STORE_FILE),
			br#"{"version":99,"updated_at":"2026-09-09T00:00:00Z","records":{}}"#,
		)
		.expect("write v99 store");
		assert!(load_health_store(&dir).is_err(), "unknown schema version must be an error");
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn upsert_bumps_updated_at() {
		let mut store = HealthStore::new();
		let first = store.updated_at;
		store.upsert(verification_record(MODEL_A, VerificationStatus::Passing));
		assert!(store.updated_at >= first, "updated_at must advance on upsert");
		assert_eq!(store.get(MODEL_A).map(|record| record.model_id.as_str()), Some(MODEL_A));
	}

	// ------------------------------------------------------------------
	// budget env
	// ------------------------------------------------------------------

	#[test]
	fn budget_env_is_optional_and_only_reported() {
		with_env(&[(MAX_COST_ENV, None)], || {
			assert_eq!(max_cost_usd_env(), None);
		});
		with_env(&[(MAX_COST_ENV, Some("0.05"))], || {
			assert_eq!(max_cost_usd_env(), Some(0.05));
		});
		with_env(&[(MAX_COST_ENV, Some("garbage"))], || {
			assert_eq!(max_cost_usd_env(), None, "unparsable budget is ignored");
		});
		with_env(&[(MAX_COST_ENV, Some("0"))], || {
			assert_eq!(max_cost_usd_env(), None, "a zero budget is treated as absent");
		});
		// The estimate is reported only when the budget env is present.
		with_env(&[(MAX_COST_ENV, Some("0.05")), (LIVE_TESTS_ENV, Some("1"))], || {
			assert_eq!(max_cost_usd_env(), Some(0.05));
		});
	}
}
