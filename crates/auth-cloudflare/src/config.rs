//! Config - typed account/token/base-url/cache resolution with strict
//! precedence and secret-safe token handling.
//!
//! Precedence chain (binding feedback 03/06):
//!
//! ```text
//! 1. explicit constructor/config value (ConfigBuilder)
//! 2. canonical AUTH_CLOUDFLARE_* environment variables
//! 3. legacy Hermes-compatible aliases
//!    (CLOUDFLARE_ACCOUNT_ID, CLOUDFLARE_API_TOKEN,
//!     HERMES_CUSTOM_API_CLOUDFLARE_COM_API_KEY)
//! 4. user config file (JSON - non-secret values only)
//! 5. typed missing-config error (CloudflareError::MissingEnv)
//! ```
//!
//! The API token is held in [`SecretString`]: it never appears in `Debug`,
//! `Display`, JSON serialization, or any error message. Callers consume it
//! through `as_ref()`/`into()` (or [`SecretString::bearer_header`]) when
//! building the `Authorization: Bearer <token>` header.

use std::path::PathBuf;

use serde::Serialize;

use crate::auth::{AuthProvider, ACCOUNT_ENV, TOKEN_ENV};
use crate::cache::HERMES_HOME_ENV;
use crate::error::CloudflareError;

/// Canonical env var for the Cloudflare account ID (non-secret).
pub const ACCOUNT_ID_ENV: &str = "AUTH_CLOUDFLARE_ACCOUNT_ID";
/// Canonical env var for the Cloudflare API token (secret).
pub const API_TOKEN_ENV: &str = "AUTH_CLOUDFLARE_API_TOKEN";
/// Optional override for the Workers AI inference base URL.
pub const BASE_URL_ENV: &str = "AUTH_CLOUDFLARE_WORKERS_AI_BASE_URL";
/// Optional override for the account-scoped cache directory.
pub const CACHE_DIR_ENV: &str = "AUTH_CLOUDFLARE_CACHE_DIR";
/// Optional override for the user config file path.
pub const CONFIG_ENV: &str = "AUTH_CLOUDFLARE_CONFIG";
/// Legacy Hermes-compatible token alias (feedback 03).
pub const LEGACY_HERMES_TOKEN_ENV: &str = "HERMES_CUSTOM_API_CLOUDFLARE_COM_API_KEY";

/// Expected Cloudflare account ID shape: exactly this many ASCII hex digits.
/// (Workers & Pages → Overview → Account ID.)
pub const ACCOUNT_ID_LEN: usize = 32;

/// Default user config file name under `$HERMES_HOME/auth-cloudflare/`.
pub const CONFIG_FILE_NAME: &str = "config.json";

/// A wrapped API token that can never leak through formatting or JSON.
///
/// The raw value is only reachable through [`AsRef<str>`] and
/// [`From<SecretString> for String`] - i.e. the Bearer-header construction
/// path. `Debug`, `Display`, and `Serialize` all emit a redaction marker.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
	/// Wrap a token value. The caller is responsible for the value being a
	/// real credential; this type only guarantees it never leaks.
	pub fn new(value: impl Into<String>) -> Self {
		Self(value.into())
	}

	/// The `Authorization: Bearer <token>` header value.
	pub fn bearer_header(&self) -> String {
		format!("Bearer {}", self.0)
	}
}

impl std::fmt::Debug for SecretString {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("SecretString(\"<redacted>\")")
	}
}

impl std::fmt::Display for SecretString {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("<redacted>")
	}
}

impl AsRef<str> for SecretString {
	fn as_ref(&self) -> &str {
		&self.0
	}
}

impl From<SecretString> for String {
	fn from(secret: SecretString) -> Self {
		secret.0
	}
}

impl From<String> for SecretString {
	fn from(value: String) -> Self {
		Self(value)
	}
}

impl From<&str> for SecretString {
	fn from(value: &str) -> Self {
		Self(value.to_string())
	}
}

impl Serialize for SecretString {
	/// Serializes as the literal `"<redacted>"` - the raw token can never
	/// reach JSON output, even through a derived `Serialize` impl.
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str("<redacted>")
	}
}

