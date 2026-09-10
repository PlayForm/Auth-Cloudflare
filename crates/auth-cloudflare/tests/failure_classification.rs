//! Integration tests for the PUBLIC pure failure classifiers in
//! `auth_cloudflare::verify` - synthetic inputs only, zero network.
//!
//! Every classifier is exercised against the documented taxonomy: HTTP
//! status + envelope classification, transport message keyword
//! classification, streaming acceptance, and tool-call response
//! classification. `Unknown` is asserted only for genuinely unclassifiable
//! input - never guessed.

use auth_cloudflare::health::FailureClass;
use auth_cloudflare::verify::{
	classify_http_failure, classify_stream_failure, classify_tool_response, classify_transport_message, StreamEvent,
};

/// One SSE `data:` event with an optional content delta and optional string
/// finish reason - mirrors the live-stream parsing shape.
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

#[test]
fn http_401_is_auth_rejected() {
	let envelope = r#"{"success":false,"errors":[{"code":9109,"message":"Authentication error"}]}"#;
	assert_eq!(classify_http_failure(401, Some(envelope), false), FailureClass::AuthRejected);
	// Even with no body at all, 401 is rejected credentials.
	assert_eq!(classify_http_failure(401, None, false), FailureClass::AuthRejected);
}

#[test]
fn http_403_is_account_not_found_or_auth_rejected() {
	// Cloudflare envelope code 9103 = Account not found.
	assert_eq!(
		classify_http_failure(
			403,
			Some(r#"{"success":false,"errors":[{"code":9103,"message":"Account not found"}]}"#),
			true
		),
		FailureClass::AccountNotFound
	);
	// A message naming a missing account also classifies as not found.
	assert_eq!(
		classify_http_failure(
			403,
			Some(r#"{"success":false,"errors":[{"message":"the account was not found"}]}"#),
			false
		),
		FailureClass::AccountNotFound
	);
	// Any other 403 (e.g. envelope code 10000) is rejected credentials.
	assert_eq!(
		classify_http_failure(403, Some(r#"{"success":false,"errors":[{"code":10000}]}"#), false),
		FailureClass::AuthRejected
	);
	assert_eq!(classify_http_failure(403, None, false), FailureClass::AuthRejected);
}

#[test]
fn http_429_is_rate_limited() {
	assert_eq!(
		classify_http_failure(429, Some(r#"{"success":false,"errors":[{"code":10000}]}"#), false),
		FailureClass::RateLimited
	);
}

#[test]
fn http_5xx_is_provider_server_error() {
	for status in [500u16, 502, 503, 504, 599] {
		assert_eq!(
			classify_http_failure(status, Some("boom"), false),
			FailureClass::ProviderServerError,
			"status {status}"
		);
	}
}

#[test]
fn http_200_success_false_envelope_with_cf_ray_is_edge_error() {
	let envelope = r#"{"success":false,"errors":[]}"#;
	assert_eq!(
		classify_http_failure(200, Some(envelope), true),
		FailureClass::CloudflareEdgeError
	);
	// ... but only with a cf-ray AND a success:false envelope.
	assert_eq!(classify_http_failure(200, Some(envelope), false), FailureClass::Unknown);
	assert_eq!(
		classify_http_failure(200, Some(r#"{"success":true}"#), true),
		FailureClass::Unknown
	);
}

#[test]
fn transport_messages_classify_by_keyword() {
	assert_eq!(
		classify_transport_message("request timed out after 30s"),
		FailureClass::ReadTimeout
	);
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
}

#[test]
fn stream_failure_same_reason_duplicate_terminal_is_accepted() {
	// DeepSeek re-emits the terminal finish_reason on the final usage chunk
	// (live smoke evidence) - the same reason on an empty delta is accepted.
	let duplicate_terminal = vec![
		data_event(Some("x"), None),
		data_event(None, Some("stop")),
		data_event(None, Some("stop")),
	];
	assert_eq!(
		classify_stream_failure(&duplicate_terminal, Some(100), 500),
		None,
		"the SAME finish_reason re-emitted on an empty delta is the provider's usage trailer"
	);
}

#[test]
fn stream_failure_conflicting_terminals_are_protocol_violation() {
	let conflicting_terminal = vec![
		data_event(Some("x"), None),
		data_event(None, Some("stop")),
		data_event(None, Some("length")),
	];
	assert_eq!(
		classify_stream_failure(&conflicting_terminal, Some(100), 500),
		Some(FailureClass::InvalidSseEvent),
		"a second terminal chunk with a DIFFERENT reason is a protocol violation"
	);
}

#[test]
fn stream_failure_missing_terminal_is_missing_finish_reason() {
	let missing_terminal = vec![data_event(Some("x"), None)];
	assert_eq!(
		classify_stream_failure(&missing_terminal, Some(100), 500),
		Some(FailureClass::MissingFinishReason)
	);
	// A clean stream (content delta + exactly one terminal) passes.
	let clean = vec![
		data_event(Some("CF_HERMES"), None),
		data_event(Some("_OK"), None),
		data_event(None, Some("stop")),
	];
	assert_eq!(classify_stream_failure(&clean, Some(300), 1_500), None);
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

	let duplicate = r#"{"choices":[{"message":{"tool_calls":[{"function":{"name":"get_project_sentinel","arguments":"{}"}},{"function":{"name":"get_project_sentinel","arguments":"{}"}}]}}]}"#;
	assert_eq!(classify_tool_response(Some(duplicate)), Some(FailureClass::DuplicateToolCall));

	assert_eq!(classify_tool_response(Some("not json")), Some(FailureClass::InvalidJson));
	assert_eq!(
		classify_tool_response(Some(r#"{"no":"choices"}"#)),
		Some(FailureClass::InvalidChatCompletionShape)
	);
}

#[test]
fn unknown_only_for_unclassifiable_input() {
	// 400 with a plain body and no cf-ray: no evidence for any specific class.
	assert_eq!(classify_http_failure(400, Some("bad request"), false), FailureClass::Unknown);
	assert_eq!(classify_http_failure(200, None, false), FailureClass::Unknown);
	assert_eq!(classify_transport_message("weird mystery error"), FailureClass::Unknown);
}
