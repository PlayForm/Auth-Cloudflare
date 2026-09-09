//! auth-cloudflare - the CLI binary contract (feedback 06 JSON shapes,
//! feedback 02 exit codes 0-7).
//!
//! Commands:
//!
//! ```text
//! auth-cloudflare version [--format json]
//! auth-cloudflare doctor [--format json]
//! auth-cloudflare catalog get [--format json]
//! auth-cloudflare catalog list [--format json]
//! auth-cloudflare catalog refresh [--format json]
//! auth-cloudflare catalog export yaml|markdown
//! auth-cloudflare catalog diff [--format json]
//! auth-cloudflare model inspect <model-id> [--format json]
//! auth-cloudflare policy get [--format json]
//! ```
//!
//! Offline-safety contract: `version`, `doctor`, and `policy get` never
//! touch the network. `catalog refresh` is live-first: a typed
//! `/ai/models/search` GET (through the core's `fetch` module) that writes
//! the account-scoped cache on success (exit 0), serves a stale cache when
//! the live API is unavailable (exit 4), and exits 3 with a typed error when
//! neither the API nor a cache is available. `catalog get|list|export|diff`
//! and `model inspect` are cache-first with a live fetch only when the cache
//! is absent, and the bundled `FALLBACK_MODELS` as the final offline
//! fallback.
//!
//! Security contract: the API token is only ever held by the core's
//! `SecretString` and flows exclusively into the `Authorization: Bearer ***
//! header of the catalog GET; this binary never prints, logs, or serializes
//! it. Doctor emits a redacted account id (`624acc…9f84` pattern, feedback
//! 06) and exits 7 when the user config file contains an `api_token` VALUE
//! (the config file may only name the env var holding the token - feedback
//! 02).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use auth_cloudflare::auth::{AuthProvider, ACCOUNT_ENV, TOKEN_ENV};
use auth_cloudflare::cache::{
	cache_dir_for_account, cache_is_stale, read_catalog_cache, write_catalog_cache, CatalogCacheMeta,
};
use auth_cloudflare::catalog::{ModelRecord, FALLBACK_MODELS};
use auth_cloudflare::config::{Config, SecretString};
use auth_cloudflare::error::CloudflareError;
use auth_cloudflare::fetch::{fetch_catalog_from_api, FETCH_TIMEOUT};
use auth_cloudflare::policy::ModelPolicy;
use auth_cloudflare::schema::{VersionInfo, CATALOG_SCHEMA_VERSION};
use auth_cloudflare::{DEFAULT_MODEL, VERSION};

/// Live catalog fetcher - injectable for hermetic CLI tests (the production
/// path is `fetch_catalog_from_api`; tests substitute a canned fetcher so no
/// unit test ever touches the network).
type FetchCatalog =
	fn(account_id: &str, token: &SecretString, timeout: Duration) -> Result<serde_json::Value, CloudflareError>;

// Credential resolution: the CLI resolves full credentials through the
// core's `config.rs` (`Config::from_env`) only for the live-fetch commands
// (`catalog refresh`, and the cache-miss live path of `catalog get|list|
// export|inspect`); the token is held by `SecretString` and consumed only
// by `fetch_catalog_from_api` for the Bearer header. The token-free
// `ResolvedConfig` mirror below stays the source for everything that must
// NOT see the token (doctor, cache paths, staleness).

/// Exit code - command succeeded; health checks passed.
const EXIT_OK: i32 = 0;
/// Exit code - operational error: malformed args, I/O, unexpected failure.
const EXIT_OPERATIONAL: i32 = 1;
/// Exit code - credentials missing or invalid.
const EXIT_CREDENTIALS: i32 = 2;
/// Exit code - remote Cloudflare API failure.
const EXIT_REMOTE_API: i32 = 3;
/// Exit code - live API unavailable; stale cache successfully used.
const EXIT_STALE_CACHE: i32 = 4;
/// Exit code - requested model not found / catalog has no eligible model.
const EXIT_NO_ELIGIBLE_MODEL: i32 = 5;
/// Exit code - conformance suite ran but failed acceptance (reserved in
/// v0.0.1: no conformance command wired yet). Part of the binding 0-7 table.
#[allow(dead_code)]
const EXIT_CONFORMANCE: i32 = 6;
/// Exit code - unsafe configuration / secret-leak risk detected.
const EXIT_UNSAFE_CONFIG: i32 = 7;

/// How old a cached catalog may be before `catalog get` reports it stale
/// (and exits 4 - stale cache successfully used, feedback 02).
const CACHE_MAX_AGE: Duration = Duration::from_secs(6 * 3600);

/// Canonical env vars (config.rs contract, feedback 03/06).
const ACCOUNT_ID_ENV: &str = "AUTH_CLOUDFLARE_ACCOUNT_ID";
const API_TOKEN_ENV: &str = "AUTH_CLOUDFLARE_API_TOKEN";
const BASE_URL_ENV: &str = "AUTH_CLOUDFLARE_WORKERS_AI_BASE_URL";
const CACHE_DIR_ENV: &str = "AUTH_CLOUDFLARE_CACHE_DIR";
const CONFIG_ENV: &str = "AUTH_CLOUDFLARE_CONFIG";
const LEGACY_HERMES_TOKEN_ENV: &str = "HERMES_CUSTOM_API_CLOUDFLARE_COM_API_KEY";
/// Expected Cloudflare account ID shape (config.rs contract).
const ACCOUNT_ID_LEN: usize = 32;
/// Default config file name under `$HERMES_HOME/auth-cloudflare/`.
const CONFIG_FILE_NAME: &str = "config.json";

/// Optional override for the `catalog export` output directory (dev/test
/// knob; defaults to the current working directory).
const EXPORT_DIR_ENV: &str = "AUTH_CLOUDFLARE_EXPORT_DIR";

const USAGE: &str = "\
auth-cloudflare - Cloudflare Workers AI auth provider CLI (v{version})

Usage:
  auth-cloudflare version [--format json]
  auth-cloudflare doctor [--format json]
  auth-cloudflare catalog get [--format json]
  auth-cloudflare catalog list [--format json]
  auth-cloudflare catalog refresh [--format json]
  auth-cloudflare catalog export yaml|markdown
  auth-cloudflare catalog diff [--format json]
  auth-cloudflare model inspect <model-id> [--format json]
  auth-cloudflare policy get [--format json]
  auth-cloudflare help

Exit codes (feedback 02, binding):
  0 success
  1 operational error (malformed args, I/O, unexpected failure)
  2 credentials missing or invalid
  3 remote Cloudflare API failure
  4 live API unavailable; stale cache successfully used
  5 requested model not found / no eligible model
  6 conformance suite failed (reserved in v0.0.1)
  7 unsafe configuration / secret-leak risk detected

Environment (core precedence, feedback 03/06):
  AUTH_CLOUDFLARE_ACCOUNT_ID, AUTH_CLOUDFLARE_API_TOKEN,
  AUTH_CLOUDFLARE_WORKERS_AI_BASE_URL, AUTH_CLOUDFLARE_CACHE_DIR,
  AUTH_CLOUDFLARE_CONFIG, AUTH_CLOUDFLARE_EXPORT_DIR (export target dir)
  Legacy aliases: CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_API_TOKEN,
  HERMES_CUSTOM_API_CLOUDFLARE_COM_API_KEY
";

/// One parsed CLI invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
	Version,
	Doctor,
	CatalogGet,
	CatalogList,
	CatalogRefresh,
	CatalogExport { format: ExportFormat },
	CatalogDiff,
	ModelInspect { model_id: String },
	PolicyGet,
	Help,
}

/// Export document formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportFormat {
	Yaml,
	Markdown,
}

fn main() {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let code = run(&args, &mut std::io::stdout(), &mut std::io::stderr());
	std::process::exit(code);
}

/// Execute a CLI invocation; returns the process exit code (0-7).
///
/// JSON contract output goes to `out` (stdout); human diagnostics and usage
/// go to `err` (stderr). This is the unit-testable entry point - it never
/// calls `std::process::exit`. The live fetcher is injectable (tests pass a
/// canned fetcher; the binary uses `fetch_catalog_from_api`).
fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
	run_with_fetch(args, out, err, fetch_catalog_from_api)
}

/// [`run`] with an explicit fetcher - the test seam for hermetic CLI tests.
fn run_with_fetch(args: &[String], out: &mut dyn Write, err: &mut dyn Write, fetch: FetchCatalog) -> i32 {
	let (command, format) = match parse_args(args) {
		Ok(parsed) => parsed,
		Err(message) => {
			let _ = writeln!(err, "auth-cloudflare: {message}");
			let _ = writeln!(err, "{}", USAGE.replace("{version}", VERSION));
			return EXIT_OPERATIONAL;
		},
	};
	if let Some(format) = format {
		if format != "json" {
			let _ = writeln!(
				err,
				"auth-cloudflare: unsupported --format {format}: this command only supports --format json"
			);
			return EXIT_OPERATIONAL;
		}
	}
	execute(command, out, err, fetch)
}

