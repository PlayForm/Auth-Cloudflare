//! Observability - opt-in, local-first structured event logging and
//! secret-safe protocol redaction.
//!
//! Observability is opt-in: no invasive telemetry by default.
//! This module is **disabled by default**: [`EventLog::from_env`] returns a
//! no-op log unless `AUTH_CLOUDFLARE_OBSERVABILITY=1`. When enabled, events
//! are appended as one JSON object per line (JSONL) to a local file - never
//! shipped anywhere, never read back by this crate.
//!
//! # Privacy invariants
//!
//! - The standard event log **never** stores prompt content or full tool
//!   output. [`Event`] carries only request metadata (model id, counts,
//!   latency, cost estimate, trace id) - no user text, no secrets.
//! - [`redact`] is the shared scrubber for any text that might reach a
//!   developer log: it removes `Authorization` headers, `Bearer` tokens,
//!   `cfut_`/`cfwt_` token prefixes, `cookie` values, and
//!   `ENV_VAR=value`-style substrings.
//! - [`debug_protocol_enabled`] gates an **explicit developer-only** mode
//!   (`AUTH_CLOUDFLARE_DEBUG_PROTOCOL=1`). That mode only enables *sanitized*
//!   protocol traces - every trace must still pass through [`redact`] so
//!   `Authorization`, `Bearer` tokens, cookies, env-var values, known token
//!   prefixes, and private file contents never reach the log.

use std::io;
use std::path::{Path, PathBuf};

/// Enables the local event log when set to the exact value `"1"`.
pub const OBSERVABILITY_ENV: &str = "AUTH_CLOUDFLARE_OBSERVABILITY";
/// Overrides the event-log file path when set.
pub const EVENT_LOG_ENV: &str = "AUTH_CLOUDFLARE_EVENT_LOG";
/// Enables sanitized protocol traces when set to the exact value `"1"`.
pub const DEBUG_PROTOCOL_ENV: &str = "AUTH_CLOUDFLARE_DEBUG_PROTOCOL";
/// Default event-log file name under the `~/.hermes/auth-cloudflare/` root.
const DEFAULT_EVENT_LOG_FILE: &str = "events.jsonl";

/// One structured observability event, serialized as a single snake_case
/// JSON line.
///
/// Deliberately metadata-only: it has no field for prompt content or tool
/// output, so a plain [`Event`] can never carry user text to disk.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Event {
	/// RFC 3339 UTC timestamp, e.g. `2026-09-10T12:00:00Z`.
	pub timestamp: String,
	/// Event kind, e.g. `chat_completion`, `catalog_fetch`.
	pub event: String,
	/// Model identifier, e.g. `@cf/deepseek-ai/deepseek-v4-flash-0731`.
	pub model_id: String,
	/// Request surface, e.g. `chat_completions`.
	pub request_kind: String,
	/// Whether the request used streaming.
	pub stream: bool,
	/// Final status, e.g. an HTTP status code string.
	pub status: String,
	/// Total request latency in milliseconds.
	pub latency_ms: u64,
	/// Reported input/prompt tokens.
	pub input_tokens: u64,
	/// Reported output/completion tokens.
	pub output_tokens: u64,
	/// Estimated request cost in USD.
	pub estimated_cost_usd: f64,
	/// Number of tool calls issued during the request.
	pub tool_call_count: u64,
	/// Cache disposition, e.g. `miss`, `hit`, or empty when unknown.
	pub cache_status: String,
	/// Opaque correlation id tying related events together.
	pub trace_id: String,
}

/// Local JSONL event log. Disabled (no-op) unless explicitly enabled.
///
/// Construct with [`EventLog::from_env`] for the documented opt-in behavior,
/// or with [`EventLog::new`] / [`EventLog::disabled`] to pin a specific
/// state (tests, adapters).
#[derive(Debug, Clone)]
pub struct EventLog {
	/// `None` = disabled; `Some(path)` = append events to this JSONL file.
	path: Option<PathBuf>,
}

