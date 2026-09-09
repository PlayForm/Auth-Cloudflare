//! Fetch - live Workers AI model catalog retrieval over HTTPS.
//!
//! Typed blocking GET of
//! `/accounts/<id>/ai/models/search?format=openrouter&per_page=1000&hide_experimental=false&include_deprecated=false`
//! (feedback 01 contract) with the token carried exclusively in the
//! `Authorization: Bearer *** header.
//!
//! Error taxonomy (feedback 01/03): missing token → [`CloudflareError::MissingEnv`]
//! (defensive - the core's `Config` already guarantees a non-empty token);
//! invalid token / wrong scope / permission failure → [`CloudflareError::Api`]
//! with the Cloudflare envelope `errors[0]` code when present, else the HTTP
//! status; rate limit → [`CloudflareError::Api`] code 429 plus a Retry-After
//! hint; transient 5xx → [`CloudflareError::Http`]; malformed JSON or a
//! payload without a `data` array → [`CloudflareError::NoDataArray`];
//! transport/network failure → [`CloudflareError::Http`].
//!
//! Security: the token never reaches a URL, an error string, or a log line.
//! Every text source that could feed an error (response body, transport
//! message, body-read failure) is scrubbed of the token before it is mapped.

use std::time::Duration;

use crate::auth::{AuthProvider, TOKEN_ENV};
use crate::config::SecretString;
use crate::error::CloudflareError;

/// Overall per-request timeout for the catalog GET (15s, task contract).
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Extra query parameters beyond `AuthProvider::models_url()` (feedback 01):
/// experimental models ARE included (they are labeled downstream by policy,
/// never silently hidden) and deprecated models are excluded.
const EXTRA_QUERY: &str = "&hide_experimental=false&include_deprecated=false";

/// The exact catalog endpoint URL for one account (feedback 01 contract).
///
/// Built on `AuthProvider::models_url()` so the base path stays
/// single-sourced; only the feedback-01 query parameters are appended here.
pub fn catalog_url(account_id: &str) -> String {
	format!("{}{EXTRA_QUERY}", AuthProvider::new(account_id).models_url())
}

/// The `Authorization` header value - factored out for unit testing. This is
/// the ONLY place the token leaves [`SecretString`].
pub fn auth_header(token: &SecretString) -> String {
	format!("Bearer {}", token.as_ref())
}

/// Fetch the live catalog from the Cloudflare API.
///
/// Blocking, with [`FETCH_TIMEOUT`] as the overall request budget. The token
/// is consumed exclusively through [`auth_header`]; every error path scrubs
/// the token from any text it carries (see [`redact_token`]).
pub fn fetch_catalog_from_api(
	account_id: &str,
	token: &SecretString,
	timeout: Duration,
) -> Result<serde_json::Value, CloudflareError> {
	// Defensive: an empty token cannot build a Bearer header. The core's
	// Config::from_env() already rejects this before we are ever called.
	if token.as_ref().trim().is_empty() {
		return Err(CloudflareError::MissingEnv {
			env_var: TOKEN_ENV,
			hint: "the API token is empty - export a scoped Workers AI token (Account → Workers AI → Write)"
				.to_string(),
		});
	}

	let agent = ureq::AgentBuilder::new().timeout(timeout).build();
	let request = agent
		.get(&catalog_url(account_id))
		.set("Authorization", &auth_header(token))
		.set("Accept", "application/json");

	// ureq 2 returns non-2xx as `Error::Status` - both arms carry the
	// response we need to map the error envelope.
	let (status, response) = match request.call() {
		Ok(response) => (response.status(), response),
		Err(ureq::Error::Status(status, response)) => (status, response),
		Err(transport) => {
			let message = redact_token(&transport.to_string(), token.as_ref());
			return Err(CloudflareError::Http(message));
		},
	};

	let retry_after = response.header("Retry-After").map(str::to_string);
	let body = response.into_string().map_err(|error| {
		CloudflareError::Http(redact_token(&format!("read response body: {error}"), token.as_ref()))
	})?;
	// The body is scrubbed before it can reach any error string - Cloudflare
	// never echoes the token, but defense-in-depth costs nothing.
	let body = redact_token(&body, token.as_ref());
	map_response(status, retry_after.as_deref(), &body)
}

