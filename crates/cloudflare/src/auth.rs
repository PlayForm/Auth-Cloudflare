//! Auth - Cloudflare account ID + API token → Workers AI endpoint resolution.

use crate::error::CloudflareError;

/// Base API host for Cloudflare client v4 endpoints.
pub const API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Env var holding the Cloudflare API token (`cfut_…` / `cfwt_…`).
pub const TOKEN_ENV: &str = "CLOUDFLARE_API_TOKEN";

/// Env var holding the (non-secret) Cloudflare account ID.
pub const ACCOUNT_ENV: &str = "CLOUDFLARE_ACCOUNT_ID";

/// Resolved Cloudflare account credentials.
///
/// The account ID is operational metadata, not a secret; the token IS a
/// secret and is only ever echoed back through Bearer headers - never into
/// Display/Debug output (see the manual impls below).
#[derive(Clone)]
pub struct AccountCredentials {
	pub account_id: String,
	pub api_token: String,
}

impl std::fmt::Debug for AccountCredentials {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("AccountCredentials")
			.field("account_id", &self.account_id)
			.field("api_token", &"<redacted>")
			.finish()
	}
}

impl std::fmt::Display for AccountCredentials {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "Cloudflare account {} (token redacted)", self.account_id)
	}
}

impl AccountCredentials {
	/// Resolve credentials from the environment.
	///
	/// Raises an actionable error naming the exact env var that is missing.
	pub fn from_env() -> Result<Self, CloudflareError> {
		let account_id = std::env::var(ACCOUNT_ENV).unwrap_or_default().trim().to_string();
		if account_id.is_empty() {
			return Err(CloudflareError::MissingEnv {
				env_var: ACCOUNT_ENV,
				hint: format!(
					"export {ACCOUNT_ENV}=<your account id> - found under Workers & Pages → Overview → Account ID"
				),
			});
		}
		let api_token = std::env::var(TOKEN_ENV).unwrap_or_default().trim().to_string();
		if api_token.is_empty() {
			return Err(CloudflareError::MissingEnv {
				env_var: TOKEN_ENV,
				hint: format!(
					"export {TOKEN_ENV}=<scoped api token> - create a custom token with Account → Workers AI → Edit"
				),
			});
		}
		Ok(Self { account_id, api_token })
	}

	/// Validate credentials without requiring them (profile bootstrap).
	pub fn from_env_lenient() -> Option<Self> {
		let account_id = std::env::var(ACCOUNT_ENV).unwrap_or_default().trim().to_string();
		let api_token = std::env::var(TOKEN_ENV).unwrap_or_default().trim().to_string();
		if account_id.is_empty() || api_token.is_empty() {
			None
		} else {
			Some(Self { account_id, api_token })
		}
	}
}

/// Workers AI auth-provider endpoints for one account.
///
/// Exactly one source of truth for every URL the plugin and the crates build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthProvider {
	pub account_id: String,
}

impl AuthProvider {
	/// Build the provider from an account ID (lenient - used by the Python
	/// profile, which handles the missing-env case itself).
	pub fn new(account_id: impl Into<String>) -> Self {
		Self { account_id: account_id.into().trim().to_string() }
	}

	/// Build the provider from environment credentials, if present.
	pub fn from_env_lenient() -> Option<Self> {
		std::env::var(ACCOUNT_ENV).ok().filter(|v| !v.trim().is_empty()).map(Self::new)
	}

	/// OpenAI-compatible inference base URL - `hermes` appends
	/// `/chat/completions` in Chat Completions mode.
	pub fn base_url(&self) -> String {
		format!("{API_BASE}/accounts/{}/ai/v1", self.account_id)
	}

	/// Model catalog endpoint (OpenRouter-compatible response shape).
	pub fn models_url(&self) -> String {
		format!(
			"{API_BASE}/accounts/{}/ai/models/search?format=openrouter&per_page=1000",
			self.account_id
		)
	}

	/// Token verification endpoint - the plugin's health check.
	pub fn verify_url(&self) -> String {
		format!("{API_BASE}/user/tokens/verify")
	}

	/// Native (REST-style) model invocation path - kept for parity with the
	/// user's existing curl workflow, NOT used by the OpenAI-compatible wire.
	pub fn run_url(&self, model: &str) -> String {
		format!("{API_BASE}/accounts/{}/ai/run/{}", self.account_id, model)
	}

	/// Account-scoped cache directory name (first 12 hex of sha256).
	pub fn cache_slug(&self) -> String {
		use std::fmt::Write;
		// FNV-1a 64-bit - a stable, dependency-free stand-in for sha256[:12].
		let mut hash: u64 = 0xcbf29ce484222325;
		for byte in self.account_id.as_bytes() {
			hash ^= u64::from(*byte);
			hash = hash.wrapping_mul(0x100000001b3);
		}
		let mut slug = String::new();
		let _ = write!(slug, "{hash:016x}");
		slug
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn endpoints_are_stable() {
		// Synthetic account ID - never a real account.
		let provider = AuthProvider::new("00000000000000000000000000000000");
		assert_eq!(
			provider.base_url(),
			"https://api.cloudflare.com/client/v4/accounts/00000000000000000000000000000000/ai/v1"
		);
		assert_eq!(
			provider.models_url(),
			"https://api.cloudflare.com/client/v4/accounts/00000000000000000000000000000000/ai/models/search?format=openrouter&per_page=1000"
		);
		assert_eq!(provider.verify_url(), "https://api.cloudflare.com/client/v4/user/tokens/verify");
		assert_eq!(
			provider.run_url("@cf/zai-org/glm-5.3-flash"),
			"https://api.cloudflare.com/client/v4/accounts/00000000000000000000000000000000/ai/run/@cf/zai-org/glm-5.3-flash"
		);
	}

	#[test]
	fn cache_slug_is_hex_and_stable() {
		let provider = AuthProvider::new("test-account");
		let slug = provider.cache_slug();
		assert_eq!(slug.len(), 16);
		assert!(slug.chars().all(|c| c.is_ascii_hexdigit()));
		assert_eq!(slug, AuthProvider::new("test-account").cache_slug());
		assert_ne!(slug, AuthProvider::new("other-account").cache_slug());
	}

	#[test]
	fn credentials_redact_token() {
		let credentials = AccountCredentials { account_id: "acct".to_string(), api_token: "cfut_SECRET".to_string() };
		let debug = format!("{credentials:?}");
		assert!(!debug.contains("cfut_SECRET"), "Debug must redact the token");
		let display = format!("{credentials}");
		assert!(!display.contains("cfut_SECRET"), "Display must redact the token");
	}
}