impl EventLog {
	/// A disabled log: [`Self::record`] is a no-op that writes nothing.
	pub fn disabled() -> Self {
		Self { path: None }
	}

	/// An enabled log pinned to a specific JSONL file path.
	pub fn new(path: impl Into<PathBuf>) -> Self {
		Self { path: Some(path.into()) }
	}

	/// Resolve from the environment.
	///
	/// Enabled iff `AUTH_CLOUDFLARE_OBSERVABILITY` is exactly `"1"`. The log
	/// path is `AUTH_CLOUDFLARE_EVENT_LOG` when set, otherwise
	/// `~/.hermes/auth-cloudflare/events.jsonl` (respecting `HERMES_HOME`).
	/// Any other value - including unset - yields a disabled no-op log.
	pub fn from_env() -> Self {
		if env_flag_is_one(OBSERVABILITY_ENV) {
			let path = std::env::var(EVENT_LOG_ENV)
				.ok()
				.map(|v| v.trim().to_string())
				.filter(|v| !v.is_empty())
				.map(PathBuf::from)
				.unwrap_or_else(default_log_path);
			Self::new(path)
		} else {
			Self::disabled()
		}
	}

	/// True when events will actually be written.
	pub fn is_enabled(&self) -> bool {
		self.path.is_some()
	}

	/// The log file path, when enabled.
	pub fn path(&self) -> Option<&Path> {
		self.path.as_deref()
	}

	/// Append one event as a single JSON line.
	///
	/// No-op (returns `Ok(())`) when disabled. When enabled, the parent
	/// directory is created if needed, the file is opened in append mode,
	/// and the JSON object plus trailing newline is written in a single
	/// `write_all`. The file is created user-private (mode `0o600` on Unix).
	pub fn record(&self, event: Event) -> Result<(), io::Error> {
		let Some(path) = &self.path else {
			return Ok(());
		};
		use std::io::Write;
		let mut line = serde_json::to_string(&event).map_err(io::Error::other)?;
		line.push('\n');
		if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
			std::fs::create_dir_all(parent)?;
		}
		let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
		file.write_all(line.as_bytes())?;
		// 0o600 - user-private operational metadata.
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
		}
		Ok(())
	}
}

/// True when `AUTH_CLOUDFLARE_DEBUG_PROTOCOL` is exactly `"1"` - the
/// developer-only gate for *sanitized* protocol traces. Any other value
/// (including `"true"`, `"0"`, or unset) is `false`.
pub fn debug_protocol_enabled() -> bool {
	env_flag_is_one(DEBUG_PROTOCOL_ENV)
}

/// Scrub secret-bearing text for a developer log or protocol trace.
///
/// Removes (case-insensitively):
/// - `Authorization: <value>` headers - including the `Authorization` key
///   itself - and a bare `Authorization` word;
/// - `Bearer <token>` credential values (the `Bearer` keyword is kept, the
///   token is replaced);
/// - standalone `cfut_*` / `cfwt_*` token values;
/// - `cookie` header/attribute values;
/// - `ENV_VAR=value`-style assignments (the value is replaced).
///
/// The caller is responsible for routing prompt content and tool output
/// through the debug-protocol path only when [`debug_protocol_enabled`] is
/// true - and even then through this function.
pub fn redact(text: &str) -> String {
	let mut out = redact_authorization(text);
	out = redact_bearer(&out);
	out = redact_known_tokens(&out);
	out = redact_cookie(&out);
	redact_env_values(&out)
}

/// `AUTH_CLOUDFLARE_*` flag comparison: only the exact string `"1"` counts.
fn env_flag_is_one(name: &str) -> bool {
	std::env::var(name).map(|v| v == "1").unwrap_or(false)
}

/// Default log path: `$HERMES_HOME/auth-cloudflare/events.jsonl`, with
/// `HERMES_HOME` falling back to `~/.hermes` - matching `crate::config`.
fn default_log_path() -> PathBuf {
	hermes_home().join("auth-cloudflare").join(DEFAULT_EVENT_LOG_FILE)
}