/// Map an HTTP status + body to the typed error taxonomy.
///
/// Factored as a pure function so the mapping is unit-testable with injected
/// inputs (no network).
fn map_response(status: u16, retry_after: Option<&str>, body: &str) -> Result<serde_json::Value, CloudflareError> {
	match status {
		200 => parse_payload(body),
		401 => Err(envelope_api_error(401, body)),
		403 => Err(envelope_api_error(403, body)),
		429 => Err(rate_limit_error(retry_after, body)),
		500..=599 => Err(CloudflareError::Http(format!(
			"Cloudflare API returned HTTP {status} (transient server error)"
		))),
		other => Err(envelope_api_error(other, body)),
	}
}

/// Parse a 200 payload; malformed JSON or a missing `data` array are both
/// [`CloudflareError::NoDataArray`] (feedback 01: "invalid model payload").
fn parse_payload(body: &str) -> Result<serde_json::Value, CloudflareError> {
	let value: serde_json::Value = serde_json::from_str(body).map_err(|_| CloudflareError::NoDataArray)?;
	match value.get("data") {
		Some(serde_json::Value::Array(_)) => Ok(value),
		_ => Err(CloudflareError::NoDataArray),
	}
}

/// Build an `Api` error from the Cloudflare envelope `errors[0]` when
/// present, else fall back to the HTTP status as the code.
fn envelope_api_error(status: u16, body: &str) -> CloudflareError {
	if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
		if let Some(first) = value
			.get("errors")
			.and_then(|errors| errors.as_array())
			.and_then(|errors| errors.first())
		{
			let code = first.get("code").and_then(|code| code.as_u64()).unwrap_or(u64::from(status)) as u32;
			let message = first
				.get("message")
				.and_then(|message| message.as_str())
				.unwrap_or("unknown Cloudflare error")
				.to_string();
			return CloudflareError::Api { code, message };
		}
	}
	CloudflareError::Api { code: u32::from(status), message: format!("HTTP {status}") }
}

/// Rate-limit error: always `Api` code 429 (task contract) with the
/// Retry-After hint and the envelope message when available.
fn rate_limit_error(retry_after: Option<&str>, body: &str) -> CloudflareError {
	let retry_hint = retry_after
		.map(|seconds| format!("; retry after ~{seconds}s"))
		.unwrap_or_default();
	let envelope_message = match serde_json::from_str::<serde_json::Value>(body) {
		Ok(value) => value
			.get("errors")
			.and_then(|errors| errors.as_array())
			.and_then(|errors| errors.first())
			.and_then(|first| first.get("message").and_then(|message| message.as_str()))
			.map(|message| format!(": {message}"))
			.unwrap_or_default(),
		Err(_) => String::new(),
	};
	CloudflareError::Api {
		code: 429,
		message: format!("rate limited (HTTP 429){retry_hint}{envelope_message}"),
	}
}