/// Parse args into a command. `--format <value>` / `--format=<value>` may
/// appear anywhere; everything else is positional. Returns a usage error
/// message on malformed/unknown input.
fn parse_args(args: &[String]) -> Result<(Command, Option<String>), String> {
	let mut format: Option<String> = None;
	let mut positional: Vec<String> = Vec::new();
	let mut iter = args.iter();
	while let Some(arg) = iter.next() {
		if arg == "--format" {
			let Some(value) = iter.next() else {
				return Err("--format requires a value (e.g. --format json)".to_string());
			};
			format = Some(value.clone());
		} else if let Some(value) = arg.strip_prefix("--format=") {
			format = Some(value.to_string());
		} else {
			positional.push(arg.clone());
		}
	}

	let command = match positional.as_slice() {
		[] => return Err("no command given".to_string()),
		[word] if word == "help" || word == "--help" || word == "-h" => Command::Help,
		[word] if word == "version" => Command::Version,
		[word] if word == "doctor" => Command::Doctor,
		[first, rest @ ..] if first == "catalog" => match rest {
			[sub] if sub == "get" => Command::CatalogGet,
			[sub] if sub == "list" => Command::CatalogList,
			[sub] if sub == "refresh" => Command::CatalogRefresh,
			[sub] if sub == "diff" => Command::CatalogDiff,
			[sub, export_format] if sub == "export" => {
				Command::CatalogExport { format: parse_export_format(export_format)? }
			},
			[sub] if sub == "export" => return Err("catalog export requires a format: yaml|markdown".to_string()),
			_ => return Err(format!("unknown catalog subcommand: {}", positional.join(" "))),
		},
		[first, rest @ ..] if first == "model" => match rest {
			[sub, model_id] if sub == "inspect" => Command::ModelInspect { model_id: model_id.clone() },
			[sub] if sub == "inspect" => {
				return Err(
					"model inspect requires a model id, e.g. model inspect @cf/deepseek-ai/deepseek-v4-flash-0731"
						.to_string(),
				);
			},
			_ => return Err(format!("unknown model subcommand: {}", positional.join(" "))),
		},
		[first, rest @ ..] if first == "policy" => match rest {
			[sub] if sub == "get" => Command::PolicyGet,
			_ => return Err(format!("unknown policy subcommand: {}", positional.join(" "))),
		},
		_ => return Err(format!("unknown command: {}", positional.join(" "))),
	};
	Ok((command, format))
}

fn parse_export_format(value: &str) -> Result<ExportFormat, String> {
	match value {
		"yaml" | "yml" => Ok(ExportFormat::Yaml),
		"markdown" | "md" => Ok(ExportFormat::Markdown),
		other => Err(format!("unsupported export format {other}: use yaml or markdown")),
	}
}

/// Dispatch a parsed command; returns the exit code.
fn execute(command: Command, out: &mut dyn Write, err: &mut dyn Write, fetch: FetchCatalog) -> i32 {
	match command {
		Command::Help => {
			let _ = writeln!(out, "{}", USAGE.replace("{version}", VERSION));
			EXIT_OK
		},
		Command::Version => {
			let (code, value) = version_json();
			emit_json(out, err, &value, code)
		},
		Command::Doctor => {
			let (code, value) = doctor_json();
			emit_json(out, err, &value, code)
		},
		Command::CatalogGet => {
			let (code, value) = catalog_get_json(fetch);
			emit_json(out, err, &value, code)
		},
		Command::CatalogList => {
			let (code, value) = catalog_list_json(fetch);
			emit_json(out, err, &value, code)
		},
		Command::CatalogRefresh => {
			let (code, value) = catalog_refresh_json(fetch, err);
			emit_json(out, err, &value, code)
		},
		Command::CatalogExport { format } => catalog_export(format, fetch, out, err),
		Command::CatalogDiff => {
			let (code, value) = catalog_diff_json();
			emit_json(out, err, &value, code)
		},
		Command::ModelInspect { model_id } => {
			let (code, value) = model_inspect_json(fetch, &model_id);
			emit_json(out, err, &value, code)
		},
		Command::PolicyGet => {
			let (code, value) = policy_get_json();
			emit_json(out, err, &value, code)
		},
	}
}

/// Print a JSON contract document to stdout (pretty, trailing newline) and a
/// human hint to stderr for non-zero exits.
fn emit_json(out: &mut dyn Write, err: &mut dyn Write, value: &serde_json::Value, code: i32) -> i32 {
	match serde_json::to_string_pretty(value) {
		Ok(json) => {
			let _ = writeln!(out, "{json}");
			if code != EXIT_OK {
				if let Some(message) = value.get("error").and_then(|e| e.as_str()) {
					let _ = writeln!(err, "auth-cloudflare: {message} (exit {code})");
				}
			}
		},
		Err(error) => {
			let _ = writeln!(err, "auth-cloudflare: failed to serialize JSON output: {error}");
			return EXIT_OPERATIONAL;
		},
	}
	code
}

// ---------------------------------------------------------------------------
// version
// ---------------------------------------------------------------------------

/// Feedback-06 version contract - `VersionInfo::current()` serialized.
fn version_json() -> (i32, serde_json::Value) {
	(
		EXIT_OK,
		serde_json::to_value(VersionInfo::current()).expect("VersionInfo serializes"),
	)
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

/// Feedback-06 doctor contract - never performs a network or paid inference
/// request (feedback 03). The token value never appears anywhere in the
/// output; account id is redacted as `624acc…9f84` (feedback 06).
///
/// Exit codes: 0 ok · 2 credentials missing/invalid · 7 unsafe config
/// (config file carries an `api_token` VALUE - feedback 02 forbids that).
fn doctor_json() -> (i32, serde_json::Value) {
	let resolved = resolve_config();
	let account_id = resolved.account_id.clone();
	let account_configured = account_id.is_some();
	let token_configured = resolved.token_configured;
	let unsafe_config = detect_unsafe_config(&resolved.config_path);

	let (redacted, base_url, cache_dir) = match &account_id {
		Some(id) => {
			let redacted = redact_account_id(id);
			let base_url = match &resolved.base_url_override {
				// Explicit override is user configuration, not a secret -
				// show it as-is.
				Some(url) => Some(url.clone()),
				// Derived endpoint - account id redacted.
				None => Some(AuthProvider::new(id).base_url().replace(id.as_str(), "<redacted>")),
			};
			(Some(redacted), base_url, resolved.cache_dir.clone())
		},
		None => (None, None, None),
	};

	let (cache_present, cache_age) = match &cache_dir {
		Some(dir) => match read_catalog_cache(dir) {
			Ok(Some((meta, _))) => (true, cache_age_seconds(&meta)),
			_ => (false, 0),
		},
		None => (false, 0),
	};

	let status = if account_configured && token_configured && !unsafe_config {
		"ok"
	} else {
		"error"
	};
	let exit = if unsafe_config {
		EXIT_UNSAFE_CONFIG
	} else if !account_configured || !token_configured {
		EXIT_CREDENTIALS
	} else {
		EXIT_OK
	};

	let value = serde_json::json!({
		"status": status,
		"account_id": {
			"configured": account_configured,
			"redacted": redacted,
		},
		"api_token": {
			"configured": token_configured,
			"value_redacted": true,
		},
		"endpoint": {
			"base_url": base_url,
		},
		"catalog_cache": {
			"present": cache_present,
			"age_seconds": cache_age,
		},
	});
	(exit, value)
}

// ---------------------------------------------------------------------------
// token-free configuration resolution (mirrors config.rs, no lib.rs edits)
// ---------------------------------------------------------------------------

/// Non-secret configuration resolved for the CLI. The API token VALUE is
/// deliberately never resolved into this binary - only its presence - so no
/// credential can leak through any code path, error, or serialization.
struct ResolvedConfig {
	/// Validated account id (exactly 32 ASCII hex digits), when resolvable.
	account_id: Option<String>,
	/// Whether an API token is present (canonical → legacy → file-named var).
	token_configured: bool,
	/// Account-scoped cache directory: override, file value, or derived
	/// `$HERMES_HOME/cache/auth-cloudflare/<slug>/`.
	cache_dir: Option<PathBuf>,
	/// Explicit base-url override, when configured.
	base_url_override: Option<String>,
	/// Resolved user config file path (for unsafe-config detection).
	config_path: PathBuf,
}

/// Resolve configuration through the exact config.rs precedence chain
/// (canonical `AUTH_CLOUDFLARE_*` → legacy `CLOUDFLARE_*` aliases → user
/// config file). The account id is validated for shape; whitespace-only
/// values count as missing.
fn resolve_config() -> ResolvedConfig {
	let config_path = env_nonempty(CONFIG_ENV)
		.map(PathBuf::from)
		.unwrap_or_else(|| hermes_home().join("auth-cloudflare").join(CONFIG_FILE_NAME));
	let file = read_config_file(&config_path);

	let account_id = env_nonempty(ACCOUNT_ID_ENV)
		.or_else(|| env_nonempty(ACCOUNT_ENV))
		.or_else(|| file.account_id.clone())
		.filter(|value| is_valid_account_id(value));

	let token_configured = env_nonempty(API_TOKEN_ENV)
		.or_else(|| env_nonempty(TOKEN_ENV))
		.or_else(|| env_nonempty(LEGACY_HERMES_TOKEN_ENV))
		.or_else(|| file.api_token_env.as_deref().and_then(env_nonempty))
		.is_some();

	let base_url_override = env_nonempty(BASE_URL_ENV).or_else(|| file.base_url.clone());

	let cache_dir = env_nonempty(CACHE_DIR_ENV)
		.map(PathBuf::from)
		.or_else(|| file.cache_dir.map(PathBuf::from))
		.or_else(|| account_id.as_ref().map(|id| cache_dir_for_account(&AuthProvider::new(id))));

	ResolvedConfig { account_id, token_configured, cache_dir, base_url_override, config_path }
}

/// Non-secret user config file (JSON) - same shape as config.rs's
/// FileConfig. A stray `api_token` VALUE is ignored by serde here (it is
/// flagged separately by `detect_unsafe_config`).
#[derive(Default, serde::Deserialize)]
struct ConfigFile {
	account_id: Option<String>,
	base_url: Option<String>,
	cache_dir: Option<String>,
	api_token_env: Option<String>,
}

/// Read the config file; missing/unreadable/malformed files resolve to an
/// empty config (never an error - matches config.rs).
fn read_config_file(path: &Path) -> ConfigFile {
	let Ok(raw) = std::fs::read_to_string(path) else {
		return ConfigFile::default();
	};
	serde_json::from_str(&raw).unwrap_or_default()
}

/// Read a non-empty (after trim) environment variable, if present.
fn env_nonempty(name: &str) -> Option<String> {
	std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Cloudflare account ids are 32 ASCII hex digits (config.rs contract).
fn is_valid_account_id(value: &str) -> bool {
	value.len() == ACCOUNT_ID_LEN && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Resolve `$HERMES_HOME`, falling back to `~/.hermes` (config.rs contract).
fn hermes_home() -> PathBuf {
	env_nonempty("HERMES_HOME")
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "~".to_string())).join(".hermes"))
}

