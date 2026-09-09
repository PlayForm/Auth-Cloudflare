//! Integration tests for the versioned `model-health.json` store through
//! the PUBLIC `verify` API: missing store → empty, atomic save (0o600, no
//! stray `.tmp`), read-modify-write preservation of other models' records,
//! and typed errors (never panics) for corrupt files and unsupported schema
//! versions.
//!
//! All scratch dirs live under `std::env::temp_dir()` and are removed by
//! the test; nothing is written into the repository.

use std::path::PathBuf;

use chrono::Utc;

use auth_cloudflare::health::{CONFORMANCE_SUITE_VERSION, ModelVerification, VerificationConfidence, VerificationStatus};
use auth_cloudflare::verify::{
	load_health_store, save_health_store, save_verification, HealthStore, HEALTH_STORE_FILE, HEALTH_STORE_VERSION,
};

const MODEL_A: &str = "@cf/deepseek-ai/deepseek-v4-flash-0731";
const MODEL_B: &str = "@cf/zai-org/glm-5.3-flash";

/// Unique scratch dir per test - tests run in parallel.
fn scratch_dir(name: &str) -> PathBuf {
	std::env::temp_dir().join(format!("auth-cloudflare-it-health-{}-{name}", std::process::id()))
}

/// A minimal valid verification record for store tests.
fn verification_record(model_id: &str, status: VerificationStatus) -> ModelVerification {
	ModelVerification {
		model_id: model_id.to_string(),
		latest_run_at: Some(Utc::now()),
		expires_at: None,
		suite_version: CONFORMANCE_SUITE_VERSION.to_string(),
		runner_version: "integration-test".to_string(),
		status,
		agent_eligible: true,
		confidence: VerificationConfidence::SmokeTested,
		total_runs: 3,
		successful_runs: 3,
		text_completion_success_rate: Some(1.0),
		stream_completion_success_rate: Some(1.0),
		single_tool_success_rate: Some(1.0),
		multi_turn_tool_success_rate: None,
		structured_output_success_rate: None,
		median_latency_ms: Some(100),
		p95_latency_ms: None,
		total_failures: 0,
		timeout_failures: 0,
		transport_failures: 0,
		provider_5xx_failures: 0,
		malformed_response_failures: 0,
		malformed_tool_call_failures: 0,
		tool_loop_failures: 0,
		last_failure: None,
	}
}

#[test]
fn missing_store_loads_empty() {
	let dir = scratch_dir("missing");
	let _ = std::fs::remove_dir_all(&dir);
	let store = load_health_store(&dir).expect("a missing store is an empty store, not an error");
	assert_eq!(store.version, HEALTH_STORE_VERSION);
	assert!(store.records.is_empty());
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_writes_atomic_private_file_without_tmp_leftover() {
	let dir = scratch_dir("roundtrip");
	let _ = std::fs::remove_dir_all(&dir);
	let mut store = HealthStore::new();
	store.upsert(verification_record(MODEL_A, VerificationStatus::Passing));
	save_health_store(&dir, &store).expect("save");

	let path = dir.join(HEALTH_STORE_FILE);
	assert!(path.exists(), "store file must exist after save");
	// The atomic write leaves no sibling temp file behind.
	assert!(!dir.join("model-health.tmp").exists(), "no .tmp may survive a save");

	// User-private permissions on unix.
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let mode = std::fs::metadata(&path).expect("stat store").permissions().mode() & 0o777;
		assert_eq!(mode, 0o600, "health store must be user-private (0o600), got {mode:#o}");
	}

	let loaded = load_health_store(&dir).expect("load");
	assert_eq!(loaded.version, HEALTH_STORE_VERSION);
	// `ModelVerification` deliberately has no `PartialEq`, so compare
	// through serialization.
	assert_eq!(
		serde_json::to_value(&loaded.records).expect("records serialize"),
		serde_json::to_value(&store.records).expect("records serialize")
	);
	assert_eq!(
		loaded.get(MODEL_A).map(|record| record.status),
		Some(VerificationStatus::Passing)
	);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_verification_preserves_other_models() {
	let dir = scratch_dir("read-modify-write");
	let _ = std::fs::remove_dir_all(&dir);
	save_verification(&dir, &verification_record(MODEL_A, VerificationStatus::Passing)).expect("seed A");
	save_verification(&dir, &verification_record(MODEL_B, VerificationStatus::Failing)).expect("upsert B");
	let store = load_health_store(&dir).expect("load");
	assert!(store.get(MODEL_A).is_some(), "model A's record must survive model B's upsert");
	assert_eq!(
		store.get(MODEL_B).map(|record| record.status),
		Some(VerificationStatus::Failing)
	);
	assert_eq!(store.records.len(), 2);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_store_errors_without_panic() {
	let dir = scratch_dir("corrupt");
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).expect("create scratch dir");
	std::fs::write(dir.join(HEALTH_STORE_FILE), b"not json at all{").expect("write corrupt store");
	let error = load_health_store(&dir).expect_err("corrupt JSON must be an error, not a panic");
	assert!(
		error.to_string().contains("model-health.json"),
		"error must name the file: {error}"
	);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn version_mismatch_errors_without_panic() {
	let dir = scratch_dir("version-mismatch");
	let _ = std::fs::remove_dir_all(&dir);
	std::fs::create_dir_all(&dir).expect("create scratch dir");
	std::fs::write(
		dir.join(HEALTH_STORE_FILE),
		br#"{"version":99,"updated_at":"2026-09-09T00:00:00Z","records":{}}"#,
	)
	.expect("write v99 store");
	let error = load_health_store(&dir).expect_err("unsupported schema version must be an error");
	assert!(
		error.to_string().contains("99"),
		"error must name the offending version: {error}"
	);
	let _ = std::fs::remove_dir_all(&dir);
}
