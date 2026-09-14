//! Errors - actionable, token-free Cloudflare provider errors.

use std::fmt;

use thiserror::Error;

/// The scope classification of an authentication/authorization rejection,
/// derived from the Cloudflare error envelope (invalid token vs wrong
/// account scope vs Workers AI permission are distinct, actionable
/// failures, never collapsed into a generic auth error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthScope {
	/// The API token is invalid, expired, or otherwise not a valid credential
	/// (Cloudflare envelope code 9109).
	InvalidToken,
	/// The token is valid but not scoped to this account (envelope code 9103
	/// "Account not found").
	WrongAccountScope,
	/// The token is valid and account-scoped but lacks the Workers AI
	/// permission (envelope code 10000 "insufficient permission").
	WorkersAiPermission,
	/// Any auth rejection we could not classify further.
	GenericAuth,
}

impl fmt::Display for AuthScope {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let label = match self {
			Self::InvalidToken => "invalid API token",
			Self::WrongAccountScope => "wrong account scope",
			Self::WorkersAiPermission => "Workers AI permission failure",
			Self::GenericAuth => "authentication rejected",
		};
		f.write_str(label)
	}
}

#[derive(Debug, Error)]
pub enum CloudflareError {
	/// A required environment variable is missing or empty.
	#[error("Missing environment variable {env_var}. {hint}")]
	MissingEnv { env_var: &'static str, hint: String },

	/// Cloudflare returned an API error envelope (success: false).
	#[error("Cloudflare API error (code {code}): {message}")]
	Api { code: u32, message: String },

	/// An authentication/authorization rejection classified by scope
	/// ([`AuthScope::InvalidToken`] vs
	/// [`AuthScope::WrongAccountScope`] vs [`AuthScope::WorkersAiPermission`]
	/// are distinct, actionable failures. `message` is always token-scrubbed
	/// before it reaches this variant.
	#[error("Cloudflare auth rejected ({kind}): {message} (code {code})")]
	AuthRejected { kind: AuthScope, code: u32, message: String },

	/// The OpenRouter-format response had no `data` array.
	#[error("Cloudflare returned no OpenRouter-format data array at .data")]
	NoDataArray,

	/// HTTP transport failure during a catalog or verify call.
	#[error("Cloudflare request failed: {0}")]
	Http(String),
}

impl CloudflareError {
	/// True when the error came from the catalog endpoint (fetch_models
	/// falls back to the static list on these).
	pub fn is_catalog_failure(&self) -> bool {
		matches!(
			self,
			Self::Api { .. } | Self::AuthRejected { .. } | Self::NoDataArray | Self::Http(_)
		)
	}
}

#[cfg(test)]
mod tests {
	use super::{AuthScope, CloudflareError};
	use crate::auth::ACCOUNT_ENV;

	#[test]
	fn missing_env_names_the_var() {
		let error = CloudflareError::MissingEnv { env_var: ACCOUNT_ENV, hint: "export it".to_string() };
		let message = error.to_string();
		assert!(message.contains(ACCOUNT_ENV));
		assert!(!error.is_catalog_failure());
	}

	#[test]
	fn catalog_failures_fall_back() {
		assert!(CloudflareError::NoDataArray.is_catalog_failure());
		assert!(CloudflareError::Http("timeout".to_string()).is_catalog_failure());
		assert!(
			CloudflareError::AuthRejected {
				kind: AuthScope::InvalidToken,
				code: 9109,
				message: "Invalid access token".to_string(),
			}
			.is_catalog_failure(),
			"a classified auth rejection must trigger the static-list fallback"
		);
		assert!(
			!CloudflareError::MissingEnv { env_var: crate::auth::TOKEN_ENV, hint: String::new() }.is_catalog_failure()
		);
	}

	#[test]
	fn auth_rejected_display_is_token_free() {
		let error = CloudflareError::AuthRejected {
			kind: AuthScope::WorkersAiPermission,
			code: 10000,
			message: "insufficient permission".to_string(),
		};
		let rendered = error.to_string();
		assert!(
			rendered.contains("Workers AI permission failure"),
			"scope is actionable: {rendered}"
		);
		assert!(rendered.contains("10000"), "envelope code survives: {rendered}");
		assert!(!rendered.contains("cfut_"), "Display must never carry a token: {rendered}");
		assert!(
			!rendered.contains("Bearer"),
			"Display must never carry a Bearer header: {rendered}"
		);
	}
}