/// True when the user config file carries an `api_token` VALUE. The config
/// file may only name the env var holding the token (feedback 02); a literal
/// secret value in the file is an unsafe-config/secret-leak risk (exit 7).
fn detect_unsafe_config(path: &Path) -> bool {
	let Ok(raw) = std::fs::read_to_string(path) else {
		return false;
	};
	let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
		return false;
	};
	match value.get("api_token") {
		Some(serde_json::Value::String(token)) => !token.trim().is_empty(),
		_ => false,
	}
}

/// Redact an account id as first-6 chars + ellipsis + last-4 chars
/// (`624acc…9f84` pattern, feedback 06). The account id is operational
/// metadata, not a secret - this is display hygiene for the endpoint URL.
fn redact_account_id(id: &str) -> String {
	let chars: Vec<char> = id.chars().collect();
	let n = chars.len();
	if n >= 10 {
		let head: String = chars[..6].iter().collect();
		let tail: String = chars[n - 4..].iter().collect();
		format!("{head}…{tail}")
	} else if n >= 4 {
		let head: String = chars[..2].iter().collect();
		let tail: String = chars[n - 2..].iter().collect();
		format!("{head}…{tail}")
	} else if n >= 2 {
		let head: String = chars[..1].iter().collect();
		format!("{head}…")
	} else {
		"…".to_string()
	}
}

/// Cache age in whole seconds since `fetched_at` (0 when absent/unparseable,
/// clamped at 0 - a future timestamp is not a negative age).
fn cache_age_seconds(meta: &CatalogCacheMeta) -> u64 {
	match chrono::DateTime::parse_from_rfc3339(&meta.fetched_at) {
		Ok(fetched_at) => chrono::Utc::now().signed_duration_since(fetched_at).num_seconds().max(0) as u64,
		Err(_) => 0,
	}
}

// ---------------------------------------------------------------------------
// catalog resolution (cache-first, offline-safe)
// ---------------------------------------------------------------------------

/// Resolved catalog data - one source of truth for `catalog get|list|export`
/// and `model inspect`.
struct ResolvedCatalog {
	/// `live` | `cache` | `fallback` - where the records came from.
	source: &'static str,
	/// `fresh` | `stale` | `none` (feedback 06 `cache_status`).
	cache_status: &'static str,
	/// RFC 3339 timestamp: the cache's `fetched_at`, fetch time for live, or
	/// now for fallback.
	fetched_at: String,
	/// Normalized model records.
	records: Vec<ModelRecord>,
}

/// Resolve the catalog: cache-first, live fetch when the cache is absent,
/// bundled `FALLBACK_MODELS` last. Never panics; the live fetch is skipped
/// whenever credentials do not resolve (fallback).
fn resolve_catalog_data(fetch: FetchCatalog) -> ResolvedCatalog {
	let resolved_config = resolve_config();
	// The cache dir is only meaningful with a valid account id (the derived
	// default is account-scoped); an override without credentials is not
	// trusted for catalog reads.
	let cache_dir = resolved_config.cache_dir.filter(|_| resolved_config.account_id.is_some());

	// 1. Cache present and usable -> serve it (fresh or stale, exit code
	//    decided by the caller).
	if let Some(dir) = &cache_dir {
		if let Ok(Some((meta, payload))) = read_catalog_cache(dir) {
			let records = records_from_payload(&payload);
			if !records.is_empty() {
				let stale = cache_is_stale(&meta, CACHE_MAX_AGE);
				return ResolvedCatalog {
					source: "cache",
					cache_status: if stale { "stale" } else { "fresh" },
					fetched_at: meta.fetched_at,
					records,
				};
			}
		}
	}

	// 2. No usable cache -> live fetch when full credentials resolve.
	if let Some((account_id, token)) = live_credentials() {
		if let Ok(payload) = fetch(&account_id, &token, FETCH_TIMEOUT) {
			let records = records_from_payload(&payload);
			if !records.is_empty() {
				let fetched_at = chrono::Utc::now().to_rfc3339();
				if let Some(dir) = &cache_dir {
					let meta = CatalogCacheMeta {
						schema_version: CATALOG_SCHEMA_VERSION,
						fetched_at: fetched_at.clone(),
						source: "cloudflare-workers-ai".to_string(),
						model_count: records.len(),
						account_fingerprint: AuthProvider::new(&account_id).cache_slug(),
					};
					// Best-effort: a cache write failure must not fail the
					// command - the live data is still served this run.
					let _ = write_catalog_cache(dir, &meta, &payload);
				}
				return ResolvedCatalog { source: "live", cache_status: "fresh", fetched_at, records };
			}
		}
	}

	// 3. Final offline fallback (source "fallback", cache_status "none").
	ResolvedCatalog {
		source: "fallback",
		cache_status: "none",
		fetched_at: chrono::Utc::now().to_rfc3339(),
		records: fallback_records(),
	}
}

/// Resolve full live-fetch credentials (validated account id + token) via
/// the core's `Config`; `None` when they do not resolve.
fn live_credentials() -> Option<(String, SecretString)> {
	let config = Config::from_env().ok()?;
	Some((config.account_id().to_string(), config.api_token().clone()))
}

/// Normalize an OpenRouter-format payload into model records.
fn records_from_payload(payload: &serde_json::Value) -> Vec<ModelRecord> {
	match payload.get("data").and_then(|data| data.as_array()) {
		Some(items) => items.iter().filter_map(ModelRecord::from_openrouter).collect(),
		None => Vec::new(),
	}
}

/// Build records for the bundled fallback list. Only id/name are
/// synthesized; pricing/context stay `None` (honest "no data" - never
/// invented Cloudflare fields) and capabilities come from the core's own
/// marker-based inference, so no classification logic is duplicated here.
fn fallback_records() -> Vec<ModelRecord> {
	let data: Vec<serde_json::Value> = FALLBACK_MODELS
		.iter()
		.map(|id| serde_json::json!({ "id": id, "name": display_name_from_id(id) }))
		.collect();
	records_from_payload(&serde_json::json!({ "data": data }))
}