/// Resolve the Hermes config root: `$HERMES_HOME` or `~/.hermes`.
fn hermes_home() -> PathBuf {
	std::env::var(crate::cache::HERMES_HOME_ENV)
		.ok()
		.map(|v| v.trim().to_string())
		.filter(|v| !v.is_empty())
		.map(PathBuf::from)
		.unwrap_or_else(|| {
			std::env::var("HOME")
				.ok()
				.map(PathBuf::from)
				.unwrap_or_else(|| PathBuf::from("~"))
				.join(".hermes")
		})
}

/// Byte length of the UTF-8 sequence starting at `b` (all scanned input is
/// ASCII; this keeps slicing safe on the rare multibyte byte).
fn utf8_len(b: u8) -> usize {
	if b < 0x80 {
		1
	} else if b >> 5 == 0b110 {
		2
	} else if b >> 4 == 0b1110 {
		3
	} else if b >> 3 == 0b11110 {
		4
	} else {
		1
	}
}

/// True for a continuation byte of a `cfut_`/`cfwt_` token.
fn is_token_char(b: u8) -> bool {
	b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_ident_start(b: u8) -> bool {
	b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
	b.is_ascii_alphanumeric() || b == b'_'
}

/// Redact `Authorization` headers (key + value) and the bare word.
fn redact_authorization(text: &str) -> String {
	let bytes = text.as_bytes();
	let needle = b"Authorization";
	let mut out = String::with_capacity(text.len());
	let mut i = 0;
	while i < bytes.len() {
		if i + needle.len() <= bytes.len() && bytes[i..i + needle.len()].eq_ignore_ascii_case(needle) {
			let j = i + needle.len();
			let mut k = j;
			while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
				k += 1;
			}
			if k < bytes.len() && bytes[k] == b':' {
				// Header form: consume the value to end of line.
				k += 1;
				while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
					k += 1;
				}
				while k < bytes.len() && bytes[k] != b'\n' && bytes[k] != b'\r' {
					k += 1;
				}
				out.push_str("<redacted>");
				i = k;
			} else {
				// Bare word: drop the key itself.
				out.push_str("<redacted>");
				i = j;
			}
		} else {
			let ch_len = utf8_len(bytes[i]);
			out.push_str(&text[i..i + ch_len]);
			i += ch_len;
		}
	}
	out
}

/// Redact `Bearer <token>` credential values (keeps the keyword).
fn redact_bearer(text: &str) -> String {
	let bytes = text.as_bytes();
	let needle = b"Bearer";
	let mut out = String::with_capacity(text.len());
	let mut i = 0;
	while i < bytes.len() {
		if i + needle.len() <= bytes.len() && bytes[i..i + needle.len()].eq_ignore_ascii_case(needle) {
			let j = i + needle.len();
			let mut k = j;
			while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
				k += 1;
			}
			let tok_start = k;
			while k < bytes.len()
				&& bytes[k] < 0x80
				&& !bytes[k].is_ascii_whitespace()
				&& bytes[k] != b','
				&& bytes[k] != b';'
				&& bytes[k] != b'"'
			{
				k += 1;
			}
			if k > tok_start {
				out.push_str(&text[i..tok_start]); // "Bearer" + whitespace
				out.push_str("<redacted>");
				i = k;
			} else {
				let ch_len = utf8_len(bytes[i]);
				out.push_str(&text[i..i + ch_len]);
				i += ch_len;
			}
		} else {
			let ch_len = utf8_len(bytes[i]);
			out.push_str(&text[i..i + ch_len]);
			i += ch_len;
		}
	}
	out
}

