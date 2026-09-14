//! Fetch - live Workers AI model catalog retrieval over HTTPS.
//!
//! Typed blocking GET of
//! `/accounts/<id>/ai/models/search?format=openrouter&per_page=1000&hide_experimental=false&include_deprecated=false`
//! with the token carried exclusively in the
//! `Authorization: Bearer *** header.
//!
//! Pagination ("handle pagination even if Cloudflare later caps per_page"): [`fetch_catalog_from_api`] follows cursor/next-page markers -
//! `result_info.cursor`, `result_info.page`/`total_pages`, a top-level
//! `cursor`, or a top-level `next` field - merging each page's `data` array
//! until the marker disappears or [`MAX_CATALOG_PAGES`] pages were pulled. A
//! payload with no pagination field behaves exactly as before (single page).
//!
//! Error taxonomy: missing token → [`CloudflareError::MissingEnv`]
//! (defensive - the core's `Config` already guarantees a non-empty token);
//! 403 → [`CloudflareError::AuthRejected`] with a scope classification from
//! the envelope code (invalid token / wrong account scope / Workers AI
//! permission / generic - see [`auth_scope_for`]); 401 → [`CloudflareError::Api`]
//! with the envelope code (defensive - the catalog endpoint rejects auth
//! failures as 403); rate limit → [`CloudflareError::Api`] code 429 plus a
//! Retry-After hint; transient 5xx → [`CloudflareError::Http`]; malformed JSON
//! or a payload without a `data` array → [`CloudflareError::NoDataArray`];
//! transport/network failure → [`CloudflareError::Http`].
//!
//! Security: the token never reaches a URL, an error string, or a log line.
//! Every text source that could feed an error (response body, transport
//! message, body-read failure) is scrubbed of the token before it is mapped.

use std::time::Duration;

use crate::auth::{AuthProvider, TOKEN_ENV};
use crate::config::SecretString;
use crate::error::{AuthScope, CloudflareError};

/// Overall per-request timeout for the catalog GET (15s).
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Safety cap on catalog pages (a misconfigured server that
/// keeps returning a cursor cannot loop forever).
const MAX_CATALOG_PAGES: usize = 10;

/// Extra query parameters beyond `AuthProvider::models_url()`:
/// experimental models ARE included (they are labeled downstream by policy,
/// never silently hidden) and deprecated models are excluded.
const EXTRA_QUERY: &str = "&hide_experimental=false&include_deprecated=false";

/// The exact catalog endpoint URL for one account.
///
/// Built on `AuthProvider::models_url()` so the base path stays
/// single-sourced; only the required query parameters are appended here.
pub fn catalog_url(account_id: &str) -> String {
	format!("{}{EXTRA_QUERY}", AuthProvider::new(account_id).models_url())
}

/// The `Authorization` header value - factored out for unit testing. This is
/// the ONLY place the token leaves [`SecretString`].
pub fn auth_header(token: &SecretString) -> String {
	format!("Bearer {}", token.as_ref())
}

/// Fetch the live catalog from the Cloudflare API, following pagination.
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
	let header = auth_header(token);
	let base_url = catalog_url(account_id);

	// First page, then follow any cursor/next-page marker up to the cap.
	let first = fetch_page(&agent, &base_url, &header, token.as_ref())?;
	collect_pages(first, |marker| {
		let next_url = append_next_page(&base_url, marker);
		fetch_page(&agent, &next_url, &header, token.as_ref())
	})
}

/// One catalog GET: send, read, scrub, and map the response. `url` never
/// carries the token; `auth` is the pre-built Bearer header.
fn fetch_page(agent: &ureq::Agent, url: &str, auth: &str, token: &str) -> Result<serde_json::Value, CloudflareError> {
	let request = agent.get(url).set("Authorization", auth).set("Accept", "application/json");

	// ureq 2 returns non-2xx as `Error::Status` - both arms carry the
	// response we need to map the error envelope.
	let (status, response) = match request.call() {
		Ok(response) => (response.status(), response),
		Err(ureq::Error::Status(status, response)) => (status, response),
		Err(transport) => {
			let message = map_transport_error(&transport).to_string();
			return Err(CloudflareError::Http(redact_token(&message, token)));
		},
	};

	let retry_after = response.header("Retry-After").map(str::to_string);
	let body = response
		.into_string()
		.map_err(|error| CloudflareError::Http(redact_token(&format!("read response body: {error}"), token)))?;
	// The body is scrubbed before it can reach any error string - Cloudflare
	// never echoes the token, but defense-in-depth costs nothing.
	let body = redact_token(&body, token);
	map_response(status, retry_after.as_deref(), &body)
}