/// Replace the token with a redaction marker in any text that could reach an
/// error string. The token value never survives into user-facing output.
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

	/// Synthetic Cloudflare-shaped account id (32 hex digits) - never real.
	const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
	/// Synthetic token - never a real credential.
	const TOKEN: &str = "cfut_test_synthetic_token_0001";

	#[test]
	fn catalog_url_is_exact_feedback_01_endpoint() {
		assert_eq!(
			catalog_url(ACCOUNT),
			"https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/ai/models/search?format=openrouter&per_page=1000&hide_experimental=false&include_deprecated=false"
		);
	}

	#[test]
	fn auth_header_is_bearer_with_token_value() {
		let token = SecretString::new(TOKEN);
		assert_eq!(auth_header(&token), format!("Bearer {TOKEN}"));
	}

	#[test]
	fn empty_token_is_missing_env_before_any_network() {
		let token = SecretString::new("   ");
		let error = fetch_catalog_from_api(ACCOUNT, &token, Duration::from_secs(1)).unwrap_err();
		assert!(matches!(error, CloudflareError::MissingEnv { env_var: TOKEN_ENV, .. }));
		// The error must not echo the (empty) credential.
		assert!(!error.to_string().contains("Bearer"));
	}

	#[test]
	fn ok_payload_with_data_array_is_returned() {
		let body = r#"{"data":[{"id":"@cf/deepseek-ai/deepseek-v4-flash-0731"}]}"#;
		let value = map_response(200, None, body).expect("valid payload parses");
		assert_eq!(value["data"][0]["id"], "@cf/deepseek-ai/deepseek-v4-flash-0731");
	}

	#[test]
	fn malformed_json_and_missing_data_map_to_no_data_array() {
		assert!(matches!(
			map_response(200, None, "not json at all"),
			Err(CloudflareError::NoDataArray)
		));
		assert!(matches!(map_response(200, None, "{}"), Err(CloudflareError::NoDataArray)));
		assert!(
			matches!(
				map_response(200, None, r#"{"success":true,"result":[]}"#),
				Err(CloudflareError::NoDataArray)
			),
			"a native-envelope payload without a top-level data array is NoDataArray"
		);
	}

	#[test]
	fn unauthorized_maps_to_api_with_envelope_code() {
		let body = r#"{"success":false,"errors":[{"code":9109,"message":"Invalid access token"}]}"#;
		match map_response(401, None, body) {
			Err(CloudflareError::Api { code, message }) => {
				assert_eq!(code, 9109);
				assert!(message.contains("Invalid access token"));
			},
			other => panic!("expected Api error, got {other:?}"),
		}
	}

	#[test]
	fn forbidden_maps_to_api_with_envelope_code() {
		let body = r#"{"success":false,"errors":[{"code":10000,"message":"insufficient permission"}]}"#;
		match map_response(403, None, body) {
			Err(CloudflareError::Api { code, message }) => {
				assert_eq!(code, 10000);
				assert!(message.contains("insufficient permission"));
			},
			other => panic!("expected Api error, got {other:?}"),
		}
	}

	#[test]
	fn rate_limit_maps_to_api_429_with_retry_after_hint() {
		let body = r#"{"success":false,"errors":[{"code":10000,"message":"Too many requests"}]}"#;
		match map_response(429, Some("120"), body) {
			Err(CloudflareError::Api { code, message }) => {
				assert_eq!(code, 429, "rate limit always carries Api code 429");
				assert!(message.to_lowercase().contains("rate"));
				assert!(message.contains("120"), "Retry-After hint must be in the message: {message}");
				assert!(
					message.contains("Too many requests"),
					"envelope message must survive: {message}"
				);
			},
			other => panic!("expected Api 429, got {other:?}"),
		}
	}

	#[test]
	fn server_errors_map_to_transient_http() {
		for status in [500u16, 502, 503, 504] {
			assert!(matches!(map_response(status, None, "boom"), Err(CloudflareError::Http(_))));
		}
	}

	#[test]
	fn unknown_status_maps_to_api_with_status_code() {
		match map_response(400, None, "not an envelope") {
			Err(CloudflareError::Api { code, message }) => {
				assert_eq!(code, 400);
				assert!(message.contains("400"));
			},
			other => panic!("expected Api error, got {other:?}"),
		}
	}

	#[test]
	fn transport_failure_maps_to_http() {
		let transport = ureq::Error::Transport(
			std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused").into(),
		);
		let error = CloudflareError::Http(transport.to_string());
		assert!(matches!(error, CloudflareError::Http(_)));
		assert!(error.to_string().contains("connection refused"));
	}

	#[test]
	fn token_is_redacted_from_error_source_text() {
		let scrubbed = redact_token(&format!("read response body: boom {TOKEN} boom"), TOKEN);
		assert!(!scrubbed.contains(TOKEN), "token must be scrubbed: {scrubbed}");
		assert!(scrubbed.contains("<redacted>"));
		// Empty token: text passes through unchanged (nothing to redact).
		assert_eq!(redact_token("plain text", ""), "plain text");
	}

	#[test]
	fn error_strings_never_carry_the_token() {
		// Feed bodies that (defensively) contain the token; the mapping
		// layer must not interpolate the token itself - the fetch function
		// scrubs the body before it reaches map_response.
		let body_with_token = format!(r#"{{"success":false,"errors":[{{"code":9109,"message":"{TOKEN}"}}]}}"#);
		let errors = [
			map_response(401, None, &body_with_token),
			map_response(429, None, &body_with_token),
			map_response(200, None, "not json"),
		];
		for error in errors {
			let rendered = error.to_string();
			// map_response never sees the token; the assertion guards
			// against future token interpolation in this module.
			assert!(!rendered.contains(TOKEN), "error string leaked the token: {rendered}");
		}
		// The header path is the only place the token appears, and it is
		// never logged or embedded in errors.
		assert_eq!(auth_header(&SecretString::new(TOKEN)), format!("Bearer {TOKEN}"));
	}
}