/// Redact standalone `cfut_*` / `cfwt_*` token values.
fn redact_known_tokens(text: &str) -> String {
	const PREFIXES: [&[u8]; 2] = [b"cfut_", b"cfwt_"];
	let bytes = text.as_bytes();
	let mut out = String::with_capacity(text.len());
	let mut i = 0;
	while i < bytes.len() {
		let mut matched = false;
		for prefix in PREFIXES {
			if i + prefix.len() <= bytes.len() && &bytes[i..i + prefix.len()] == prefix {
				let mut j = i + prefix.len();
				while j < bytes.len() && is_token_char(bytes[j]) {
					j += 1;
				}
				out.push_str("<redacted>");
				i = j;
				matched = true;
				break;
			}
		}
		if !matched {
			let ch_len = utf8_len(bytes[i]);
			out.push_str(&text[i..i + ch_len]);
			i += ch_len;
		}
	}
	out
}

/// Redact `cookie` header/attribute values (keeps the key).
fn redact_cookie(text: &str) -> String {
	let bytes = text.as_bytes();
	let needle = b"cookie";
	let mut out = String::with_capacity(text.len());
	let mut i = 0;
	while i < bytes.len() {
		if i + needle.len() <= bytes.len() && bytes[i..i + needle.len()].eq_ignore_ascii_case(needle) {
			let mut k = i + needle.len();
			while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
				k += 1;
			}
			if k < bytes.len() && (bytes[k] == b':' || bytes[k] == b'=') {
				k += 1;
				while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
					k += 1;
				}
				let val_start = k;
				while k < bytes.len()
					&& bytes[k] < 0x80
					&& !bytes[k].is_ascii_whitespace()
					&& bytes[k] != b','
					&& bytes[k] != b';'
					&& bytes[k] != b'"'
				{
					k += 1;
				}
				if k > val_start {
					out.push_str(&text[i..val_start]);
					out.push_str("<redacted>");
					i = k;
					continue;
				}
			}
			let ch_len = utf8_len(bytes[i]);
			out.push_str(&text[i..i + ch_len]);
			i += ch_len;
		} else {
			let ch_len = utf8_len(bytes[i]);
			out.push_str(&text[i..i + ch_len]);
			i += ch_len;
		}
	}
	out
}