/// Derive a display name from an id (`@cf/deepseek-ai/deepseek-v4-flash-0731`
/// → `DeepSeek V4 Flash 0731`). Pure display heuristics - never policy.
fn display_name_from_id(id: &str) -> String {
	let rest = id.strip_prefix("@cf/").unwrap_or(id);
	let model = rest.split('/').last().unwrap_or(rest);
	let mut out = String::new();
	let mut capitalize_next = true;
	let mut previous_was_digit = false;
	for ch in model.chars() {
		if ch == '-' || ch == '_' {
			out.push(' ');
			capitalize_next = true;
			previous_was_digit = false;
		} else if capitalize_next || previous_was_digit {
			out.extend(ch.to_uppercase());
			capitalize_next = false;
			previous_was_digit = ch.is_ascii_digit();
		} else {
			out.push(ch);
			previous_was_digit = ch.is_ascii_digit();
		}
	}
	out.split_whitespace()
		.map(|word| match word {
			// Brand casing where generic capitalization is wrong.
			"Deepseek" => "DeepSeek".to_string(),
			"Openai" => "OpenAI".to_string(),
			"Glm" => "GLM".to_string(),
			other => other.to_string(),
		})
		.collect::<Vec<_>>()
		.join(" ")
}

/// The feedback-06 per-model object shared by `catalog get`, `catalog
/// refresh`, and `model inspect`: id, display_name, status,
/// primary_agent_eligible, context_tokens, pricing_per_million {input,
/// cached_input, output}, capabilities {chat, tools, reasoning}.
fn model_json(record: &ModelRecord, policy: &ModelPolicy) -> serde_json::Value {
	serde_json::json!({
		"id": record.id,
		"display_name": record.display_name,
		"status": policy.status_for(&record.id),
		"primary_agent_eligible": policy.is_primary_agent_eligible(&record.id),
		"context_tokens": record.limits.context_tokens,
		"pricing_per_million": {
			"input": record.pricing.input,
			"cached_input": record.pricing.cached_input,
			"output": record.pricing.output,
		},
		"capabilities": {
			"chat": record.capabilities.chat,
			"tools": record.capabilities.tools,
			"reasoning": record.capabilities.reasoning,
		},
	})
}

/// The feedback-06 catalog envelope: schema_version, source, fetched_at,
/// cache_status, default_model, models. Used by `catalog get` (provenance
/// source `live`|`cache`|`fallback`) and `catalog refresh` (source
/// `cloudflare-workers-ai` - the catalog origin).
fn catalog_document(
	source: &str,
	cache_status: &str,
	fetched_at: String,
	records: &[ModelRecord],
) -> serde_json::Value {
	let policy = ModelPolicy::default_policy();
	let models: Vec<serde_json::Value> = records.iter().map(|record| model_json(record, &policy)).collect();
	serde_json::json!({
		"schema_version": CATALOG_SCHEMA_VERSION,
		"source": source,
		"fetched_at": fetched_at,
		"cache_status": cache_status,
		"default_model": DEFAULT_MODEL,
		"models": models,
	})
}

// ---------------------------------------------------------------------------
// catalog get / list / refresh / diff / export, model inspect, policy get
// ---------------------------------------------------------------------------

/// Feedback-06 `catalog get` envelope. Cache-first: exit 0 fresh/live/
/// fallback, exit 4 when the cache is stale (live API unavailable, stale
/// cache successfully used). A live fetch is attempted only when no usable
/// cache exists; on live failure the bundled fallback is served (exit 0,
/// cache_status "none").
fn catalog_get_json(fetch: FetchCatalog) -> (i32, serde_json::Value) {
	let resolved = resolve_catalog_data(fetch);
	let exit = match resolved.cache_status {
		"stale" => EXIT_STALE_CACHE,
		_ => EXIT_OK,
	};
	let value = catalog_document(resolved.source, resolved.cache_status, resolved.fetched_at, &resolved.records);
	(exit, value)
}

/// `catalog list` - the same resolution as `catalog get`, but models is an
/// ordered list of model ids (picker order).
fn catalog_list_json(fetch: FetchCatalog) -> (i32, serde_json::Value) {
	let resolved = resolve_catalog_data(fetch);
	let models: Vec<String> = resolved.records.iter().map(|record| record.id.clone()).collect();
	let exit = match resolved.cache_status {
		"stale" => EXIT_STALE_CACHE,
		_ => EXIT_OK,
	};
	let value = serde_json::json!({
		"schema_version": CATALOG_SCHEMA_VERSION,
		"source": resolved.source,
		"cache_status": resolved.cache_status,
		"default_model": DEFAULT_MODEL,
		"models": models,
	});
	(exit, value)
}

/// `catalog refresh` - live-first: typed `/ai/models/search` GET, cache
/// written on success (exit 0). On remote failure: serve a stale cache with
/// exit 4 (a fresh cache is still served with exit 0 - it is not stale), or
/// exit 3 with a typed error JSON when no cache exists.
fn catalog_refresh_json(fetch: FetchCatalog, err: &mut dyn Write) -> (i32, serde_json::Value) {
	let config = match Config::from_env() {
		Ok(config) => config,
		Err(error) => {
			return (
				EXIT_CREDENTIALS,
				serde_json::json!({
					"status": "error",
					"error": error.to_string(),
					"exit_code": EXIT_CREDENTIALS,
				}),
			);
		},
	};
	let account_id = config.account_id().to_string();
	let cache_dir = config.cache_dir();

	match fetch(&account_id, config.api_token(), FETCH_TIMEOUT) {
		Ok(payload) => {
			let records = records_from_payload(&payload);
			if records.is_empty() {
				// A live payload with no usable records is a remote failure
				// (invalid model payload - feedback 01 taxonomy).
				return refresh_failure(&CloudflareError::NoDataArray, &cache_dir, err);
			}
			let fetched_at = chrono::Utc::now().to_rfc3339();
			let meta = CatalogCacheMeta {
				schema_version: CATALOG_SCHEMA_VERSION,
				fetched_at: fetched_at.clone(),
				source: "cloudflare-workers-ai".to_string(),
				model_count: records.len(),
				account_fingerprint: AuthProvider::new(&account_id).cache_slug(),
			};
			if let Err(write_error) = write_catalog_cache(&cache_dir, &meta, &payload) {
				let _ = writeln!(
					err,
					"auth-cloudflare: warning: catalog refreshed but cache write failed: {write_error}"
				);
			}
			(
				EXIT_OK,
				catalog_document("cloudflare-workers-ai", "fresh", fetched_at, &records),
			)
		},
		Err(error) => refresh_failure(&error, &cache_dir, err),
	}
}

/// Shared refresh failure path: serve the account cache when one exists
/// (exit 4 when stale - "stale cache successfully used", feedback 02 exit
/// table; exit 0 when the cache is still fresh), else exit 3 with the typed
/// error JSON.
fn refresh_failure(error: &CloudflareError, cache_dir: &Path, err: &mut dyn Write) -> (i32, serde_json::Value) {
	if let Ok(Some((meta, payload))) = read_catalog_cache(cache_dir) {
		let records = records_from_payload(&payload);
		if !records.is_empty() {
			let stale = cache_is_stale(&meta, CACHE_MAX_AGE);
			let cache_status = if stale { "stale" } else { "fresh" };
			let exit = if stale { EXIT_STALE_CACHE } else { EXIT_OK };
			let _ = writeln!(
				err,
				"auth-cloudflare: live catalog fetch failed ({error}); serving {cache_status} cache (exit {exit})"
			);
			return (
				exit,
				catalog_document("cloudflare-workers-ai", cache_status, meta.fetched_at, &records),
			);
		}
	}
	(
		EXIT_REMOTE_API,
		serde_json::json!({
			"status": "error",
			"error": error.to_string(),
			"exit_code": EXIT_REMOTE_API,
		}),
	)
}

/// `catalog diff` - cache snapshot vs the bundled fallback list. Exit 1 when
/// there is no cache to diff against.
fn catalog_diff_json() -> (i32, serde_json::Value) {
	let resolved = resolve_config();
	let cache_dir = resolved.cache_dir.filter(|_| resolved.account_id.is_some());
	let Some(cache_dir) = cache_dir else {
		return (
			EXIT_OPERATIONAL,
			serde_json::json!({
				"status": "error",
				"error": "no account credentials resolved - cannot locate an account-scoped catalog cache to diff",
				"exit_code": EXIT_OPERATIONAL,
			}),
		);
	};
	let (meta, payload) = match read_catalog_cache(&cache_dir) {
		Ok(Some(found)) => found,
		Ok(None) => {
			return (
				EXIT_OPERATIONAL,
				serde_json::json!({
					"status": "error",
					"error": "no catalog cache present to diff against; run 'catalog refresh' to seed the cache",
					"exit_code": EXIT_OPERATIONAL,
				}),
			);
		},
		Err(error) => {
			return (
				EXIT_OPERATIONAL,
				serde_json::json!({
					"status": "error",
					"error": format!("catalog cache unreadable: {error}"),
					"exit_code": EXIT_OPERATIONAL,
				}),
			);
		},
	};

	let cache_ids: std::collections::BTreeSet<String> =
		records_from_payload(&payload).iter().map(|record| record.id.clone()).collect();
	let fallback_ids: std::collections::BTreeSet<String> = FALLBACK_MODELS.iter().map(|id| (*id).to_string()).collect();

	let added: Vec<String> = fallback_ids.difference(&cache_ids).cloned().collect();
	let removed: Vec<String> = cache_ids.difference(&fallback_ids).cloned().collect();
	let common = cache_ids.intersection(&fallback_ids).count();

	(
		EXIT_OK,
		serde_json::json!({
			"schema_version": CATALOG_SCHEMA_VERSION,
			"baseline": {
				"source": "cache",
				"fetched_at": meta.fetched_at,
				"model_count": cache_ids.len(),
			},
			"comparison": {
				"source": "fallback",
				"model_count": fallback_ids.len(),
			},
			"added": added,
			"removed": removed,
			"common_count": common,
		}),
	)
}

