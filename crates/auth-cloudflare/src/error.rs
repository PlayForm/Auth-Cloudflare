//! Errors - actionable, token-free Cloudflare provider errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CloudflareError {
	/// A required environment variable is missing or empty.
	#[error("Missing environment variable {env_var}. {hint}")]
	MissingEnv { env_var: &'static str, hint: String },

	/// Cloudflare returned an API error envelope (success: false).
	#[error("Cloudflare API error (code {code}): {message}")]
	Api { code: u32, message: String },

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
		matches!(self, Self::Api { .. } | Self::NoDataArray | Self::Http(_))
	}
}

#[cfg(test)]
mod tests {
	use super::CloudflareError;
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
			!CloudflareError::MissingEnv { env_var: crate::auth::TOKEN_ENV, hint: String::new() }.is_catalog_failure()
		);
	}
}