/// Redact `ENV_VAR=value`-style assignments (keeps the name and `=`).
fn redact_env_values(text: &str) -> String {
	let bytes = text.as_bytes();
	let mut out = String::with_capacity(text.len());
	let mut i = 0;
	while i < bytes.len() {
		if is_ident_start(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
			let mut j = i;
			while j < bytes.len() && is_ident_char(bytes[j]) {
				j += 1;
			}
			if j < bytes.len() && bytes[j] == b'=' {
				let mut k = j + 1;
				while k < bytes.len()
					&& bytes[k] < 0x80
					&& !bytes[k].is_ascii_whitespace()
					&& bytes[k] != b','
					&& bytes[k] != b';'
				{
					k += 1;
				}
				if k > j + 1 {
					out.push_str(&text[i..j]);
					out.push('=');
					out.push_str("<redacted>");
					i = k;
					continue;
				}
			}
		}
		let ch_len = utf8_len(bytes[i]);
		out.push_str(&text[i..i + ch_len]);
		i += ch_len;
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every env var this module reads, saved/restored for isolation.
	const ALL_VARS: &[&str] = &[
		OBSERVABILITY_ENV,
		EVENT_LOG_ENV,
		DEBUG_PROTOCOL_ENV,
		crate::cache::HERMES_HOME_ENV,
		"HOME",
	];

	/// `std::env` is process-global and tests run in parallel - serialize
	/// env mutation through a static mutex and restore prior values after.
	static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	fn with_env<F, R>(vars: &[(&str, Option<&str>)], f: F) -> R
	where
		F: FnOnce() -> R,
	{
		let _guard = ENV_LOCK.lock().unwrap();
		let saved: Vec<(String, Option<String>)> =
			ALL_VARS.iter().map(|k| ((*k).to_string(), std::env::var(k).ok())).collect();
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
	fn scratch_dir(name: &str) -> PathBuf {
		std::env::temp_dir().join(format!("auth-cloudflare-observability-test-{}-{name}", std::process::id()))
	}

	fn sample_event() -> Event {
		Event {
			timestamp: "2026-09-10T12:00:00Z".to_string(),
			event: "chat_completion".to_string(),
			model_id: crate::DEFAULT_MODEL.to_string(),
			request_kind: "chat_completions".to_string(),
			stream: false,
			status: "200".to_string(),
			latency_ms: 1234,
			input_tokens: 100,
			output_tokens: 50,
			estimated_cost_usd: 0.0042,
			tool_call_count: 2,
			cache_status: "miss".to_string(),
			trace_id: "trace-0001".to_string(),
		}
	}

	#[test]
	fn default_disabled_creates_no_file() {
		let dir = scratch_dir("default-disabled");
		let log_path = dir.join("events.jsonl");
		with_env(&[(EVENT_LOG_ENV, Some(log_path.to_str().unwrap()))], || {
			let log = EventLog::from_env();
			assert!(!log.is_enabled(), "observability must be off by default");
			assert!(log.path().is_none());
			log.record(sample_event()).expect("no-op record succeeds");
		});
		assert!(!log_path.exists(), "disabled log must not create a file");
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn record_writes_one_valid_json_line() {
		let dir = scratch_dir("record");
		std::fs::create_dir_all(&dir).expect("create scratch dir");
		let path = dir.join("events.jsonl");
		let log = EventLog::new(path.clone());
		assert!(log.is_enabled());
		let event = sample_event();
		log.record(event.clone()).expect("record");
		let contents = std::fs::read_to_string(&path).expect("read log");
		let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
		assert_eq!(lines.len(), 1, "exactly one JSON line must be written");
		let parsed: Event = serde_json::from_str(lines[0]).expect("valid JSON line");
		assert_eq!(parsed, event);
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn redact_strips_bearer_cfut_and_authorization() {
		let header = "Authorization: Bearer cfut_secret_token_12345";
		let out = redact(header);
		assert!(!out.contains("Authorization"), "Authorization key must be scrubbed");
		assert!(!out.contains("Bearer cfut_secret_token_12345"), "Bearer token must be scrubbed");
		assert!(!out.contains("cfut_secret_token_12345"), "cfut_ token must be scrubbed");

		assert!(!redact("token cfut_abc123-def here").contains("cfut_abc123-def"));
		assert!(!redact("Bearer cfwt_deadbeef").contains("cfwt_deadbeef"));
		// Case-insensitive header redaction.
		assert!(!redact("authorization: bearer cfut_lower_xyz").contains("cfut_lower_xyz"));
	}

	#[test]
	fn debug_protocol_enabled_exact_one_only() {
		with_env(&[(DEBUG_PROTOCOL_ENV, None)], || {
			assert!(!debug_protocol_enabled(), "unset must be false");
		});
		with_env(&[(DEBUG_PROTOCOL_ENV, Some("1"))], || {
			assert!(debug_protocol_enabled(), "exact 1 must be true");
		});
		with_env(&[(DEBUG_PROTOCOL_ENV, Some("0"))], || {
			assert!(!debug_protocol_enabled(), "0 must be false");
		});
		with_env(&[(DEBUG_PROTOCOL_ENV, Some("true"))], || {
			assert!(!debug_protocol_enabled(), "true must be false");
		});
		with_env(&[(DEBUG_PROTOCOL_ENV, Some(" 1 "))], || {
			assert!(!debug_protocol_enabled(), "whitespace-padded must be false (exact match only)");
		});
	}

	#[test]
	fn event_serde_roundtrip_snake_case() {
		let event = sample_event();
		let json = serde_json::to_string(&event).expect("serialize");
		let back: Event = serde_json::from_str(&json).expect("deserialize");
		assert_eq!(back, event);
		assert!(json.contains("\"model_id\""), "keys must be snake_case");
		assert!(json.contains("\"latency_ms\""));
		assert!(json.contains("\"input_tokens\""));
		assert!(json.contains("\"output_tokens\""));
		assert!(json.contains("\"estimated_cost_usd\""));
		assert!(json.contains("\"tool_call_count\""));
		assert!(json.contains("\"cache_status\""));
		assert!(json.contains("\"trace_id\""));
	}
}