/// `catalog export yaml|markdown` - derive generated docs from the same
/// cache/live/fallback records (feedback 01: generated docs must derive from
/// the canonical catalog, never be hand-maintained duplicates). Writes
/// `catalog.generated.yaml` / `catalog.generated.md` into the export dir
/// (`AUTH_CLOUDFLARE_EXPORT_DIR`, default cwd).
fn catalog_export(format: ExportFormat, fetch: FetchCatalog, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
	let resolved = resolve_catalog_data(fetch);
	let policy = ModelPolicy::default_policy();
	let export_dir = std::env::var(EXPORT_DIR_ENV)
		.ok()
		.filter(|v| !v.trim().is_empty())
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from("."));
	let (file_name, contents) = match format {
		ExportFormat::Yaml => (
			"catalog.generated.yaml",
			export_yaml(&resolved.records, resolved.source, &resolved.fetched_at, &policy),
		),
		ExportFormat::Markdown => (
			"catalog.generated.md",
			export_markdown(&resolved.records, resolved.source, &resolved.fetched_at, &policy),
		),
	};
	let path = export_dir.join(file_name);
	if let Err(error) = std::fs::write(&path, contents) {
		let _ = writeln!(err, "auth-cloudflare: failed to write {}: {error}", path.display());
		return EXIT_OPERATIONAL;
	}
	let _ = writeln!(
		out,
		"wrote {} ({} models, source: {}, cache_status: {})",
		path.display(),
		resolved.records.len(),
		resolved.source,
		resolved.cache_status
	);
	EXIT_OK
}

/// `model inspect <id>` - the feedback-06 per-model object plus its source.
/// Exit 5 when the id is not in the catalog (feedback 02: no eligible model).
fn model_inspect_json(fetch: FetchCatalog, model_id: &str) -> (i32, serde_json::Value) {
	let resolved = resolve_catalog_data(fetch);
	let policy = ModelPolicy::default_policy();
	let Some(record) = resolved.records.iter().find(|record| record.id == model_id) else {
		return (
			EXIT_NO_ELIGIBLE_MODEL,
			serde_json::json!({
				"status": "error",
				"error": format!("model '{model_id}' not found in catalog"),
				"model_id": model_id,
				"exit_code": EXIT_NO_ELIGIBLE_MODEL,
			}),
		);
	};
	let mut value = model_json(record, &policy);
	value["source"] = serde_json::Value::String(resolved.source.to_string());
	(EXIT_OK, value)
}

/// `policy get` - the core's bundled policy document, serialized as-is.
fn policy_get_json() -> (i32, serde_json::Value) {
	let policy = ModelPolicy::default_policy();
	(EXIT_OK, serde_json::to_value(policy).expect("ModelPolicy serializes"))
}

// ---------------------------------------------------------------------------
// export document builders (hand-rolled; no serde_yaml dependency)
// ---------------------------------------------------------------------------

/// `catalog.generated.yaml` - user-copyable Hermes fragment (feedback 02
/// shape), derived from the same records as `catalog get`.
fn export_yaml(records: &[ModelRecord], source: &str, fetched_at: &str, policy: &ModelPolicy) -> String {
	let mut s = String::new();
	s.push_str("# GENERATED FILE - do not edit by hand.\n");
	s.push_str(&format!("# Source: Cloudflare Workers AI catalog ({source})\n"));
	s.push_str(&format!("# Refreshed: {fetched_at}\n"));
	s.push_str("# Plugin policy: model-policy.yaml\n");
	s.push_str("# Verification: model-health.json\n");
	s.push_str("\nmodels:\n");
	for record in records {
		let status = policy.status_for(&record.id);
		let status_str = enum_str(&status);
		let marker: String = if record.id == DEFAULT_MODEL {
			"DEFAULT".to_string()
		} else {
			status_str.clone()
		};
		s.push_str(&format!("	# {marker} | {}\n", record.display_name));
		s.push_str(&format!("	# Status: {status_str}\n"));
		match record.limits.context_tokens {
			Some(tokens) => s.push_str(&format!("	# Context: {tokens} tokens\n")),
			None => s.push_str("	# Context: unknown\n"),
		}
		let price = |value: Option<f64>| match value {
			Some(v) => format!("${v:.2}/M"),
			None => "unknown".to_string(),
		};
		s.push_str(&format!(
			"	# Price: {} input; {} cached input; {} output\n",
			price(record.pricing.input),
			price(record.pricing.cached_input),
			price(record.pricing.output)
		));
		s.push_str(&format!("	# Tools: {}\n", enum_str(&record.capabilities.tools)));
		s.push_str(&format!("	# Reasoning: {}\n", enum_str(&record.capabilities.reasoning)));
		s.push_str(&format!("	- \"{}\"\n", record.id));
	}
	s
}

/// `catalog.generated.md` - summary + model table (feedback 02 shape).
fn export_markdown(records: &[ModelRecord], source: &str, fetched_at: &str, policy: &ModelPolicy) -> String {
	let eligible = records
		.iter()
		.filter(|record| policy.is_primary_agent_eligible(&record.id))
		.count();
	let mut s = String::new();
	s.push_str("# Cloudflare Workers AI catalog\n\n");
	s.push_str(&format!("Generated: {fetched_at}\n"));
	s.push_str(&format!("Plugin: {VERSION}\n"));
	s.push_str(&format!("Catalog: {source}\n"));
	s.push_str(&format!("Cache age: {} seconds\n", doc_cache_age_seconds(fetched_at)));
	s.push_str(&format!("Models found: {}\n", records.len()));
	s.push_str(&format!("Primary-agent eligible: {eligible}\n\n"));
	s.push_str("| Model | Status | Context | Input/M | Cached/M | Output/M | Tools | Reasoning |\n");
	s.push_str("| --- | --- | ---: | ---: | ---: | ---: | --- | --- |\n");
	for record in records {
		let status_str = enum_str(&policy.status_for(&record.id));
		let context = record
			.limits
			.context_tokens
			.map(|tokens| format!("{tokens}"))
			.unwrap_or_else(|| "-".to_string());
		let price = |value: Option<f64>| match value {
			Some(v) => format!("${v:.2}"),
			None => "-".to_string(),
		};
		s.push_str(&format!(
			"| {} | {} | {} | {} | {} | {} | {} | {} |\n",
			record.display_name,
			status_str,
			context,
			price(record.pricing.input),
			price(record.pricing.cached_input),
			price(record.pricing.output),
			enum_str(&record.capabilities.tools),
			enum_str(&record.capabilities.reasoning)
		));
	}
	s
}

/// snake_case string for a serde-serializable enum (uses the same serde
/// vocabulary as the JSON contract - never a hand-written duplicate).
fn enum_str<T: serde::Serialize>(value: &T) -> String {
	serde_json::to_value(value)
		.ok()
		.and_then(|value| value.as_str().map(str::to_string))
		.unwrap_or_else(|| "unknown".to_string())
}

