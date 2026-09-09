//! Cache - account-scoped, atomic cache paths for the Workers AI catalog.

use std::path::PathBuf;

use crate::auth::AuthProvider;

/// Env var overriding the Hermes home (matches `hermes`'s own resolution).
pub const HERMES_HOME_ENV: &str = "HERMES_HOME";

/// Return the account-scoped cache directory for one provider.
///
/// Layout: `$HERMES_HOME/cache/cloudflare/<slug>/` where `<slug>` is the
/// stable 16-hex account hash. The token never enters the cache path or any
/// file under it.
pub fn cache_dir_for_account(provider: &AuthProvider) -> PathBuf {
	let home = std::env::var(HERMES_HOME_ENV)
		.ok()
		.filter(|v| !v.trim().is_empty())
		.unwrap_or_else(|| format!("{}/.hermes", std::env::var("HOME").unwrap_or_else(|_| "~".to_string())));
	PathBuf::from(home).join("cache").join("cloudflare").join(provider.cache_slug())
}

/// Atomic write helper - write to a sibling temp file, then rename over the
/// destination. Never leave a half-written catalog behind.
pub fn atomic_write(destination: &std::path::Path, contents: &str) -> Result<(), crate::error::CloudflareError> {
	use std::io::Write;
	if let Some(parent) = destination.parent() {
		std::fs::create_dir_all(parent).map_err(|e| crate::error::CloudflareError::Http(e.to_string()))?;
	}
	let temp = destination.with_extension("tmp");
	{
		let mut file = std::fs::File::create(&temp).map_err(|e| crate::error::CloudflareError::Http(e.to_string()))?;
		file.write_all(contents.as_bytes())
			.map_err(|e| crate::error::CloudflareError::Http(e.to_string()))?;
		file.write_all(b"\n")
			.map_err(|e| crate::error::CloudflareError::Http(e.to_string()))?;
	}
	std::fs::rename(&temp, destination).map_err(|e| crate::error::CloudflareError::Http(e.to_string()))?;
	// 0o600 - user-private operational metadata.
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let _ = std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cache_dir_uses_slug() {
		let provider = AuthProvider::new("test-account");
		let dir = cache_dir_for_account(&provider);
		assert!(dir.ends_with(provider.cache_slug()));
		assert!(dir.to_string_lossy().contains("cache/cloudflare"));
	}

	#[test]
	fn atomic_write_renames_over() {
		let provider = AuthProvider::new("test-account");
		let dir = cache_dir_for_account(&provider);
		let destination = dir.join("catalog.json");
		atomic_write(&destination, "{}").expect("first write");
		atomic_write(&destination, "{\"v\":2}").expect("overwrite");
		let contents = std::fs::read_to_string(&destination).expect("read");
		assert!(contents.trim() == "{\"v\":2}");
		// No stray temp file left behind.
		assert!(!destination.with_extension("tmp").exists());
		let _ = std::fs::remove_file(&destination);
	}
}