/// Fully resolved provider configuration.
///
/// Values are resolved once at construction through the documented
/// precedence chain; the account ID is validated for shape and the token is
/// held in a [`SecretString`].
#[derive(Clone, Debug)]
pub struct Config {
	account_id: String,
	api_token: SecretString,
	base_url: Option<String>,
	cache_dir: Option<PathBuf>,
	config_path: PathBuf,
}

impl Config {
	/// Resolve configuration strictly: constructor/env/config file, or a
	/// typed [`CloudflareError::MissingEnv`] error naming the exact
	/// environment variable that is missing.
	pub fn from_env() -> Result<Self, CloudflareError> {
		ConfigBuilder::new().build()
	}

	/// Resolve configuration leniently (profile bootstrap): `None` whenever
	/// the account ID or token cannot be resolved and validated.
	pub fn from_env_lenient() -> Option<Self> {
		Self::from_env().ok()
	}

	/// The validated account ID (non-secret operational metadata).
	pub fn account_id(&self) -> &str {
		&self.account_id
	}

	/// The API token - redacted everywhere except `as_ref()`/`into()`.
	pub fn api_token(&self) -> &SecretString {
		&self.api_token
	}

	/// The Workers AI inference base URL: the explicit override when set,
	/// otherwise derived as
	/// `https://api.cloudflare.com/client/v4/accounts/<id>/ai/v1`.
	pub fn base_url(&self) -> Result<String, CloudflareError> {
		Ok(match &self.base_url {
			Some(url) => url.clone(),
			None => AuthProvider::new(&self.account_id).base_url(),
		})
	}

	/// The account-scoped cache directory.
	///
	/// Explicit override wins; the default is
	/// `$HERMES_HOME/cache/auth-cloudflare/<slug>/` where `<slug>` is the
	/// stable 16-hex account hash and `HERMES_HOME` falls back to `~/.hermes`.
	/// The token never enters the cache path.
	pub fn cache_dir(&self) -> PathBuf {
		match &self.cache_dir {
			Some(dir) => dir.clone(),
			None => hermes_home()
				.join("cache")
				.join("auth-cloudflare")
				.join(AuthProvider::new(&self.account_id).cache_slug()),
		}
	}

	/// The user config file path: `AUTH_CLOUDFLARE_CONFIG` override or
	/// `$HERMES_HOME/auth-cloudflare/config.json`.
	pub fn config_path(&self) -> PathBuf {
		self.config_path.clone()
	}
}

/// Builder for [`Config`] - the "explicit constructor/config value" tier of
/// the precedence chain.
///
/// Any field set here is pinned above every environment variable and the
/// config file; unset fields fall through the canonical env vars, the
/// legacy aliases, and the user config file.
#[derive(Clone, Debug, Default)]
pub struct ConfigBuilder {
	account_id: Option<String>,
	api_token: Option<String>,
	base_url: Option<String>,
	cache_dir: Option<PathBuf>,
	config_path: Option<PathBuf>,
}

impl ConfigBuilder {
	pub fn new() -> Self {
		Self::default()
	}

	/// Pin the account ID (non-secret).
	pub fn account_id(mut self, value: impl Into<String>) -> Self {
		self.account_id = Some(value.into());
		self
	}

	/// Pin the API token (secret - held as [`SecretString`] on resolution).
	pub fn api_token(mut self, value: impl Into<String>) -> Self {
		self.api_token = Some(value.into());
		self
	}

	/// Pin the Workers AI base URL override.
	pub fn base_url(mut self, value: impl Into<String>) -> Self {
		self.base_url = Some(value.into());
		self
	}

	/// Pin the cache directory override.
	pub fn cache_dir(mut self, value: impl Into<PathBuf>) -> Self {
		self.cache_dir = Some(value.into());
		self
	}

	/// Pin the user config file path.
	pub fn config_path(mut self, value: impl Into<PathBuf>) -> Self {
		self.config_path = Some(value.into());
		self
	}