/// Pure pagination driver: merge page `data` arrays, following the marker on
/// the newest page until there is none or [`MAX_CATALOG_PAGES`] pages were
/// pulled. `fetch_next` fetches one follow-up page for a marker.
fn collect_pages<F>(first: serde_json::Value, mut fetch_next: F) -> Result<serde_json::Value, CloudflareError>
where
	F: FnMut(&NextPageMarker) -> Result<serde_json::Value, CloudflareError>,
{
	let mut merged = first;
	let mut pages = 1usize;
	while pages < MAX_CATALOG_PAGES {
		let Some(marker) = next_page_marker(&merged) else { break };
		let next = fetch_next(&marker)?;
		merged = merge_data_arrays(merged, next);
		pages += 1;
	}
	Ok(merged)
}

/// Merge two catalog pages: the newer page's metadata wins (so the next-page
/// marker naturally advances), and the `data` arrays are concatenated.
fn merge_data_arrays(first: serde_json::Value, second: serde_json::Value) -> serde_json::Value {
	let mut data: Vec<serde_json::Value> =
		first.get("data").and_then(|data| data.as_array()).cloned().unwrap_or_default();
	if let Some(second_data) = second.get("data").and_then(|data| data.as_array()) {
		data.extend(second_data.iter().cloned());
	}
	let mut merged = second;
	merged["data"] = serde_json::Value::Array(data);
	merged
}

/// A detected next-page marker, ready to be appended as a query parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NextPageMarker {
	/// An opaque cursor token - append `&cursor=<token>`.
	Cursor(String),
	/// A numeric page index - append `&page=<n>`.
	Page(u64),
}

/// Extract the next-page marker from a catalog payload, if any. Shapes:
/// `result_info.cursor` (opaque token), `result_info.page` + `total_pages`
/// (numeric), a top-level `cursor`, or a top-level `next` field. An empty
/// string or a last page yields `None` (single page).
fn next_page_marker(payload: &serde_json::Value) -> Option<NextPageMarker> {
	if let Some(result_info) = payload.get("result_info").filter(|info| info.is_object()) {
		if let Some(cursor) = result_info.get("cursor").and_then(|cursor| cursor.as_str()) {
			if !cursor.is_empty() {
				return Some(NextPageMarker::Cursor(cursor.to_string()));
			}
		}
		if let (Some(page), Some(total_pages)) = (
			result_info.get("page").and_then(|page| page.as_u64()),
			result_info.get("total_pages").and_then(|total| total.as_u64()),
		) {
			if page < total_pages {
				return Some(NextPageMarker::Page(page + 1));
			}
		}
	}
	if let Some(cursor) = payload.get("cursor").and_then(|cursor| cursor.as_str()) {
		if !cursor.is_empty() {
			return Some(NextPageMarker::Cursor(cursor.to_string()));
		}
	}
	if let Some(next) = payload.get("next").and_then(|next| next.as_str()) {
		if !next.is_empty() {
			return Some(NextPageMarker::Cursor(next.to_string()));
		}
	}
	None
}

/// Build the follow-up page URL from the base catalog URL and a marker.
fn append_next_page(base: &str, marker: &NextPageMarker) -> String {
	match marker {
		NextPageMarker::Cursor(cursor) => format!("{base}&cursor={cursor}"),
		NextPageMarker::Page(page) => format!("{base}&page={page}"),
	}
}

/// Map an HTTP status + body to the typed error taxonomy.
///
/// Factored as a pure function so the mapping is unit-testable with injected
/// inputs (no network).
fn map_response(status: u16, retry_after: Option<&str>, body: &str) -> Result<serde_json::Value, CloudflareError> {
	match status {
		200 => parse_payload(body),
		401 => Err(envelope_api_error(401, body)),
		403 => Err(forbidden_error(body)),
		429 => Err(rate_limit_error(retry_after, body)),
		500..=599 => Err(CloudflareError::Http(format!(
			"Cloudflare API returned HTTP {status} (transient server error)"
		))),
		other => Err(envelope_api_error(other, body)),
	}
}

