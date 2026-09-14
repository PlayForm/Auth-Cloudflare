//! Health - delivery health records, conformance verification, and typed
//! failure evidence for Cloudflare Workers AI models.
//!
//! The provider captures delivery-failure evidence (circuit-breaker record)
//! instead of silently switching models, and maintains the canonical
//! verification record (`ModelVerification`), the delivery-failure taxonomy
//! (`FailureClass`), and sanitized-evidence rules: never persist
//! Authorization headers, API tokens, full prompts, or tool outputs;
//! response excerpts are capped at `MAX_EXCERPT_CHARS` (512).
//!
//! The three statements a model can make must never be conflated:
//! *Available* (Cloudflare says the account can invoke it), *Capable*
//! (schema/docs say a feature is supported), and *Verified* (this project
//! recently tested it successfully with Hermes-style tools).
//! `ModelVerification` owns the *Verified* statement, and
//! `passes_acceptance_gate` implements the acceptance criteria for
//! recommended models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Version of the conformance suite that produced verification records.
///
/// Bump when suite scenarios or acceptance criteria change so stale
/// `ModelVerification` artifacts cannot be mistaken for fresh ones.
pub const CONFORMANCE_SUITE_VERSION: &str = "0.0.1";

/// Maximum length of a sanitized `response_excerpt`.
pub const MAX_EXCERPT_CHARS: usize = 512;

/// Minimum completed runs before the acceptance gate may pass.
const MIN_ACCEPTANCE_RUNS: u32 = 100;

/// Maximum tolerated fraction of transport/timeout/provider-5xx failures.
const MAX_TRANSPORT_FAILURE_FRACTION: f64 = 0.02;

/// Minimum single-tool-call success rate for the acceptance gate.
const MIN_SINGLE_TOOL_SUCCESS_RATE: f64 = 0.95;

/// Minimum multi-turn tool-loop success rate for the acceptance gate.
const MIN_MULTI_TURN_TOOL_SUCCESS_RATE: f64 = 0.93;

/// Delivery-rate threshold below which a health window is degraded.
const DEGRADED_RATE_THRESHOLD: f64 = 0.9;

/// Typed delivery-failure taxonomy.
///
/// Every failure recorded for a Cloudflare Workers AI model must map to
/// exactly one class. `Unknown` is the last resort for classes not yet in
/// the taxonomy - never guess a specific class without evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
	/// Credentials could not be resolved (missing env var or empty token).
	AuthMissing,
	/// Cloudflare rejected the presented credentials (401).
	AuthRejected,
	/// The configured account could not be found or is out of scope.
	AccountNotFound,
	/// The requested model is not available on this account or route.
	ModelUnavailable,
	/// A request parameter is not supported by the model or endpoint.
	UnsupportedParameter,
	/// The context window was exceeded by the request.
	ContextLimitExceeded,
	/// Connection establishment timed out.
	ConnectTimeout,
	/// The upstream did not respond within the read timeout.
	ReadTimeout,
	/// The stream produced no event within the idle timeout.
	StreamIdleTimeout,
	/// The connection was reset mid-request or mid-stream.
	ConnectionReset,
	/// TLS handshake or certificate failure.
	TlsFailure,
	/// Cloudflare rate-limited the request (429).
	RateLimited,
	/// The provider returned a 5xx error.
	ProviderServerError,
	/// Cloudflare edge error envelope (cf-ray present, success: false).
	CloudflareEdgeError,
	/// The completion was delivered with empty content.
	EmptyCompletion,
	/// The completion was cut off before a terminal state.
	TruncatedCompletion,
	/// The response body was not valid JSON.
	InvalidJson,
	/// The JSON did not match the Chat Completions shape.
	InvalidChatCompletionShape,
	/// An SSE event was malformed.
	InvalidSseEvent,
	/// A terminal chunk was missing its finish reason.
	MissingFinishReason,
	/// The model answered without issuing the required tool call.
	NoToolCall,
	/// The tool call named an unknown or disallowed tool.
	InvalidToolName,
	/// The tool-call arguments were not valid JSON for the schema.
	InvalidToolArguments,
	/// The same tool call was issued more than once.
	DuplicateToolCall,
	/// The multi-turn tool loop did not converge to a final answer.
	ToolLoopDidNotConverge,
	/// Failure class not yet classified - never guess a specific class.
	Unknown,
}

