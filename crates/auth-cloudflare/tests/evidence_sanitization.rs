//! Integration tests for `FailureEvidence` sanitization under the security
//! contract: excerpt capping at 512 chars (char-safe), serde roundtrip,
//! `deny_unknown_fields`, and a serialized payload that can never carry
//! token/authorization/secret content.
//!
//! Zero network and zero secrets: all inputs are synthetic.

use auth_cloudflare::health::{FailureClass, FailureEvidence, MAX_EXCERPT_CHARS};

const MODEL: &str = "@cf/deepseek-ai/deepseek-v4-flash-0731";

#[test]
fn long_excerpt_is_capped_at_512_chars() {
	let evidence = FailureEvidence::new(
		FailureClass::ReadTimeout,
		Some(200),
		Some("ray-1".to_string()),
		Some(120_000),
		MODEL,
		Some("req-1".to_string()),
		Some("e".repeat(600)),
	);
	let excerpt = evidence.response_excerpt.expect("excerpt present");
	assert_eq!(excerpt.chars().count(), MAX_EXCERPT_CHARS);
	assert_eq!(excerpt, "e".repeat(MAX_EXCERPT_CHARS));
}

#[test]
fn truncation_is_char_safe_for_multibyte() {
	// Multi-byte scalars must never be split mid-sequence.
	let evidence = FailureEvidence::new(
		FailureClass::StreamIdleTimeout,
		None,
		None,
		None,
		MODEL,
		None,
		Some("€".repeat(600)),
	);
	let excerpt = evidence.response_excerpt.expect("excerpt present");
	assert_eq!(excerpt.chars().count(), MAX_EXCERPT_CHARS);
	assert_eq!(excerpt, "€".repeat(MAX_EXCERPT_CHARS));
}

#[test]
fn short_and_missing_excerpts_pass_through() {
	let short = FailureEvidence::new(
		FailureClass::RateLimited,
		Some(429),
		None,
		Some(1_200),
		MODEL,
		None,
		Some("rate limited".to_string()),
	);
	assert_eq!(short.response_excerpt.as_deref(), Some("rate limited"));
	let none = FailureEvidence::new(FailureClass::Unknown, None, None, None, MODEL, None, None);
	assert_eq!(none.response_excerpt, None);
}

#[test]
fn evidence_serde_roundtrip() {
	let evidence = FailureEvidence::new(
		FailureClass::CloudflareEdgeError,
		Some(200),
		Some("ray-abc".to_string()),
		Some(3_000),
		MODEL,
		Some("req-7".to_string()),
		Some("edge error".to_string()),
	);
	let json = serde_json::to_string(&evidence).expect("serializes");
	let back: FailureEvidence = serde_json::from_str(&json).expect("deserializes");
	assert_eq!(back, evidence);
}

#[test]
fn deny_unknown_fields_rejects_a_token_field() {
	// A leaked credential field must not round-trip through serde.
	let json = serde_json::json!({
		"failure_class": "auth_rejected",
		"http_status": 401,
		"cloudflare_ray_id": null,
		"elapsed_ms": 100,
		"model_id": MODEL,
		"request_id": null,
		"response_excerpt": null,
		"token": "leaked",
	});
	assert!(
		serde_json::from_value::<FailureEvidence>(json).is_err(),
		"an unknown 'token' field must be rejected"
	);
}

#[test]
fn serialized_json_never_contains_secret_substrings() {
	let evidence = FailureEvidence::new(
		FailureClass::AuthRejected,
		Some(401),
		None,
		Some(100),
		MODEL,
		None,
		Some("unauthorized request".to_string()),
	);
	let json = serde_json::to_string(&evidence).expect("serializes");
	let lowered = json.to_lowercase();
	for forbidden in ["token", "authorization", "secret"] {
		assert!(
			!lowered.contains(forbidden),
			"serialized evidence must not contain {forbidden}: {json}"
		);
	}
}