/// Parse a 200 payload; malformed JSON or a missing `data` array are both
/// [`CloudflareError::NoDataArray`] ("invalid model payload").
fn parse_payload(body: &str) -> Result<serde_json::Value, CloudflareError> {
	let value: serde_json::Value = serde_json::from_str(body).map_err(|_| CloudflareError::NoDataArray)?;
	match value.get("data") {
		Some(serde_json::Value::Array(_)) => Ok(value),
		_ => Err(CloudflareError::NoDataArray),
	}
}

/// Extract the Cloudflare envelope `errors[0]` code + message, falling back to
/// the HTTP status and a generic message when the body is not an envelope.
fn envelope_parts(status: u16, body: &str) -> (u32, String) {
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
			return (code, message);
		}
	}
	(u32::from(status), format!("HTTP {status}"))
}

/// Build an `Api` error from the Cloudflare envelope `errors[0]` when
/// present, else fall back to the HTTP status as the code.
fn envelope_api_error(status: u16, body: &str) -> CloudflareError {
	let (code, message) = envelope_parts(status, body);
	CloudflareError::Api { code, message }
}

/// Map a 403 rejection to a distinct, actionable [`CloudflareError::AuthRejected`]
/// using the envelope code + message (invalid-token / wrong-account-scope /
/// Workers-AI-permission distinguished).
fn forbidden_error(body: &str) -> CloudflareError {
	let (code, message) = envelope_parts(403, body);
	CloudflareError::AuthRejected { kind: auth_scope_for(code, &message), code, message }
}

/// Classify a 403 envelope `(code, message)` into the auth scope taxonomy.
/// Envelope codes take precedence (9109 invalid token, 9103 wrong account
/// scope, 10000 permission); the message is the fallback when the code is
/// absent/unknown.
fn auth_scope_for(code: u32, message: &str) -> AuthScope {
	let lower = message.to_lowercase();
	match code {
		9109 => return AuthScope::InvalidToken,
		9103 => return AuthScope::WrongAccountScope,
		10000 => {
			// 10000 is Cloudflare's generic permission/authentication code;
			// refine it to the account-scope bucket when the message says so.
			if lower.contains("account") && (lower.contains("scope") || lower.contains("not found")) {
				return AuthScope::WrongAccountScope;
			}
			return AuthScope::WorkersAiPermission;
		},
		_ => {},
	}
	if lower.contains("invalid")
		&& (lower.contains("token") || lower.contains("credential") || lower.contains("authorization"))
	{
		return AuthScope::InvalidToken;
	}
	if lower.contains("permission")
		|| lower.contains("insufficient")
		|| lower.contains("workers ai")
		|| lower.contains("workers-ai")
	{
		return AuthScope::WorkersAiPermission;
	}
	if lower.contains("account") {
		return AuthScope::WrongAccountScope;
	}
	AuthScope::GenericAuth
}

/// Rate-limit error: always `Api` code 429 with the
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