	/// Resolve through the full precedence chain.
	pub fn build(self) -> Result<Config, CloudflareError> {
		let config_path = self
			.config_path
			.or_else(|| env_nonempty(CONFIG_ENV).map(PathBuf::from))
			.unwrap_or_else(|| hermes_home().join("auth-cloudflare").join(CONFIG_FILE_NAME));
		let file = FileConfig::load(&config_path)?;

		// 1. Account ID: constructor > canonical env > legacy alias > file.
		let account_id = normalize(self.account_id)
			.or_else(|| env_nonempty(ACCOUNT_ID_ENV))
			.or_else(|| env_nonempty(ACCOUNT_ENV))
			.or_else(|| normalize(file.account_id))
			.ok_or_else(|| CloudflareError::MissingEnv {
				env_var: ACCOUNT_ID_ENV,
				hint: format!(
					"export {ACCOUNT_ID_ENV}=<account id> (aliases: {ACCOUNT_ENV}) - found under Workers & Pages → Overview → Account ID"
				),
			})?;
		if !is_valid_account_id(&account_id) {
			return Err(CloudflareError::MissingEnv {
				env_var: ACCOUNT_ID_ENV,
				hint: format!(
					"{ACCOUNT_ID_ENV} must be exactly {ACCOUNT_ID_LEN} ASCII hex digits, got {} (\"{account_id}\")",
					account_id.len()
				),
			});
		}

		// 2. API token: constructor > canonical env > legacy aliases > file
		//    (which may only name the env var holding the token - never the
		//    value itself, per feedback 02).
		let api_token = normalize(self.api_token)
			.or_else(|| env_nonempty(API_TOKEN_ENV))
			.or_else(|| env_nonempty(TOKEN_ENV))
			.or_else(|| env_nonempty(LEGACY_HERMES_TOKEN_ENV))
			.or_else(|| {
				let name = file.api_token_env.as_deref().map(str::trim).filter(|n| !n.is_empty())?;
				env_nonempty(name)
			})
			.ok_or_else(|| CloudflareError::MissingEnv {
				env_var: API_TOKEN_ENV,
				hint: format!(
					"export {API_TOKEN_ENV}=<scoped api token> (aliases: {TOKEN_ENV}, {LEGACY_HERMES_TOKEN_ENV}) - create a token with Account → Workers AI → Write/Edit"
				),
			})?;

		// 3. Base URL override: constructor > canonical env > file.
		let base_url = normalize(self.base_url)
			.or_else(|| env_nonempty(BASE_URL_ENV))
			.or_else(|| normalize(file.base_url));

		// 4. Cache dir override: constructor > canonical env > file.
		let cache_dir = self
			.cache_dir
			.or_else(|| env_nonempty(CACHE_DIR_ENV).map(PathBuf::from))
			.or_else(|| file.cache_dir.map(PathBuf::from));

		Ok(Config {
			account_id,
			api_token: SecretString::new(api_token),
			base_url,
			cache_dir,
			config_path,
		})
	}
}

/// Non-secret user config file (JSON).
///
/// The token VALUE is never stored here; `api_token_env` may name the
/// environment variable that holds it ("the configuration file may contain
/// the variable name but never the secret value" - binding feedback 02). A
/// stray `api_token` value in the file is ignored by serde and never read.
#[derive(serde::Deserialize, Default)]
struct FileConfig {
	account_id: Option<String>,
	base_url: Option<String>,
	cache_dir: Option<String>,
	api_token_env: Option<String>,
}

impl FileConfig {
	/// Load the config file; a missing file is an empty config, an
	/// unreadable or malformed file is a typed error naming the file.
	fn load(path: &std::path::Path) -> Result<Self, CloudflareError> {
		let contents = match std::fs::read_to_string(path) {
			Ok(contents) => contents,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
			Err(error) => {
				return Err(CloudflareError::MissingEnv {
					env_var: CONFIG_ENV,
					hint: format!("config file {} is unreadable: {error}", path.display()),
				});
			},
		};
		serde_json::from_str(&contents).map_err(|error| CloudflareError::MissingEnv {
			env_var: CONFIG_ENV,
			hint: format!("config file {} is not valid JSON: {error}", path.display()),
		})
	}
}