/// One sanitized failure observation.
///
/// Security contract: this type has NO field for Authorization headers, API
/// tokens, prompts, or tool outputs - and `deny_unknown_fields` rejects any
/// JSON carrying such a field, so a leaked credential cannot round-trip
/// through serde silently. `response_excerpt` is capped at
/// `MAX_EXCERPT_CHARS` (512) characters by `new`/`sanitize_excerpt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureEvidence {
	pub failure_class: FailureClass,
	pub http_status: Option<u16>,
	pub cloudflare_ray_id: Option<String>,
	pub elapsed_ms: Option<u64>,
	pub model_id: String,
	pub request_id: Option<String>,
	pub response_excerpt: Option<String>,
}

impl FailureEvidence {
	/// Build sanitized evidence. `response_excerpt` is truncated to
	/// `MAX_EXCERPT_CHARS` characters; everything else passes through as-is.
	pub fn new(
		failure_class: FailureClass,
		http_status: Option<u16>,
		cloudflare_ray_id: Option<String>,
		elapsed_ms: Option<u64>,
		model_id: impl Into<String>,
		request_id: Option<String>,
		response_excerpt: Option<String>,
	) -> Self {
		Self {
			failure_class,
			http_status,
			cloudflare_ray_id,
			elapsed_ms,
			model_id: model_id.into(),
			request_id,
			response_excerpt: Self::sanitize_excerpt(response_excerpt.as_deref()),
		}
	}

	/// Cap a raw response excerpt at `MAX_EXCERPT_CHARS` characters
	/// (char-safe: never splits a UTF-8 scalar mid-sequence).
	///
	/// Callers must still avoid passing Authorization/token content - this
	/// caps size, it does not redact secrets.
	pub fn sanitize_excerpt(raw: Option<&str>) -> Option<String> {
		raw.map(|excerpt| {
			if excerpt.chars().count() <= MAX_EXCERPT_CHARS {
				excerpt.to_string()
			} else {
				excerpt.chars().take(MAX_EXCERPT_CHARS).collect()
			}
		})
	}
}

/// Rolling delivery-health window for one model (circuit-breaker record:
/// captures failure evidence; automatic failover stays disabled).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelHealthRecord {
	pub model_id: String,
	pub window_started_at: DateTime<Utc>,
	pub request_count: u64,
	pub successful_responses: u64,
	pub transport_failures: u64,
	pub upstream_5xx_failures: u64,
	pub timeout_failures: u64,
	pub malformed_tool_calls: u64,
	pub median_latency_ms: Option<u64>,
	pub last_failure_at: Option<DateTime<Utc>>,
	pub last_failure_class: Option<FailureClass>,
}

impl ModelHealthRecord {
	/// Fraction of requests that delivered successfully
	/// (`successful / request_count`), `None` when the window is empty.
	pub fn delivery_success_rate(&self) -> Option<f64> {
		if self.request_count == 0 {
			return None;
		}
		Some((self.successful_responses as f64 / self.request_count as f64).min(1.0))
	}

	/// True when the window is degraded: delivery rate below 0.9 or any
	/// recorded failure in the window (GLM-5.3 Flash delivered 22/31 = 70.9%
	/// before it was demoted to experimental).
	pub fn is_degraded(&self) -> bool {
		let failure_total =
			self.transport_failures + self.upstream_5xx_failures + self.timeout_failures + self.malformed_tool_calls;
		if failure_total > 0 {
			return true;
		}
		self.delivery_success_rate().is_some_and(|rate| rate < DEGRADED_RATE_THRESHOLD)
	}
}

/// Render a token-free, human-readable delivery-health warning for a
/// degraded model, or `None` for a clean/empty window.
///
/// The message states the model id, the percent delivery rate, the request
/// count, and the recommended stable alternative - no tokens, secrets, or
/// Authorization material are ever embedded.
pub fn health_warning(record: &ModelHealthRecord, stable_alternative: &str) -> Option<String> {
	if !record.is_degraded() {
		return None;
	}
	let percent = record.delivery_success_rate().unwrap_or(0.0) * 100.0;
	Some(format!(
		"model {} delivery is degraded ({:.1} percent successful over {} requests); recommended stable alternative: {}",
		record.model_id, percent, record.request_count, stable_alternative
	))
}