/// Seconds between `fetched_at` and now for the generated docs header.
fn doc_cache_age_seconds(fetched_at: &str) -> u64 {
	match chrono::DateTime::parse_from_rfc3339(fetched_at) {
		Ok(timestamp) => chrono::Utc::now().signed_duration_since(timestamp).num_seconds().max(0) as u64,
		Err(_) => 0,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use auth_cloudflare::auth::{ACCOUNT_ENV, TOKEN_ENV};

	/// Every env var the CLI or core reads, saved/restored for isolation.
	const ALL_VARS: &[&str] = &[
		"AUTH_CLOUDFLARE_ACCOUNT_ID",
		"AUTH_CLOUDFLARE_API_TOKEN",
		"AUTH_CLOUDFLARE_WORKERS_AI_BASE_URL",
		"AUTH_CLOUDFLARE_CACHE_DIR",
		"AUTH_CLOUDFLARE_CONFIG",
		"AUTH_CLOUDFLARE_EXPORT_DIR",
		"CLOUDFLARE_ACCOUNT_ID",
		"CLOUDFLARE_API_TOKEN",
		"HERMES_CUSTOM_API_CLOUDFLARE_COM_API_KEY",
		"HERMES_HOME",
		"HOME",
	];

	/// Synthetic Cloudflare-shaped account id (32 hex digits) - never real.
	const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
	/// Synthetic token - never a real credential.
	const TOKEN: &str = "cfut_test_synthetic_token_0001";

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
	fn scratch_dir(name: &str) -> PathBuf {
		std::env::temp_dir().join(format!("auth-cloudflare-cli-test-{}-{name}", std::process::id()))
	}

	/// Run the CLI, returning (exit code, parsed stdout JSON or empty object).
	fn run_json(args: &[&str]) -> (i32, serde_json::Value) {
		let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
		let mut out: Vec<u8> = Vec::new();
		let mut err: Vec<u8> = Vec::new();
		let code = run(&owned, &mut out, &mut err);
		let value = serde_json::from_slice(&out).unwrap_or_else(|_| serde_json::json!({}));
		(code, value)
	}

	/// Run the CLI, returning (exit code, raw stdout text).
	fn run_raw(args: &[&str]) -> (i32, String) {
		let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
		let mut out: Vec<u8> = Vec::new();
		let mut err: Vec<u8> = Vec::new();
		let code = run(&owned, &mut out, &mut err);
		(code, String::from_utf8_lossy(&out).to_string())
	}

	/// Run the CLI with an injected fetcher (hermetic live-fetch tests).
	fn run_json_with_fetch(args: &[&str], fetch: FetchCatalog) -> (i32, serde_json::Value) {
		let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
		let mut out: Vec<u8> = Vec::new();
		let mut err: Vec<u8> = Vec::new();
		let code = run_with_fetch(&owned, &mut out, &mut err, fetch);
		let value = serde_json::from_slice(&out).unwrap_or_else(|_| serde_json::json!({}));
		(code, value)
	}

	/// Run the CLI with an injected fetcher, returning raw stdout.
	fn run_raw_with_fetch(args: &[&str], fetch: FetchCatalog) -> (i32, String) {
		let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
		let mut out: Vec<u8> = Vec::new();
		let mut err: Vec<u8> = Vec::new();
		let code = run_with_fetch(&owned, &mut out, &mut err, fetch);
		(code, String::from_utf8_lossy(&out).to_string())
	}

	/// Canned successful fetcher: a small OpenRouter-format live payload.
	fn fetch_ok(
		_account_id: &str,
		_token: &SecretString,
		_timeout: Duration,
	) -> Result<serde_json::Value, CloudflareError> {
		Ok(serde_json::json!({
			"data": [
				{
					"id": DEFAULT_MODEL,
					"name": "DeepSeek V4 Flash 0731",
					"context_length": 1_310_720,
					"pricing": { "prompt": "0.00000044", "completion": "0.00000132" },
				},
				{ "id": "@cf/moonshotai/kimi-k2.7-code", "name": "Kimi K2.7 Code" },
			]
		}))
	}

	/// Canned failing fetcher: a transport-style remote failure.
	fn fetch_err(
		_account_id: &str,
		_token: &SecretString,
		_timeout: Duration,
	) -> Result<serde_json::Value, CloudflareError> {
		Err(CloudflareError::Http("simulated network failure".to_string()))
	}

	/// Seed the account-scoped cache for the synthetic account/token env.
	fn seed_cache(fetched_at: &str, model_ids: &[&str]) {
		let dir = cache_dir_for_account(&AuthProvider::new(ACCOUNT));
		let data: Vec<serde_json::Value> =
			model_ids.iter().map(|id| serde_json::json!({ "id": id, "name": id })).collect();
		let payload = serde_json::json!({ "data": data });
		let meta = CatalogCacheMeta {
			schema_version: CATALOG_SCHEMA_VERSION,
			fetched_at: fetched_at.to_string(),
			source: "cloudflare-workers-ai".to_string(),
			model_count: model_ids.len(),
			account_fingerprint: "0123456789abcdef".to_string(),
		};
		auth_cloudflare::cache::write_catalog_cache(&dir, &meta, &payload).expect("seed cache");
	}

	// ------------------------------------------------------------------
	// arg parsing
	// ------------------------------------------------------------------

	#[test]
	fn parses_every_command() {
		let cases: &[(&[&str], Command)] = &[
			(&["version"], Command::Version),
			(&["version", "--format", "json"], Command::Version),
			(&["--format", "json", "version"], Command::Version),
			(&["doctor", "--format=json"], Command::Doctor),
			(&["catalog", "get", "--format", "json"], Command::CatalogGet),
			(&["catalog", "list"], Command::CatalogList),
			(&["catalog", "refresh", "--format", "json"], Command::CatalogRefresh),
			(
				&["catalog", "export", "yaml"],
				Command::CatalogExport { format: ExportFormat::Yaml },
			),
			(
				&["catalog", "export", "markdown"],
				Command::CatalogExport { format: ExportFormat::Markdown },
			),
			(&["catalog", "diff", "--format", "json"], Command::CatalogDiff),
			(
				&["model", "inspect", "@cf/deepseek-ai/deepseek-v4-flash-0731", "--format", "json"],
				Command::ModelInspect { model_id: "@cf/deepseek-ai/deepseek-v4-flash-0731".to_string() },
			),
			(&["policy", "get", "--format", "json"], Command::PolicyGet),
		];
		for (args, expected) in cases {
			let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
			let (command, format) = parse_args(&owned).unwrap_or_else(|e| panic!("{args:?} should parse: {e}"));
			assert_eq!(&command, expected, "args {args:?}");
			let _ = format;
		}
	}

	#[test]
	fn unknown_command_is_a_parse_error() {
		let owned = vec!["frobnicate".to_string()];
		assert!(parse_args(&owned).is_err(), "unknown command must fail parsing");
		let owned = vec!["catalog".to_string(), "fly".to_string()];
		assert!(parse_args(&owned).is_err());
		let owned = vec!["model".to_string(), "inspect".to_string()];
		assert!(parse_args(&owned).is_err(), "inspect without id must fail");
	}

	#[test]
	fn unknown_command_exits_1() {
		with_env(&[], || {
			assert_eq!(run_raw(&["frobnicate"]).0, EXIT_OPERATIONAL);
		});
	}

	#[test]
	fn bad_format_flag_exits_1() {
		with_env(&[], || {
			assert_eq!(run_raw(&["version", "--format", "xml"]).0, EXIT_OPERATIONAL);
		});
	}

	#[test]
	fn bad_export_format_exits_1() {
		with_env(&[], || {
			assert_eq!(run_raw(&["catalog", "export", "toml"]).0, EXIT_OPERATIONAL);
		});
	}

	#[test]
	fn help_exits_0() {
		with_env(&[], || {
			assert_eq!(run_raw(&["help"]).0, EXIT_OK);
			assert_eq!(run_raw(&["--help"]).0, EXIT_OK);
		});
	}

	// ------------------------------------------------------------------
	// version (feedback 06 exact shape)
	// ------------------------------------------------------------------

	#[test]
	fn version_json_exact_feedback_06_shape() {
		with_env(&[], || {
			let (code, value) = run_json(&["version", "--format", "json"]);
			assert_eq!(code, EXIT_OK);
			let object = value.as_object().expect("object");
			let keys: std::collections::BTreeSet<&str> = object.keys().map(String::as_str).collect();
			let expected: std::collections::BTreeSet<&str> = [
				"name",
				"package_version",
				"protocol_version",
				"catalog_schema_versions",
				"minimum_hermes_plugin_version",
			]
			.into_iter()
			.collect();
			assert_eq!(keys, expected);
			assert_eq!(value["name"], "auth-cloudflare");
			assert_eq!(value["package_version"], VERSION);
			assert_eq!(value["protocol_version"], 1);
			assert_eq!(value["catalog_schema_versions"], serde_json::json!([1]));
			assert_eq!(value["minimum_hermes_plugin_version"], "0.0.1");
		});
	}

	#[test]
	fn version_defaults_to_json() {
		with_env(&[], || {
			let (code, value) = run_json(&["version"]);
			assert_eq!(code, EXIT_OK);
			assert_eq!(value["name"], "auth-cloudflare");
		});
	}

	// ------------------------------------------------------------------
	// doctor (feedback 06 shape, redaction, exit codes)
	// ------------------------------------------------------------------

	#[test]
	fn doctor_redacts_token_and_account() {
		let home = scratch_dir("doctor-redact");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				let (code, value) = run_json(&["doctor", "--format", "json"]);
				assert_eq!(code, EXIT_OK);
				assert_eq!(value["status"], "ok");
				assert_eq!(value["account_id"]["configured"], true);
				assert_eq!(value["account_id"]["redacted"], "012345…cdef");
				assert_eq!(value["api_token"]["configured"], true);
				assert_eq!(value["api_token"]["value_redacted"], true);
				assert_eq!(
					value["endpoint"]["base_url"],
					"https://api.cloudflare.com/client/v4/accounts/<redacted>/ai/v1"
				);
				assert_eq!(value["catalog_cache"]["present"], false);
				assert_eq!(value["catalog_cache"]["age_seconds"], 0);
				// The token value must never appear in ANY doctor output.
				let (_, raw) = run_raw(&["doctor", "--format", "json"]);
				assert!(!raw.contains(TOKEN), "doctor output must never contain the token: {raw}");
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn doctor_missing_creds_exits_2() {
		with_env(&[], || {
			let (code, value) = run_json(&["doctor", "--format", "json"]);
			assert_eq!(code, EXIT_CREDENTIALS);
			assert_eq!(value["status"], "error");
			assert_eq!(value["account_id"]["configured"], false);
			assert_eq!(value["api_token"]["configured"], false);
			assert_eq!(value["api_token"]["value_redacted"], true);
			assert!(value["endpoint"]["base_url"].is_null(), "base_url omitted when account unset");
		});
	}

	#[test]
	fn doctor_token_only_missing_reports_account_configured() {
		with_env(&[(ACCOUNT_ENV, Some(ACCOUNT))], || {
			let (code, value) = run_json(&["doctor", "--format", "json"]);
			assert_eq!(code, EXIT_CREDENTIALS);
			assert_eq!(value["account_id"]["configured"], true);
			assert_eq!(value["api_token"]["configured"], false);
		});
	}

	#[test]
	fn doctor_detects_unsafe_config_exit_7() {
		let home = scratch_dir("doctor-unsafe");
		let _ = std::fs::remove_dir_all(&home);
		std::fs::create_dir_all(&home).expect("create scratch dir");
		let config_path = home.join("config.json");
		std::fs::write(
			&config_path,
			format!(r#"{{"account_id":"{ACCOUNT}","api_token":"cfut_leaked_value"}}"#),
		)
		.expect("write unsafe config");
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("AUTH_CLOUDFLARE_CONFIG", Some(config_path.to_str().unwrap())),
			],
			|| {
				let (code, value) = run_json(&["doctor", "--format", "json"]);
				assert_eq!(code, EXIT_UNSAFE_CONFIG);
				assert_eq!(value["status"], "error");
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn doctor_shows_cache_present_and_age() {
		let home = scratch_dir("doctor-cache");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				seed_cache(&chrono::Utc::now().to_rfc3339(), &[DEFAULT_MODEL]);
				let (code, value) = run_json(&["doctor", "--format", "json"]);
				assert_eq!(code, EXIT_OK);
				assert_eq!(value["catalog_cache"]["present"], true);
				assert!(value["catalog_cache"]["age_seconds"].as_u64().unwrap_or(u64::MAX) < 10);
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn redact_account_id_patterns() {
		assert_eq!(redact_account_id("624acc1234567890abcdef123456789f84"), "624acc…9f84");
		assert_eq!(redact_account_id("abcdef"), "ab…ef");
		assert_eq!(redact_account_id("ab"), "a…");
		assert_eq!(redact_account_id(""), "…");
	}

	// ------------------------------------------------------------------
	// catalog get (cache-first, fallback, exit 4 stale)
	// ------------------------------------------------------------------

	#[test]
	fn catalog_get_cache_miss_falls_back_to_bundled_models() {
		with_env(&[], || {
			let (code, value) = run_json(&["catalog", "get", "--format", "json"]);
			assert_eq!(code, EXIT_OK);
			assert_eq!(value["schema_version"], 1);
			assert_eq!(value["source"], "fallback");
			assert_eq!(value["cache_status"], "none");
			assert_eq!(value["default_model"], DEFAULT_MODEL);
			let models = value["models"].as_array().expect("models array");
			assert!(!models.is_empty());
			assert_eq!(models[0]["id"], DEFAULT_MODEL);
			assert_eq!(models[0]["status"], "recommended");
			assert_eq!(models[0]["primary_agent_eligible"], true);
			assert!(
				models[0]["pricing_per_million"]["input"].is_null(),
				"fallback pricing is honestly null"
			);
		});
	}

	#[test]
	fn catalog_get_fresh_cache_served() {
		let home = scratch_dir("get-fresh");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				seed_cache(&chrono::Utc::now().to_rfc3339(), &[DEFAULT_MODEL]);
				let (code, value) = run_json(&["catalog", "get", "--format", "json"]);
				assert_eq!(code, EXIT_OK);
				assert_eq!(value["source"], "cache");
				assert_eq!(value["cache_status"], "fresh");
				assert_eq!(value["models"][0]["id"], DEFAULT_MODEL);
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn catalog_get_stale_cache_exits_4() {
		let home = scratch_dir("get-stale");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				seed_cache("2016-01-01T00:00:00Z", &[DEFAULT_MODEL]);
				let (code, value) = run_json(&["catalog", "get", "--format", "json"]);
				assert_eq!(code, EXIT_STALE_CACHE);
				assert_eq!(value["source"], "cache");
				assert_eq!(value["cache_status"], "stale");
				assert_eq!(value["models"][0]["id"], DEFAULT_MODEL, "stale cache is still served");
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn catalog_list_is_ordered_ids() {
		with_env(&[], || {
			let (code, value) = run_json(&["catalog", "list", "--format", "json"]);
			assert_eq!(code, EXIT_OK);
			let models = value["models"].as_array().expect("models array");
			assert!(!models.is_empty());
			assert!(models.iter().all(|m| m.is_string()));
			assert_eq!(models[0], DEFAULT_MODEL);
		});
	}

	// ------------------------------------------------------------------
	// catalog refresh (live-first: fetch, cache write, stale/typed fallback)
	// ------------------------------------------------------------------

	#[test]
	fn refresh_missing_creds_exits_2() {
		with_env(&[], || {
			let (code, value) = run_json_with_fetch(&["catalog", "refresh", "--format", "json"], fetch_ok);
			assert_eq!(code, EXIT_CREDENTIALS);
			assert_eq!(value["status"], "error");
			assert_eq!(value["exit_code"], EXIT_CREDENTIALS);
			assert!(
				value["error"]
					.as_str()
					.expect("error string")
					.contains("AUTH_CLOUDFLARE_ACCOUNT_ID"),
				"missing-cred error names the exact env var"
			);
		});
	}

	#[test]
	fn refresh_live_success_exits_0_and_writes_cache() {
		let home = scratch_dir("refresh-live-ok");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				let (code, value) = run_json_with_fetch(&["catalog", "refresh", "--format", "json"], fetch_ok);
				assert_eq!(code, EXIT_OK);
				assert_eq!(value["schema_version"], 1);
				assert_eq!(value["source"], "cloudflare-workers-ai");
				assert_eq!(value["cache_status"], "fresh");
				assert_eq!(value["default_model"], DEFAULT_MODEL);
				let models = value["models"].as_array().expect("models array");
				assert_eq!(models.len(), 2);
				assert_eq!(models[0]["id"], DEFAULT_MODEL);
				assert_eq!(models[0]["status"], "recommended");
				assert_eq!(models[0]["pricing_per_million"]["input"], 0.44);
				assert_eq!(models[0]["capabilities"]["tools"], "confirmed");
				// The account cache was written with live provenance.
				let dir = cache_dir_for_account(&AuthProvider::new(ACCOUNT));
				let (meta, payload) = read_catalog_cache(&dir).expect("read cache").expect("cache written");
				assert_eq!(meta.source, "cloudflare-workers-ai");
				assert_eq!(meta.model_count, 2);
				assert_eq!(meta.account_fingerprint, AuthProvider::new(ACCOUNT).cache_slug());
				assert_eq!(payload["data"][0]["id"], DEFAULT_MODEL);
				// The token never appears in refresh stdout.
				let (_, raw) = run_raw_with_fetch(&["catalog", "refresh", "--format", "json"], fetch_ok);
				assert!(!raw.contains(TOKEN), "refresh output must never contain the token: {raw}");
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn refresh_remote_failure_no_cache_exits_3() {
		let home = scratch_dir("refresh-fail-nocache");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				let (code, value) = run_json_with_fetch(&["catalog", "refresh", "--format", "json"], fetch_err);
				assert_eq!(code, EXIT_REMOTE_API);
				assert_eq!(value["status"], "error");
				assert!(
					value["error"]
						.as_str()
						.expect("error string")
						.contains("simulated network failure")
				);
				assert_eq!(value["exit_code"], EXIT_REMOTE_API);
				let (_, raw) = run_raw_with_fetch(&["catalog", "refresh", "--format", "json"], fetch_err);
				assert!(!raw.contains(TOKEN), "error output must never contain the token: {raw}");
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn refresh_remote_failure_stale_cache_exits_4() {
		let home = scratch_dir("refresh-fail-stale");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				seed_cache("2016-01-01T00:00:00Z", &[DEFAULT_MODEL]);
				let (code, value) = run_json_with_fetch(&["catalog", "refresh", "--format", "json"], fetch_err);
				assert_eq!(code, EXIT_STALE_CACHE);
				assert_eq!(value["source"], "cloudflare-workers-ai");
				assert_eq!(value["cache_status"], "stale");
				assert_eq!(value["models"][0]["id"], DEFAULT_MODEL, "stale cache is still served");
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn refresh_remote_failure_fresh_cache_still_exits_0() {
		let home = scratch_dir("refresh-fail-fresh");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				seed_cache(&chrono::Utc::now().to_rfc3339(), &[DEFAULT_MODEL]);
				let (code, value) = run_json_with_fetch(&["catalog", "refresh", "--format", "json"], fetch_err);
				assert_eq!(code, EXIT_OK, "a still-fresh cache is served, not discarded");
				assert_eq!(value["cache_status"], "fresh");
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	// ------------------------------------------------------------------
	// catalog get live-fetch path (cache absent -> live -> fallback)
	// ------------------------------------------------------------------

	#[test]
	fn catalog_get_live_fetch_on_cache_miss() {
		let home = scratch_dir("get-live");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				let (code, value) = run_json_with_fetch(&["catalog", "get", "--format", "json"], fetch_ok);
				assert_eq!(code, EXIT_OK);
				assert_eq!(value["source"], "live");
				assert_eq!(value["cache_status"], "fresh");
				assert_eq!(value["models"][0]["id"], DEFAULT_MODEL);
				// The live result was cached for next time.
				let dir = cache_dir_for_account(&AuthProvider::new(ACCOUNT));
				assert!(
					read_catalog_cache(&dir).expect("read cache").is_some(),
					"live get seeds the cache"
				);
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn catalog_get_live_failure_falls_back_to_bundled() {
		let home = scratch_dir("get-live-fail");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				let (code, value) = run_json_with_fetch(&["catalog", "get", "--format", "json"], fetch_err);
				assert_eq!(code, EXIT_OK, "live failure without cache falls back with exit 0");
				assert_eq!(value["source"], "fallback");
				assert_eq!(value["cache_status"], "none");
				assert_eq!(value["models"][0]["id"], DEFAULT_MODEL);
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn catalog_list_live_fetch_on_cache_miss() {
		let home = scratch_dir("list-live");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				let (code, value) = run_json_with_fetch(&["catalog", "list", "--format", "json"], fetch_ok);
				assert_eq!(code, EXIT_OK);
				assert_eq!(value["source"], "live");
				let models = value["models"].as_array().expect("models array");
				assert_eq!(models[0], DEFAULT_MODEL);
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn diff_without_cache_exits_1() {
		with_env(&[], || {
			let (code, value) = run_json(&["catalog", "diff", "--format", "json"]);
			assert_eq!(code, EXIT_OPERATIONAL);
			assert_eq!(value["status"], "error");
		});
	}

	#[test]
	fn diff_against_seeded_cache_reports_added_and_common() {
		let home = scratch_dir("diff");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				seed_cache(&chrono::Utc::now().to_rfc3339(), &[DEFAULT_MODEL]);
				let (code, value) = run_json(&["catalog", "diff", "--format", "json"]);
				assert_eq!(code, EXIT_OK);
				assert_eq!(value["common_count"], 1);
				assert_eq!(value["added"].as_array().expect("added").len(), FALLBACK_MODELS.len() - 1);
				assert!(value["removed"].as_array().expect("removed").is_empty());
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	#[test]
	fn export_yaml_and_markdown_derive_from_catalog() {
		let home = scratch_dir("export");
		let _ = std::fs::remove_dir_all(&home);
		std::fs::create_dir_all(&home).expect("create scratch dir");
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
				(EXPORT_DIR_ENV, Some(home.to_str().unwrap())),
			],
			|| {
				// Seed a fresh cache so the export stays hermetic (no live fetch).
				seed_cache(&chrono::Utc::now().to_rfc3339(), &[DEFAULT_MODEL]);
				let (code, message) = run_raw(&["catalog", "export", "yaml"]);
				assert_eq!(code, EXIT_OK, "yaml export: {message}");
				let yaml = std::fs::read_to_string(home.join("catalog.generated.yaml")).expect("yaml file");
				assert!(yaml.contains("GENERATED FILE"));
				assert!(yaml.contains(DEFAULT_MODEL));

				let (code, message) = run_raw(&["catalog", "export", "markdown"]);
				assert_eq!(code, EXIT_OK, "markdown export: {message}");
				let md = std::fs::read_to_string(home.join("catalog.generated.md")).expect("md file");
				assert!(md.contains("Cloudflare Workers AI catalog"));
				assert!(md.contains("| Model | Status |"));
				// Markdown rows show display names; ids appear in YAML.
				assert!(md.contains("DeepSeek V4 Flash 0731"));
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	// ------------------------------------------------------------------
	// model inspect
	// ------------------------------------------------------------------

	#[test]
	fn model_inspect_found_offline() {
		with_env(&[], || {
			let (code, value) = run_json(&["model", "inspect", DEFAULT_MODEL, "--format", "json"]);
			assert_eq!(code, EXIT_OK);
			assert_eq!(value["id"], DEFAULT_MODEL);
			assert_eq!(value["source"], "fallback");
			assert_eq!(value["status"], "recommended");
		});
	}

	#[test]
	fn model_inspect_missing_exits_5() {
		with_env(&[], || {
			let (code, value) = run_json(&["model", "inspect", "@cf/unknown/not-in-catalog", "--format", "json"]);
			assert_eq!(code, EXIT_NO_ELIGIBLE_MODEL);
			assert_eq!(value["status"], "error");
		});
	}

	#[test]
	fn model_inspect_serves_experimental_from_cache() {
		let home = scratch_dir("inspect-cache");
		let _ = std::fs::remove_dir_all(&home);
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(TOKEN_ENV, Some(TOKEN)),
				("HERMES_HOME", Some(home.to_str().unwrap())),
			],
			|| {
				seed_cache(&chrono::Utc::now().to_rfc3339(), &["@cf/zai-org/glm-5.3-flash"]);
				let (code, value) = run_json(&["model", "inspect", "@cf/zai-org/glm-5.3-flash", "--format", "json"]);
				assert_eq!(code, EXIT_OK);
				assert_eq!(value["source"], "cache");
				assert_eq!(value["status"], "experimental", "policy marks GLM-5.3 Flash experimental");
			},
		);
		let _ = std::fs::remove_dir_all(&home);
	}

	// ------------------------------------------------------------------
	// policy get
	// ------------------------------------------------------------------

	#[test]
	fn policy_get_matches_core_policy() {
		with_env(&[], || {
			let (code, value) = run_json(&["policy", "get", "--format", "json"]);
			assert_eq!(code, EXIT_OK);
			assert_eq!(value["version"], "1");
			let models = value["models"].as_array().expect("models array");
			assert_eq!(models[0]["model_id"], DEFAULT_MODEL);
			assert_eq!(models[0]["status"], "recommended");
			assert_eq!(models[0]["default"], true);
			let guard = models
				.iter()
				.find(|m| m["model_id"] == "@cf/meta/llama-guard-3-8b")
				.expect("guard entry");
			assert_eq!(guard["status"], "hidden");
			assert_eq!(guard["primary_agent_eligible"], false);
		});
	}

	// ------------------------------------------------------------------
	// helpers
	// ------------------------------------------------------------------

	#[test]
	fn display_name_from_id_heuristics() {
		assert_eq!(
			display_name_from_id("@cf/deepseek-ai/deepseek-v4-flash-0731"),
			"DeepSeek V4 Flash 0731"
		);
		assert_eq!(display_name_from_id("@cf/zai-org/glm-5.3-flash"), "GLM 5.3 Flash");
		assert_eq!(display_name_from_id("plain-id"), "Plain Id");
	}

	#[test]
	fn unsafe_config_detection_ignores_missing_or_safe_files() {
		let home = scratch_dir("unsafe-detect");
		let _ = std::fs::remove_dir_all(&home);
		std::fs::create_dir_all(&home).expect("create scratch dir");
		assert!(!detect_unsafe_config(&home.join("missing.json")), "missing file is safe");
		let safe = home.join("safe.json");
		std::fs::write(&safe, r#"{"account_id":"0123456789abcdef0123456789abcdef"}"#).expect("write safe config");
		assert!(!detect_unsafe_config(&safe), "no api_token value is safe");
		let env_named = home.join("env-named.json");
		std::fs::write(&env_named, r#"{"api_token_env":"MY_CF_TOKEN_VAR"}"#).expect("write env-named config");
		assert!(!detect_unsafe_config(&env_named), "env-var NAME is allowed (feedback 02)");
		let leaked = home.join("leaked.json");
		std::fs::write(&leaked, r#"{"api_token":"cfut_leaked"}"#).expect("write leaked config");
		assert!(detect_unsafe_config(&leaked), "a literal token value is unsafe");
		let _ = std::fs::remove_dir_all(&home);
	}
}