/// Trim an explicit value; empty/whitespace-only values count as missing
/// (the documented "whitespace-only treated as missing" rule).
fn normalize(value: Option<String>) -> Option<String> {
	value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Read a non-empty (after trim) environment variable, if present.
fn env_nonempty(name: &str) -> Option<String> {
	std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Cloudflare account IDs are 32 ASCII hex digits (lowercase in the
/// dashboard; uppercase hex is accepted).
fn is_valid_account_id(value: &str) -> bool {
	value.len() == ACCOUNT_ID_LEN && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Resolve `$HERMES_HOME`, falling back to `~/.hermes` - matches `hermes`
/// itself and `crate::cache::cache_dir_for_account`.
fn hermes_home() -> PathBuf {
	env_nonempty(HERMES_HOME_ENV).map(PathBuf::from).unwrap_or_else(|| {
		std::env::var("HOME")
			.ok()
			.map(PathBuf::from)
			.unwrap_or_else(|| PathBuf::from("~"))
			.join(".hermes")
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Cloudflare-shaped synthetic account ID (32 hex digits) - never a
	/// real account.
	const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
	/// Second synthetic account ID for override comparisons.
	const OTHER_ACCOUNT: &str = "fedcba9876543210fedcba9876543210";
	/// Synthetic token - never a real credential.
	const TOKEN: &str = "cfut_test_synthetic_token_0001";

	/// Every env var this module reads, saved/restored for isolation.
	const ALL_VARS: &[&str] = &[
		ACCOUNT_ID_ENV,
		API_TOKEN_ENV,
		BASE_URL_ENV,
		CACHE_DIR_ENV,
		CONFIG_ENV,
		ACCOUNT_ENV,
		TOKEN_ENV,
		LEGACY_HERMES_TOKEN_ENV,
		HERMES_HOME_ENV,
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

	/// Write a temp config file outside the repo; returns its path.
	fn temp_config_file(name: &str, contents: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(format!("auth-cloudflare-config-tests-{}", std::process::id()));
		std::fs::create_dir_all(&dir).expect("create temp dir");
		let path = dir.join(name);
		std::fs::write(&path, contents).expect("write config file");
		path
	}

	#[test]
	fn canonical_env_wins_over_legacy_alias() {
		with_env(
			&[
				(ACCOUNT_ID_ENV, Some(ACCOUNT)),
				(ACCOUNT_ENV, Some(OTHER_ACCOUNT)),
				(API_TOKEN_ENV, Some("cfut_test_canonical_token")),
				(TOKEN_ENV, Some("cfut_test_legacy_token")),
			],
			|| {
				let config = Config::from_env().expect("canonical vars present");
				assert_eq!(config.account_id(), ACCOUNT);
				assert_eq!(config.api_token().as_ref(), "cfut_test_canonical_token");
				assert_eq!(
					config.base_url().unwrap(),
					format!("https://api.cloudflare.com/client/v4/accounts/{ACCOUNT}/ai/v1")
				);
			},
		);
	}

	#[test]
	fn legacy_alias_fallback() {
		with_env(&[(ACCOUNT_ENV, Some(ACCOUNT)), (TOKEN_ENV, Some(TOKEN))], || {
			let config = Config::from_env().expect("legacy vars present");
			assert_eq!(config.account_id(), ACCOUNT);
			assert_eq!(config.api_token().as_ref(), TOKEN);
		});
		// The Hermes custom-key alias resolves the token too.
		with_env(
			&[
				(ACCOUNT_ENV, Some(ACCOUNT)),
				(LEGACY_HERMES_TOKEN_ENV, Some("cfut_test_hermes_key")),
			],
			|| {
				let config = Config::from_env().expect("hermes alias present");
				assert_eq!(config.account_id(), ACCOUNT);
				assert_eq!(config.api_token().as_ref(), "cfut_test_hermes_key");
			},
		);
	}

	#[test]
	fn whitespace_only_treated_missing() {
		with_env(&[(ACCOUNT_ID_ENV, Some(" \t ")), (API_TOKEN_ENV, Some(TOKEN))], || {
			let error = Config::from_env().expect_err("whitespace account id is missing");
			assert!(matches!(error, CloudflareError::MissingEnv { env_var: ACCOUNT_ID_ENV, .. }));
		});
		with_env(&[(ACCOUNT_ID_ENV, Some(ACCOUNT)), (API_TOKEN_ENV, Some("  "))], || {
			let error = Config::from_env().expect_err("whitespace token is missing");
			assert!(matches!(error, CloudflareError::MissingEnv { env_var: API_TOKEN_ENV, .. }));
		});
	}

	#[test]
	fn token_redacted_in_debug_display_and_json() {
		let token = SecretString::new(TOKEN);
		assert!(!format!("{token:?}").contains(TOKEN), "Debug must redact the token");
		assert!(!format!("{token}").contains(TOKEN), "Display must redact the token");
		let json = serde_json::to_string(&token).expect("serialize");
		assert_eq!(json, "\"<redacted>\"");
		assert!(!json.contains(TOKEN), "JSON must redact the token");
		// The whole Config must not leak it either.
		with_env(&[(ACCOUNT_ID_ENV, Some(ACCOUNT)), (API_TOKEN_ENV, Some(TOKEN))], || {
			let config = Config::from_env().expect("configured");
			assert!(!format!("{config:?}").contains(TOKEN), "Config Debug must redact the token");
		});
	}

	#[test]
	fn bearer_header_consumes_token_via_as_ref_and_into() {
		let token = SecretString::new(TOKEN);
		assert_eq!(token.as_ref(), TOKEN);
		assert_eq!(token.bearer_header(), format!("Bearer {TOKEN}"));
		let into_string: String = token.clone().into();
		assert_eq!(into_string, TOKEN);
		let from_str: SecretString = TOKEN.into();
		assert_eq!(from_str.as_ref(), TOKEN);
	}

	#[test]
	fn base_url_derived_exactly() {
		with_env(&[(ACCOUNT_ID_ENV, Some(ACCOUNT)), (API_TOKEN_ENV, Some(TOKEN))], || {
			let config = Config::from_env().expect("configured");
			assert_eq!(
				config.base_url().unwrap(),
				"https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/ai/v1"
			);
		});
	}

	#[test]
	fn base_url_override_wins() {
		with_env(
			&[
				(ACCOUNT_ID_ENV, Some(ACCOUNT)),
				(API_TOKEN_ENV, Some(TOKEN)),
				(BASE_URL_ENV, Some("https://example.test/ai/v1")),
			],
			|| {
				let config = Config::from_env().expect("configured");
				assert_eq!(config.base_url().unwrap(), "https://example.test/ai/v1");
			},
		);
	}

	#[test]
	fn invalid_account_id_shape_rejected() {
		// Too short, 30 hex digits, internal whitespace, non-hex characters.
		for bad in [
			"abc123",
			"0123456789abcdef0123456789abcd",
			"0123456789abcdef 0123456789abcdef",
			"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
		] {
			with_env(&[(ACCOUNT_ID_ENV, Some(bad)), (API_TOKEN_ENV, Some(TOKEN))], || {
				let error = Config::from_env().expect_err("invalid shape must be rejected");
				assert!(matches!(error, CloudflareError::MissingEnv { env_var: ACCOUNT_ID_ENV, .. }));
				assert!(Config::from_env_lenient().is_none(), "lenient must drop invalid shape");
			});
		}
	}

	#[test]
	fn account_id_accepts_uppercase_hex() {
		with_env(
			&[
				(ACCOUNT_ID_ENV, Some("0123456789ABCDEF0123456789ABCDEF")),
				(API_TOKEN_ENV, Some(TOKEN)),
			],
			|| {
				assert!(Config::from_env().is_ok(), "uppercase hex is a valid account id");
			},
		);
	}

	#[test]
	fn config_file_provides_account_and_token_env_name() {
		let path = temp_config_file(
			"config-env-name.json",
			&format!(r#"{{"account_id":"{ACCOUNT}","api_token_env":"MY_CF_TOKEN_VAR"}}"#),
		);
		with_env(
			&[(CONFIG_ENV, Some(path.to_str().unwrap())), ("MY_CF_TOKEN_VAR", Some(TOKEN))],
			|| {
				let config = Config::from_env().expect("config file fallback");
				assert_eq!(config.account_id(), ACCOUNT);
				assert_eq!(config.api_token().as_ref(), TOKEN);
				assert_eq!(config.config_path(), path);
			},
		);
	}

	#[test]
	fn config_file_token_value_is_never_read() {
		// A secret value placed in the config file must be ignored - the
		// file may only reference the token by env-var name.
		let path = temp_config_file(
			"config-token-ignored.json",
			&format!(r#"{{"account_id":"{ACCOUNT}","api_token":"cfut_test_should_be_ignored"}}"#),
		);
		with_env(&[(CONFIG_ENV, Some(path.to_str().unwrap()))], || {
			let error = Config::from_env().expect_err("token value in file must not satisfy resolution");
			assert!(matches!(error, CloudflareError::MissingEnv { env_var: API_TOKEN_ENV, .. }));
		});
	}

	#[test]
	fn config_file_missing_is_not_an_error() {
		with_env(
			&[
				(ACCOUNT_ID_ENV, Some(ACCOUNT)),
				(API_TOKEN_ENV, Some(TOKEN)),
				(CONFIG_ENV, Some("/nonexistent/auth-cloudflare/config.json")),
			],
			|| {
				let config = Config::from_env().expect("missing config file is fine");
				assert_eq!(config.account_id(), ACCOUNT);
			},
		);
	}

	#[test]
	fn cache_dir_defaults_to_hermes_home_slug() {
		with_env(
			&[
				(ACCOUNT_ID_ENV, Some(ACCOUNT)),
				(API_TOKEN_ENV, Some(TOKEN)),
				(HERMES_HOME_ENV, Some("/tmp/auth-cloudflare-hermes-home")),
			],
			|| {
				let config = Config::from_env().expect("configured");
				let expected = PathBuf::from("/tmp/auth-cloudflare-hermes-home")
					.join("cache")
					.join("auth-cloudflare")
					.join(AuthProvider::new(ACCOUNT).cache_slug());
				assert_eq!(config.cache_dir(), expected);
			},
		);
	}

	#[test]
	fn cache_dir_override_wins() {
		with_env(
			&[
				(ACCOUNT_ID_ENV, Some(ACCOUNT)),
				(API_TOKEN_ENV, Some(TOKEN)),
				(CACHE_DIR_ENV, Some("/tmp/custom-cache")),
			],
			|| {
				let config = Config::from_env().expect("configured");
				assert_eq!(config.cache_dir(), PathBuf::from("/tmp/custom-cache"));
			},
		);
	}

	#[test]
	fn config_path_defaults_under_hermes_home() {
		with_env(
			&[
				(ACCOUNT_ID_ENV, Some(ACCOUNT)),
				(API_TOKEN_ENV, Some(TOKEN)),
				(HERMES_HOME_ENV, Some("/tmp/auth-cloudflare-hermes-home")),
			],
			|| {
				let config = Config::from_env().expect("configured");
				let expected = PathBuf::from("/tmp/auth-cloudflare-hermes-home")
					.join("auth-cloudflare")
					.join("config.json");
				assert_eq!(config.config_path(), expected);
			},
		);
	}

	#[test]
	fn explicit_constructor_wins_over_env() {
		with_env(
			&[
				(ACCOUNT_ID_ENV, Some(OTHER_ACCOUNT)),
				(API_TOKEN_ENV, Some("cfut_test_env_token")),
			],
			|| {
				let config = ConfigBuilder::new()
					.account_id(ACCOUNT)
					.api_token("cfut_test_explicit_token")
					.build()
					.expect("explicit values");
				assert_eq!(config.account_id(), ACCOUNT);
				assert_eq!(config.api_token().as_ref(), "cfut_test_explicit_token");
			},
		);
	}

	#[test]
	fn from_env_lenient_returns_none_when_missing() {
		with_env(&[], || {
			assert!(Config::from_env_lenient().is_none());
		});
		with_env(&[(ACCOUNT_ID_ENV, Some(ACCOUNT)), (API_TOKEN_ENV, Some(TOKEN))], || {
			let config = Config::from_env_lenient().expect("complete env");
			assert_eq!(config.account_id(), ACCOUNT);
		});
	}

	#[test]
	fn missing_env_error_names_the_var() {
		with_env(&[(API_TOKEN_ENV, Some(TOKEN))], || {
			let error = Config::from_env().expect_err("account missing");
			assert!(error.to_string().contains(ACCOUNT_ID_ENV));
		});
		with_env(&[(ACCOUNT_ID_ENV, Some(ACCOUNT))], || {
			let error = Config::from_env().expect_err("token missing");
			assert!(error.to_string().contains(API_TOKEN_ENV));
			assert!(!error.to_string().contains("cfut_"), "error must not echo token prefixes");
		});
	}
}