/// Map a ureq transport failure to a typed [`CloudflareError::Http`] error.
/// The message is token-scrubbed by the caller before it is ever wrapped.
fn map_transport_error(error: &ureq::Error) -> CloudflareError {
	CloudflareError::Http(error.to_string())
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
	fn catalog_url_is_exact_endpoint() {
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
		// ureq exposes `From<io::Error> for Error` (a transport failure).
		let transport: ureq::Error =
			std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused").into();
		let error = map_transport_error(&transport);
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
		// Production order: fetch_catalog_from_api scrubs the response body
		// with redact_token BEFORE map_response sees it. Mirror that here -
		// even a body that (defensively) contains the token must never reach
		// an error string.
		let body_with_token = format!(r#"{{"success":false,"errors":[{{"code":9109,"message":"{TOKEN}"}}]}}"#);
		let scrubbed = redact_token(&body_with_token, TOKEN);
		assert!(!scrubbed.contains(TOKEN), "body must be scrubbed before mapping: {scrubbed}");
		assert!(scrubbed.contains("<redacted>"));
		let errors = [
			map_response(401, None, &scrubbed).unwrap_err(),
			map_response(403, None, &scrubbed).unwrap_err(),
			map_response(429, None, &scrubbed).unwrap_err(),
			map_response(200, None, "not json").unwrap_err(),
			CloudflareError::Http(redact_token(&format!("transport failure near {TOKEN}"), TOKEN)),
		];
		for error in errors {
			let rendered = error.to_string();
			assert!(!rendered.contains(TOKEN), "error string leaked the token: {rendered}");
		}
		// The header path is the only place the token appears, and it is
		// never logged or embedded in errors.
		assert_eq!(auth_header(&SecretString::new(TOKEN)), format!("Bearer {TOKEN}"));
	}

	// ------------------------------------------------------------------
	// pagination
	// ------------------------------------------------------------------

	#[test]
	fn collect_pages_single_page_without_marker_is_returned_unchanged() {
		let first = serde_json::json!({ "data": [{"id": "only"}] });
		let mut called = false;
		let result = collect_pages(first, |_marker| {
			called = true;
			Err(CloudflareError::NoDataArray)
		})
		.expect("single page collects");
		assert!(!called, "no follow-up page may be requested when there is no marker");
		assert_eq!(result["data"][0]["id"], "only");
	}

	#[test]
	fn collect_pages_merges_data_across_pages_until_no_marker() {
		let first = serde_json::json!({
			"data": [{"id": "m1"}],
			"result_info": {"cursor": "page-2"}
		});
		let pages = [
			serde_json::json!({"data": [{"id": "m2"}], "result_info": {"cursor": "page-3"}}),
			serde_json::json!({"data": [{"id": "m3"}]}),
		];
		let idx = std::cell::Cell::new(0usize);
		let result = collect_pages(first, |_marker| {
			let i = idx.get();
			idx.set(i + 1);
			Ok(pages[i].clone())
		})
		.expect("pages collect");
		let ids: Vec<&str> = result["data"]
			.as_array()
			.unwrap()
			.iter()
			.map(|entry| entry["id"].as_str().unwrap())
			.collect();
		assert_eq!(ids, vec!["m1", "m2", "m3"]);
		assert_eq!(idx.get(), 2, "two follow-up pages fetched");
	}

	#[test]
	fn collect_pages_stops_at_the_safety_cap() {
		let first = serde_json::json!({"data": [{"id": "m0"}], "result_info": {"cursor": "next"}});
		// 15 more pages, each still pointing at another - the cap stops at 10.
		let pages: Vec<serde_json::Value> = (1..=15)
			.map(|n| serde_json::json!({"data": [{"id": format!("m{n}")}], "result_info": {"cursor": "next"}}))
			.collect();
		let idx = std::cell::Cell::new(0usize);
		let result = collect_pages(first, |_marker| {
			let i = idx.get();
			idx.set(i + 1);
			Ok(pages[i].clone())
		})
		.expect("pages collect to the cap");
		let data = result["data"].as_array().unwrap();
		assert_eq!(data.len(), MAX_CATALOG_PAGES, "the cap limits the merge to 10 pages");
		assert_eq!(idx.get(), MAX_CATALOG_PAGES - 1, "nine follow-up pages fetched");
	}

	#[test]
	fn next_page_marker_extracts_cursor_and_page() {
		assert_eq!(
			next_page_marker(&serde_json::json!({"result_info": {"cursor": "abc"}})),
			Some(NextPageMarker::Cursor("abc".to_string()))
		);
		assert_eq!(
			next_page_marker(&serde_json::json!({"result_info": {"page": 1, "total_pages": 3}})),
			Some(NextPageMarker::Page(2))
		);
		assert_eq!(
			next_page_marker(&serde_json::json!({"next": "token-xyz"})),
			Some(NextPageMarker::Cursor("token-xyz".to_string()))
		);
		assert_eq!(
			next_page_marker(&serde_json::json!({"cursor": "cur"})),
			Some(NextPageMarker::Cursor("cur".to_string()))
		);
		// No marker fields -> single page.
		assert_eq!(next_page_marker(&serde_json::json!({"data": [{"id": "m"}]})), None);
		// Empty marker values are not markers.
		assert_eq!(next_page_marker(&serde_json::json!({"next": ""})), None);
		assert_eq!(next_page_marker(&serde_json::json!({"result_info": {"cursor": ""}})), None);
		// Last page: page == total_pages has no next page.
		assert_eq!(
			next_page_marker(&serde_json::json!({"result_info": {"page": 3, "total_pages": 3}})),
			None
		);
	}

	#[test]
	fn append_next_page_builds_cursor_and_page_query() {
		let base = "https://api.cloudflare.com/client/v4/accounts/acct/ai/models/search?per_page=1000";
		assert_eq!(
			append_next_page(base, &NextPageMarker::Cursor("tok".to_string())),
			"https://api.cloudflare.com/client/v4/accounts/acct/ai/models/search?per_page=1000&cursor=tok"
		);
		assert_eq!(
			append_next_page(base, &NextPageMarker::Page(2)),
			"https://api.cloudflare.com/client/v4/accounts/acct/ai/models/search?per_page=1000&page=2"
		);
	}

	// ------------------------------------------------------------------
	// 403 scope taxonomy
	// ------------------------------------------------------------------

	#[test]
	fn forbidden_403_classifies_scope_by_envelope_code() {
		// 9109 = invalid token.
		let err = map_response(
			403,
			None,
			r#"{"success":false,"errors":[{"code":9109,"message":"Invalid access token"}]}"#,
		)
		.unwrap_err();
		match err {
			CloudflareError::AuthRejected { kind, code, .. } => {
				assert_eq!(kind, AuthScope::InvalidToken);
				assert_eq!(code, 9109);
			},
			other => panic!("expected AuthRejected, got {other:?}"),
		}

		// 10000 = Workers AI permission.
		let err = map_response(
			403,
			None,
			r#"{"success":false,"errors":[{"code":10000,"message":"insufficient permission"}]}"#,
		)
		.unwrap_err();
		match err {
			CloudflareError::AuthRejected { kind, code, .. } => {
				assert_eq!(kind, AuthScope::WorkersAiPermission);
				assert_eq!(code, 10000);
			},
			other => panic!("expected AuthRejected, got {other:?}"),
		}

		// 9103 = wrong account scope.
		let err = map_response(
			403,
			None,
			r#"{"success":false,"errors":[{"code":9103,"message":"Account not found"}]}"#,
		)
		.unwrap_err();
		match err {
			CloudflareError::AuthRejected { kind, code, .. } => {
				assert_eq!(kind, AuthScope::WrongAccountScope);
				assert_eq!(code, 9103);
			},
			other => panic!("expected AuthRejected, got {other:?}"),
		}
	}

	#[test]
	fn forbidden_403_without_known_code_is_generic_auth() {
		let err =
			map_response(403, None, r#"{"success":false,"errors":[{"code":9999,"message":"forbidden"}]}"#).unwrap_err();
		match err {
			CloudflareError::AuthRejected { kind, code, .. } => {
				assert_eq!(kind, AuthScope::GenericAuth);
				assert_eq!(code, 9999);
			},
			other => panic!("expected AuthRejected, got {other:?}"),
		}
		// A non-envelope 403 body falls back to the HTTP status as the code.
		let err = map_response(403, None, "not json").unwrap_err();
		match err {
			CloudflareError::AuthRejected { kind, code, .. } => {
				assert_eq!(kind, AuthScope::GenericAuth);
				assert_eq!(code, 403);
			},
			other => panic!("expected AuthRejected, got {other:?}"),
		}
	}

	#[test]
	fn auth_scope_for_classifies_by_code_then_message() {
		assert_eq!(auth_scope_for(9109, "Invalid access token"), AuthScope::InvalidToken);
		assert_eq!(auth_scope_for(10000, "insufficient permission"), AuthScope::WorkersAiPermission);
		assert_eq!(auth_scope_for(9103, "Account not found"), AuthScope::WrongAccountScope);
		// 10000 + account message refines to the account-scope bucket.
		assert_eq!(
			auth_scope_for(10000, "token is not scoped to this account"),
			AuthScope::WrongAccountScope
		);
		// Message-based fallback when the code is absent/unknown.
		assert_eq!(auth_scope_for(0, "invalid API token"), AuthScope::InvalidToken);
		assert_eq!(
			auth_scope_for(0, "requires the Workers AI permission"),
			AuthScope::WorkersAiPermission
		);
		assert_eq!(
			auth_scope_for(0, "token is not scoped to this account"),
			AuthScope::WrongAccountScope
		);
		assert_eq!(auth_scope_for(0, "something else entirely"), AuthScope::GenericAuth);
	}
}