/// The stable fallback to recommend for a model: the default model when the
/// given id differs from it, or an empty-string sentinel when it already is
/// the default (no change to recommend).
pub fn recommended_stable_alternative(model_id: &str) -> &'static str {
	if model_id == crate::DEFAULT_MODEL { "" } else { crate::DEFAULT_MODEL }
}

/// Outcome of the most recent conformance run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
	/// No conformance data yet.
	Untested,
	/// Latest run met all acceptance criteria.
	Passing,
	/// Latest run passed some, but not all, acceptance criteria.
	Degraded,
	/// Latest run failed acceptance criteria outright.
	Failing,
	/// Data is older than the freshness window - treat as untested.
	Expired,
}

/// How much evidence backs a verification verdict (the selector prefers
/// verified capability over provider claims).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationConfidence {
	/// No evidence at all.
	None,
	/// Only catalog/schema metadata, never executed.
	StaticMetadataOnly,
	/// A cheap smoke suite (completion, stream, one tool call) passed.
	SmokeTested,
	/// The full conformance suite passed at least once.
	ConformanceTested,
	/// Passing across repeated runs with regression coverage.
	RegressionTested,
}

/// Canonical verification record - the *Verified* statement for one model
/// Consumed by the picker, policy, and health reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelVerification {
	pub model_id: String,
	pub latest_run_at: Option<DateTime<Utc>>,
	pub expires_at: Option<DateTime<Utc>>,
	pub suite_version: String,
	pub runner_version: String,
	pub status: VerificationStatus,
	pub agent_eligible: bool,
	pub confidence: VerificationConfidence,
	pub total_runs: u32,
	pub successful_runs: u32,
	pub text_completion_success_rate: Option<f64>,
	pub stream_completion_success_rate: Option<f64>,
	pub single_tool_success_rate: Option<f64>,
	pub multi_turn_tool_success_rate: Option<f64>,
	pub structured_output_success_rate: Option<f64>,
	pub median_latency_ms: Option<u64>,
	pub p95_latency_ms: Option<u64>,
	pub total_failures: u32,
	pub timeout_failures: u32,
	pub transport_failures: u32,
	pub provider_5xx_failures: u32,
	pub malformed_response_failures: u32,
	pub malformed_tool_call_failures: u32,
	pub tool_loop_failures: u32,
	pub last_failure: Option<FailureEvidence>,
}

impl ModelVerification {
	/// Overall delivery rate (`successful_runs / total_runs`), `None` for
	/// zero runs.
	pub fn delivery_success_rate(&self) -> Option<f64> {
		if self.total_runs == 0 {
			return None;
		}
		Some((self.successful_runs as f64 / self.total_runs as f64).min(1.0))
	}

