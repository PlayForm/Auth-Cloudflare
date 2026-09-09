//! Schema - versioned JSON contract metadata for the catalog cache and the
//! `auth-cloudflare version --format json` CLI contract (feedback 06).
//!
//! The Hermes plugin (`auth-hermes-cloudflare`) validates protocol and
//! catalog-schema compatibility against `VersionInfo::current()` before
//! invoking catalog commands; a newer unsupported schema version must be
//! rejected rather than silently guessed (feedback 02).

use serde::Serialize;

/// Catalog cache/snapshot JSON schema version (integer, not SemVer).
///
/// Bump when the on-disk `catalog.json`/`catalog.meta.json` layout or the
/// `catalog get --format json` envelope changes incompatibly.
pub const CATALOG_SCHEMA_VERSION: u32 = 1;

/// JSON CLI protocol version (`version`, `catalog`, `doctor` commands).
pub const PROTOCOL_VERSION: u32 = 1;

/// Version metadata returned by `auth-cloudflare version --format json`.
#[derive(Debug, Clone, Serialize)]
pub struct VersionInfo {
	/// Binary/package name, e.g. `auth-cloudflare`.
	pub name: &'static str,
	/// Cargo package version, e.g. `0.0.1` (from `crate::VERSION`).
	pub package_version: &'static str,
	/// Supported JSON CLI protocol version.
	pub protocol_version: u32,
	/// Catalog schema versions this binary can read/write.
	pub catalog_schema_versions: &'static [u32],
	/// Minimum `auth-hermes-cloudflare` plugin version this binary accepts.
	pub minimum_hermes_plugin_version: &'static str,
}

impl VersionInfo {
	/// Current crate version metadata - single source of truth for the
	/// `version --format json` command.
	pub fn current() -> Self {
		Self {
			name: "auth-cloudflare",
			package_version: crate::VERSION,
			protocol_version: PROTOCOL_VERSION,
			catalog_schema_versions: &[CATALOG_SCHEMA_VERSION],
			minimum_hermes_plugin_version: "0.0.1",
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn version_info_current_matches_contract() {
		let info = VersionInfo::current();
		assert_eq!(info.name, "auth-cloudflare");
		assert_eq!(info.package_version, crate::VERSION);
		assert_eq!(info.protocol_version, 1);
		assert_eq!(info.catalog_schema_versions, &[1]);
		assert_eq!(info.minimum_hermes_plugin_version, "0.0.1");
	}

	#[test]
	fn version_info_serializes_to_feedback_06_shape() {
		let value = serde_json::to_value(VersionInfo::current()).expect("serialize");
		let object = value.as_object().expect("object");
		// Exact feedback-06 key set: name, package_version, protocol_version,
		// catalog_schema_versions, minimum_hermes_plugin_version.
		assert_eq!(
			object.keys().cloned().collect::<std::collections::BTreeSet<_>>(),
			[
				"name",
				"package_version",
				"protocol_version",
				"catalog_schema_versions",
				"minimum_hermes_plugin_version",
			]
			.into_iter()
			.map(String::from)
			.collect()
		);
		assert_eq!(value["name"], "auth-cloudflare");
		assert_eq!(value["package_version"], crate::VERSION);
		assert_eq!(value["protocol_version"], 1);
		assert_eq!(value["catalog_schema_versions"], serde_json::json!([1]));
		assert_eq!(value["minimum_hermes_plugin_version"], "0.0.1");
	}
}
