//! Cache - account-scoped, atomic cache paths and versioned catalog cache for
//! the Workers AI catalog.
//!
//! Layout: `$HERMES_HOME/cache/auth-cloudflare/<slug>/`. Each account's
//! catalog is stored as two files written atomically:
//! - `catalog.json` - the raw payload (`serde_json::Value`);
//! - `catalog.meta.json` - `CatalogCacheMeta` (schema version, fetch time,
//!   provenance, model count, account fingerprint).
//!
//! Reads return `Ok(None)` when the cache is absent, and `Err` when a present
//! file is truncated or corrupt - callers fall back to a live fetch or the
//! bundled fallback catalog (never panic).

use std::path::{Path, PathBuf};

use crate::auth::AuthProvider;

/// Cache payload file name (account-scoped).
const CATALOG_FILE: &str = "catalog.json";
/// Cache metadata file name (account-scoped).
const CATALOG_META_FILE: &str = "catalog.meta.json";

/// Env var overriding the Hermes home (matches `hermes`'s own resolution).
pub const HERMES_HOME_ENV: &str = "HERMES_HOME";

/// Return the account-scoped cache directory for one provider.
///
/// Layout: `$HERMES_HOME/cache/auth-cloudflare/<slug>/` where `<slug>` is the
/// stable 16-hex FNV-1a account hash (`AuthProvider::cache_slug`). The token
/// never enters the cache path or any file under it.
pub fn cache_dir_for_account(provider: &AuthProvider) -> PathBuf {
	let home = std::env::var(HERMES_HOME_ENV)
		.ok()
		.filter(|v| !v.trim().is_empty())
		.unwrap_or_else(|| format!("{}/.hermes", std::env::var("HOME").unwrap_or_else(|_| "~".to_string())));
	PathBuf::from(home)
		.join("cache")
		.join("auth-cloudflare")
		.join(provider.cache_slug())
}

/// Metadata recorded alongside every cached catalog payload (the cache
/// records timestamps, provenance, and model count).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CatalogCacheMeta {
	/// Catalog JSON schema version (`crate::schema::CATALOG_SCHEMA_VERSION`).
	pub schema_version: u32,
	/// RFC 3339 UTC fetch timestamp (`2026-09-09T15:30:00Z`).
	pub fetched_at: String,
	/// Provenance of the payload, e.g. `cloudflare-workers-ai`.
	pub source: String,
	/// Number of model records in the payload.
	pub model_count: usize,
	/// Opaque, token-free account fingerprint (the cache slug hash).
	pub account_fingerprint: String,
}

/// Read the versioned catalog cache for an account-scoped directory.
///
/// Returns `Ok(None)` when either cache file is missing (no cache yet).
/// Returns `Err(CloudflareError::Http)` on I/O failure or when a present file
/// is truncated/corrupt JSON - the caller falls back to a live fetch or the
/// bundled fallback catalog. Never panics.
pub fn read_catalog_cache(
	dir: &Path,
) -> Result<Option<(CatalogCacheMeta, serde_json::Value)>, crate::error::CloudflareError> {
	let meta_path = dir.join(CATALOG_META_FILE);
	let payload_path = dir.join(CATALOG_FILE);
	if !meta_path.exists() || !payload_path.exists() {
		return Ok(None);
	}
	let meta_raw = std::fs::read_to_string(&meta_path)
		.map_err(|e| crate::error::CloudflareError::Http(format!("read {}: {e}", meta_path.display())))?;
	let payload_raw = std::fs::read_to_string(&payload_path)
		.map_err(|e| crate::error::CloudflareError::Http(format!("read {}: {e}", payload_path.display())))?;
	let meta: CatalogCacheMeta = serde_json::from_str(&meta_raw)
		.map_err(|e| crate::error::CloudflareError::Http(format!("parse {}: {e}", meta_path.display())))?;
	let payload: serde_json::Value = serde_json::from_str(&payload_raw)
		.map_err(|e| crate::error::CloudflareError::Http(format!("parse {}: {e}", payload_path.display())))?;
	Ok(Some((meta, payload)))
}

/// Write the versioned catalog cache for an account-scoped directory.
///
/// Creates `dir` if needed and writes both files atomically (a sibling
/// `.tmp` file renamed into place, mode `0o600`). The payload is written
/// first; the meta file is the commit point, so a concurrent or crashed
/// reader never sees metadata without a fully written payload.
pub fn write_catalog_cache(
	dir: &Path,
	meta: &CatalogCacheMeta,
	payload: &serde_json::Value,
) -> Result<(), crate::error::CloudflareError> {
	std::fs::create_dir_all(dir)
		.map_err(|e| crate::error::CloudflareError::Http(format!("create {}: {e}", dir.display())))?;
	let payload_json = serde_json::to_string(payload)
		.map_err(|e| crate::error::CloudflareError::Http(format!("serialize payload: {e}")))?;
	let meta_json =
		serde_json::to_string(meta).map_err(|e| crate::error::CloudflareError::Http(format!("serialize meta: {e}")))?;
	atomic_write(&dir.join(CATALOG_FILE), &payload_json)?;
	atomic_write(&dir.join(CATALOG_META_FILE), &meta_json)
}