	/// Acceptance gate for recommended models:
	///
	/// - at least 100 completed runs;
	/// - transport completion >= 98% (transport + timeout + provider-5xx
	///   failures <= 2% of runs);
	/// - single-tool success rate >= 95%;
	/// - multi-turn tool-loop success rate >= 93%.
	///
	/// Missing rate evidence fails the gate - unverified is not passing.
	pub fn passes_acceptance_gate(&self) -> bool {
		if self.total_runs < MIN_ACCEPTANCE_RUNS {
			return false;
		}
		let transport_failures = self.transport_failures + self.timeout_failures + self.provider_5xx_failures;
		let transport_fraction = transport_failures as f64 / self.total_runs as f64;
		if transport_fraction > MAX_TRANSPORT_FAILURE_FRACTION {
			return false;
		}
		match (self.single_tool_success_rate, self.multi_turn_tool_success_rate) {
			(Some(single), Some(multi)) => {
				single >= MIN_SINGLE_TOOL_SUCCESS_RATE && multi >= MIN_MULTI_TURN_TOOL_SUCCESS_RATE
			},
			_ => false,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn timestamp() -> DateTime<Utc> {
		DateTime::from_timestamp(1_788_800_000, 0).expect("valid timestamp")
	}

	fn health_record(
		requests: u64,
		successful: u64,
		transport: u64,
		timeout: u64,
		fivexx: u64,
		tool_calls: u64,
	) -> ModelHealthRecord {
		ModelHealthRecord {
			model_id: "@cf/deepseek-ai/deepseek-v4-flash-0731".to_string(),
			window_started_at: timestamp(),
			request_count: requests,
			successful_responses: successful,
			transport_failures: transport,
			upstream_5xx_failures: fivexx,
			timeout_failures: timeout,
			malformed_tool_calls: tool_calls,
			median_latency_ms: None,
			last_failure_at: None,
			last_failure_class: None,
		}
	}

	fn verification_record(
		total: u32,
		successful: u32,
		transport: u32,
		timeout: u32,
		fivexx: u32,
		single: Option<f64>,
		multi: Option<f64>,
	) -> ModelVerification {
		ModelVerification {
			model_id: "@cf/deepseek-ai/deepseek-v4-flash-0731".to_string(),
			latest_run_at: Some(timestamp()),
			expires_at: None,
			suite_version: CONFORMANCE_SUITE_VERSION.to_string(),
			runner_version: "test".to_string(),
			status: VerificationStatus::Untested,
			agent_eligible: true,
			confidence: VerificationConfidence::ConformanceTested,
			total_runs: total,
			successful_runs: successful,
			text_completion_success_rate: None,
			stream_completion_success_rate: None,
			single_tool_success_rate: single,
			multi_turn_tool_success_rate: multi,
			structured_output_success_rate: None,
			median_latency_ms: None,
			p95_latency_ms: None,
			total_failures: total - successful,
			timeout_failures: timeout,
			transport_failures: transport,
			provider_5xx_failures: fivexx,
			malformed_response_failures: 0,
			malformed_tool_call_failures: 0,
			tool_loop_failures: 0,
			last_failure: None,
		}
	}

	#[test]
	fn success_rate_math() {
		// GLM-5.3 Flash evidence: 22 successful of 31.
		let rate = health_record(31, 22, 9, 0, 0, 0).delivery_success_rate().expect("rate present");
		let expected = 22.0 / 31.0;
		assert!((rate - expected).abs() < 1e-9, "rate {rate} != {expected}");
		assert_eq!(health_record(100, 100, 0, 0, 0, 0).delivery_success_rate(), Some(1.0));
		assert_eq!(health_record(0, 0, 0, 0, 0, 0).delivery_success_rate(), None);
	}

	#[test]
	fn degraded_threshold() {
		// 70.9% delivery is below the 0.9 threshold -> degraded.
		assert!(health_record(31, 22, 9, 0, 0, 0).is_degraded());
		// Any failure in the window degrades even a high-rate record.
		assert!(health_record(100, 99, 0, 1, 0, 0).is_degraded());
		assert!(health_record(100, 99, 0, 0, 1, 0).is_degraded());
		assert!(health_record(100, 99, 0, 0, 0, 1).is_degraded());
		// Clean window at 100% is not degraded.
		assert!(!health_record(100, 100, 0, 0, 0, 0).is_degraded());
		// Empty window: no evidence, not degraded.
		assert!(!health_record(0, 0, 0, 0, 0, 0).is_degraded());
	}

	#[test]
	fn acceptance_gate_passes_for_conformant_record() {
		let record = verification_record(100, 100, 0, 0, 0, Some(0.96), Some(0.94));
		assert!(record.passes_acceptance_gate());
	}

	#[test]
	fn acceptance_gate_allows_boundary_transport_failures() {
		// Exactly 2% transport failures is still >= 98% transport completion.
		let record = verification_record(100, 98, 2, 0, 0, Some(0.95), Some(0.93));
		assert!(record.passes_acceptance_gate());
	}

	#[test]
	fn acceptance_gate_rejects_glm_style_record() {
		// GLM-5.3 Flash evidence: 31 runs, 22 successful, 9 transport failures.
		let record = verification_record(31, 22, 9, 0, 0, Some(0.709), Some(0.709));
		assert!(!record.passes_acceptance_gate());
	}

	#[test]
	fn acceptance_gate_rejects_missing_rates() {
		assert!(!verification_record(100, 100, 0, 0, 0, None, None).passes_acceptance_gate());
		assert!(!verification_record(100, 100, 0, 0, 0, Some(0.96), None).passes_acceptance_gate());
	}

	#[test]
	fn acceptance_gate_rejects_low_tool_rates() {
		assert!(!verification_record(100, 100, 0, 0, 0, Some(0.90), Some(0.94)).passes_acceptance_gate());
		assert!(!verification_record(100, 100, 0, 0, 0, Some(0.96), Some(0.90)).passes_acceptance_gate());
	}

	#[test]
	fn failure_evidence_truncates_long_excerpts() {
		let evidence = FailureEvidence::new(
			FailureClass::StreamIdleTimeout,
			Some(200),
			Some("ray-1".to_string()),
			Some(120_000),
			"@cf/zai-org/glm-5.3-flash",
			Some("uuid".to_string()),
			Some("e".repeat(600)),
		);
		let excerpt = evidence.response_excerpt.expect("excerpt present");
		assert_eq!(excerpt.chars().count(), MAX_EXCERPT_CHARS);
		assert_eq!(excerpt, "e".repeat(MAX_EXCERPT_CHARS));
	}

	#[test]
	fn failure_evidence_truncation_is_char_safe() {
		// Multi-byte scalars must not be split mid-sequence.
		let sanitized = FailureEvidence::sanitize_excerpt(Some(&"€".repeat(600))).expect("sanitized");
		assert_eq!(sanitized.chars().count(), MAX_EXCERPT_CHARS);
		assert_eq!(sanitized, "€".repeat(MAX_EXCERPT_CHARS));
	}

	#[test]
	fn failure_evidence_keeps_short_excerpts() {
		assert_eq!(FailureEvidence::sanitize_excerpt(Some("ok")), Some("ok".to_string()));
		assert_eq!(FailureEvidence::sanitize_excerpt(None), None);
	}

	#[test]
	fn failure_evidence_has_no_token_field() {
		let evidence = FailureEvidence::new(
			FailureClass::AuthRejected,
			Some(401),
			None,
			Some(100),
			"@cf/deepseek-ai/deepseek-v4-flash-0731",
			None,
			Some("unauthorized".to_string()),
		);
		let json = serde_json::to_string(&evidence).expect("serializes");
		let lowered = json.to_lowercase();
		for forbidden in ["token", "authorization", "secret", "api_key", "bearer"] {
			assert!(!lowered.contains(forbidden), "JSON must not contain {forbidden}: {json}");
		}
	}

	#[test]
	fn failure_evidence_rejects_unknown_fields() {
		// A leaked credential field must not round-trip through serde.
		let json = serde_json::json!({
			"failure_class": "auth_missing",
			"http_status": null,
			"cloudflare_ray_id": null,
			"elapsed_ms": null,
			"model_id": "@cf/deepseek-ai/deepseek-v4-flash-0731",
			"request_id": null,
			"response_excerpt": null,
			"token": "leaked",
		});
		let result = serde_json::from_value::<FailureEvidence>(json);
		assert!(result.is_err(), "unknown field must be rejected: {result:?}");
	}

	#[test]
	fn failure_class_serde_snake_case() {
		let cases = [
			(FailureClass::AuthMissing, "\"auth_missing\""),
			(FailureClass::ToolLoopDidNotConverge, "\"tool_loop_did_not_converge\""),
			(FailureClass::StreamIdleTimeout, "\"stream_idle_timeout\""),
			(FailureClass::InvalidChatCompletionShape, "\"invalid_chat_completion_shape\""),
			(FailureClass::ProviderServerError, "\"provider_server_error\""),
		];
		for (class, name) in cases {
			let json = serde_json::to_string(&class).expect("ser");
			assert_eq!(json, name);
			assert_eq!(serde_json::from_str::<FailureClass>(&json).expect("de"), class);
		}
		let back: FailureClass = serde_json::from_str("\"cloudflare_edge_error\"").expect("de");
		assert_eq!(back, FailureClass::CloudflareEdgeError);
	}

	#[test]
	fn status_and_confidence_serde_snake_case() {
		let statuses = [
			(VerificationStatus::Untested, "untested"),
			(VerificationStatus::Passing, "passing"),
			(VerificationStatus::Degraded, "degraded"),
			(VerificationStatus::Failing, "failing"),
			(VerificationStatus::Expired, "expired"),
		];
		for (status, name) in statuses {
			let json = serde_json::to_string(&status).expect("ser");
			assert_eq!(json, format!("\"{name}\""));
			assert_eq!(serde_json::from_str::<VerificationStatus>(&json).expect("de"), status);
		}
		let confidences = [
			(VerificationConfidence::None, "none"),
			(VerificationConfidence::StaticMetadataOnly, "static_metadata_only"),
			(VerificationConfidence::SmokeTested, "smoke_tested"),
			(VerificationConfidence::ConformanceTested, "conformance_tested"),
			(VerificationConfidence::RegressionTested, "regression_tested"),
		];
		for (confidence, name) in confidences {
			let json = serde_json::to_string(&confidence).expect("ser");
			assert_eq!(json, format!("\"{name}\""));
			assert_eq!(serde_json::from_str::<VerificationConfidence>(&json).expect("de"), confidence);
		}
	}

	#[test]
	fn health_record_serde_roundtrip() {
		let record = health_record(100, 98, 2, 0, 0, 1);
		let json = serde_json::to_string(&record).expect("ser");
		let back: ModelHealthRecord = serde_json::from_str(&json).expect("de");
		assert_eq!(back, record);
	}

	#[test]
	fn verification_serde_roundtrip_with_evidence() {
		let mut record = verification_record(100, 98, 2, 0, 0, Some(0.96), Some(0.94));
		record.status = VerificationStatus::Passing;
		record.last_failure = Some(FailureEvidence::new(
			FailureClass::RateLimited,
			Some(429),
			Some("ray-abc".to_string()),
			Some(1_200),
			"@cf/deepseek-ai/deepseek-v4-flash-0731",
			Some("req-7".to_string()),
			Some("rate limited".to_string()),
		));
		let json = serde_json::to_string(&record).expect("ser");
		let back: ModelVerification = serde_json::from_str(&json).expect("de");
		assert_eq!(back.model_id, record.model_id);
		assert_eq!(back.status, VerificationStatus::Passing);
		assert_eq!(back.total_runs, 100);
		assert_eq!(back.single_tool_success_rate, Some(0.96));
		let evidence = back.last_failure.expect("evidence present");
		assert_eq!(evidence.failure_class, FailureClass::RateLimited);
		assert_eq!(evidence.http_status, Some(429));
		assert_eq!(evidence.cloudflare_ray_id.as_deref(), Some("ray-abc"));
	}

	#[test]
	fn health_warning_renders_for_degraded_record() {
		let record = health_record(100, 89, 11, 0, 0, 0);
		let warning = health_warning(&record, crate::DEFAULT_MODEL).expect("warning present");
		assert!(warning.contains("89.0"), "warning must contain the rate: {warning}");
		assert!(
			warning.contains(crate::DEFAULT_MODEL),
			"warning must contain the alternative: {warning}"
		);
		assert!(warning.contains("100"), "warning must contain the request count: {warning}");
		assert!(
			warning.contains(&record.model_id),
			"warning must contain the model id: {warning}"
		);
	}

	#[test]
	fn health_warning_none_for_clean_record() {
		assert_eq!(health_warning(&health_record(100, 100, 0, 0, 0, 0), "alt"), None);
	}

	#[test]
	fn health_warning_none_for_empty_record() {
		assert_eq!(health_warning(&health_record(0, 0, 0, 0, 0, 0), "alt"), None);
	}

	#[test]
	fn recommended_stable_alternative_empty_sentinel_for_default() {
		assert_eq!(recommended_stable_alternative(crate::DEFAULT_MODEL), "");
	}

	#[test]
	fn recommended_stable_alternative_returns_default_for_glm() {
		assert_eq!(
			recommended_stable_alternative("@cf/zai-org/glm-5.3-flash"),
			crate::DEFAULT_MODEL
		);
	}

	#[test]
	fn health_warning_is_token_free() {
		let record = health_record(31, 22, 9, 0, 0, 0);
		let warning = health_warning(&record, crate::DEFAULT_MODEL).expect("warning present");
		let lowered = warning.to_lowercase();
		for forbidden in ["token", "secret", "bearer"] {
			assert!(!lowered.contains(forbidden), "warning must not contain {forbidden}: {warning}");
		}
	}
}