/// True when the cache metadata is older than `max_age`.
///
/// `fetched_at` is parsed as RFC 3339 via chrono; an unparseable timestamp is
/// treated as stale (the caller refetches rather than trusting a broken
/// record). A timestamp in the future is not stale.
pub fn cache_is_stale(meta: &CatalogCacheMeta, max_age: std::time::Duration) -> bool {
	let fetched_at = match chrono::DateTime::parse_from_rfc3339(&meta.fetched_at) {
		Ok(parsed) => parsed,
		Err(_) => return true,
	};
	let max = match chrono::TimeDelta::from_std(max_age) {
		Ok(d) => d,
		// Degenerate guard: conversion only fails for absurd durations.
		Err(_) => chrono::TimeDelta::MAX,
	};
	chrono::Utc::now().signed_duration_since(fetched_at) > max
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
	use crate::schema::CATALOG_SCHEMA_VERSION;

	/// Unique scratch dir per test - tests run in parallel.
	fn scratch_dir(name: &str) -> PathBuf {
		std::env::temp_dir().join(format!("auth-cloudflare-cache-test-{}-{name}", std::process::id()))
	}

	#[test]
	fn cache_dir_uses_auth_cloudflare_root_and_slug() {
		let provider = AuthProvider::new("test-account");
		let dir = cache_dir_for_account(&provider);
		assert!(dir.ends_with(provider.cache_slug()));
		let rendered = dir.to_string_lossy();
		assert!(rendered.contains("cache/auth-cloudflare"));
		assert!(!rendered.contains("cache/cloudflare/"));
	}

	#[test]
	fn atomic_write_renames_over() {
		let dir = scratch_dir("atomic-rename");
		std::fs::create_dir_all(&dir).expect("create scratch dir");
		let destination = dir.join("catalog.json");
		atomic_write(&destination, "{}").expect("first write");
		atomic_write(&destination, "{\"v\":2}").expect("overwrite");
		let contents = std::fs::read_to_string(&destination).expect("read");
		assert!(contents.trim() == "{\"v\":2}");
		// No stray temp file left behind.
		assert!(!destination.with_extension("tmp").exists());
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn write_read_roundtrip_preserves_meta_and_payload() {
		let dir = scratch_dir("roundtrip");
		let meta = CatalogCacheMeta {
			schema_version: CATALOG_SCHEMA_VERSION,
			fetched_at: "2026-09-09T15:30:00Z".to_string(),
			source: "cloudflare-workers-ai".to_string(),
			model_count: 27,
			account_fingerprint: "0123456789abcdef".to_string(),
		};
		let payload = serde_json::json!({
			"schema_version": 1,
			"models": [{"id": "@cf/deepseek-ai/deepseek-v4-flash-0731"}]
		});
		write_catalog_cache(&dir, &meta, &payload).expect("write");
		let (read_meta, read_payload) = read_catalog_cache(&dir).expect("read").expect("cache present");
		assert_eq!(read_meta, meta);
		assert_eq!(read_payload, payload);
		// Both files exist with no stray temps.
		assert!(dir.join(CATALOG_META_FILE).exists());
		assert!(dir.join(CATALOG_FILE).exists());
		assert!(!dir.join("catalog.tmp").exists());
		assert!(!dir.join("catalog.meta.tmp").exists());
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn read_missing_cache_returns_none() {
		let dir = scratch_dir("missing");
		let _ = std::fs::remove_dir_all(&dir);
		assert!(read_catalog_cache(&dir).expect("read").is_none());
		// Only meta present, payload absent -> treated as no cache.
		std::fs::create_dir_all(&dir).expect("create");
		atomic_write(&dir.join(CATALOG_META_FILE), "{}").expect("meta write");
		assert!(read_catalog_cache(&dir).expect("read").is_none());
		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn corrupt_cache_file_returns_err_without_panic() {
		let dir = scratch_dir("corrupt");
		let meta = CatalogCacheMeta {
			schema_version: CATALOG_SCHEMA_VERSION,
			fetched_at: "2026-09-09T15:30:00Z".to_string(),
			source: "cloudflare-workers-ai".to_string(),
			model_count: 27,
			account_fingerprint: "0123456789abcdef".to_string(),
		};
		write_catalog_cache(&dir, &meta, &serde_json::json!({"models": []})).expect("write");

		// Truncated/corrupt payload -> Err, no panic.
		std::fs::write(dir.join(CATALOG_FILE), b"{\"models\": [truncated").expect("corrupt payload");
		assert!(read_catalog_cache(&dir).is_err());

		// Corrupt meta -> Err, no panic.
		write_catalog_cache(&dir, &meta, &serde_json::json!({"models": []})).expect("rewrite");
		std::fs::write(dir.join(CATALOG_META_FILE), b"not json at all{").expect("corrupt meta");
		assert!(read_catalog_cache(&dir).is_err());

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn stale_detection_fresh_vs_old() {
		let max_age = std::time::Duration::from_secs(3600);
		let fresh = CatalogCacheMeta {
			schema_version: CATALOG_SCHEMA_VERSION,
			fetched_at: chrono::Utc::now().to_rfc3339(),
			source: "cloudflare-workers-ai".to_string(),
			model_count: 27,
			account_fingerprint: "0123456789abcdef".to_string(),
		};
		assert!(!cache_is_stale(&fresh, max_age));

		let ten_year_old = CatalogCacheMeta { fetched_at: "2016-01-01T00:00:00Z".to_string(), ..fresh.clone() };
		assert!(cache_is_stale(&ten_year_old, max_age));

		// Unparseable timestamp -> stale.
		let broken = CatalogCacheMeta { fetched_at: "yesterday-ish".to_string(), ..fresh.clone() };
		assert!(cache_is_stale(&broken, max_age));
	}
}
