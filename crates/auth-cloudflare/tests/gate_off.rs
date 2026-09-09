//! Integration tests for the live-test gate (feedback 02: paid inference is
//! opt-in): with `AUTH_CLOUDFLARE_LIVE_TESTS` unset or not exactly `"1"`,
//! `live_tests_enabled` is false and the PUBLIC entry points
//! `verify::run_smoke_suite` / `tool_loop::run_tool_loop` refuse with
//! `CloudflareError::MissingEnv` naming the variable - BEFORE any HTTP is
//! attempted.
//!
//! Zero network: this file never sets `AUTH_CLOUDFLARE_LIVE_TESTS` to `"1"`
//! (the gate is never opened); the only values ever assigned are
//! non-triggering ones (`"0"`, `"yes"`, `" 1 "`) under a mutex with
//! save/restore, mirroring the crate's own unit-test isolation. The refusal
//! happens at the first line of the public function, so no socket is ever
//! created - the gate-off tests are the proof.

use std::sync::Mutex;
use std::time::Duration;

use auth_cloudflare::config::{ConfigBuilder, SecretString};
use auth_cloudflare::error::CloudflareError;
use auth_cloudflare::tool_loop::{
	live_tests_enabled as tool_loop_live_tests_enabled, run_tool_loop, LIVE_TESTS_ENV as TOOL_LOOP_LIVE_TESTS_ENV,
};
use auth_cloudflare::verify::{live_tests_enabled as verify_live_tests_enabled, run_smoke_suite, LIVE_TESTS_ENV};

/// Synthetic Cloudflare-shaped account id (32 hex digits) - never real.
const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
/// Synthetic token - never a real credential.
const TOKEN: &str = "cfut_test_synthetic_token_0001";

/// `std::env` is process-global and tests run in parallel - serialize env
/// mutation through a static mutex and restore prior values after.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with `AUTH_CLOUDFLARE_LIVE_TESTS` set to `value` (or unset),
/// restoring the prior value afterwards. Never called with `"1"`.
fn with_live_tests_env(value: Option<&str>, f: impl FnOnce()) {
	let _guard = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
	let saved = std::env::var(LIVE_TESTS_ENV).ok();
	match value {
		Some(value) => std::env::set_var(LIVE_TESTS_ENV, value),
		None => std::env::remove_var(LIVE_TESTS_ENV),
	}
	f();
	match saved {
		Some(saved) => std::env::set_var(LIVE_TESTS_ENV, saved),
		None => std::env::remove_var(LIVE_TESTS_ENV),
	}
}

/// A config that resolves without touching the environment beyond the live
/// gate var - the smoke refusal happens before any config value is read.
fn gate_off_config() -> auth_cloudflare::config::Config {
	let scratch = std::env::temp_dir().join(format!("auth-cloudflare-it-gate-{}", std::process::id()));
	ConfigBuilder::new()
		.account_id(ACCOUNT)
		.api_token(TOKEN)
		.base_url("https://example.test/ai/v1")
		.cache_dir(scratch.join("cache"))
		.config_path(scratch.join("config.json"))
		.build()
		.expect("pinned config resolves")
}

#[test]
fn live_tests_enabled_is_false_when_unset_or_not_exactly_one() {
	with_live_tests_env(None, || {
		assert!(!verify_live_tests_enabled(), "unset must close the gate");
		assert!(!tool_loop_live_tests_enabled(), "unset must close the tool-loop gate");
	});
	// Non-"1" values must also close the gate - the gate only opens on
	// exactly "1", which this file never sets.
	for value in ["0", "yes", " 1 "] {
		with_live_tests_env(Some(value), || {
			assert!(!verify_live_tests_enabled(), "{value:?} must close the gate");
			assert!(!tool_loop_live_tests_enabled(), "{value:?} must close the tool-loop gate");
		});
	}
}

#[test]
fn run_smoke_suite_refuses_without_gate_and_without_network() {
	with_live_tests_env(None, || {
		let config = gate_off_config();
		let error = run_smoke_suite(&config, "@cf/deepseek-ai/deepseek-v4-flash-0731")
			.expect_err("the closed gate must refuse before any HTTP");
		match error {
			CloudflareError::MissingEnv { env_var, hint } => {
				assert_eq!(env_var, LIVE_TESTS_ENV, "refusal must name AUTH_CLOUDFLARE_LIVE_TESTS");
				assert!(hint.contains("AUTH_CLOUDFLARE_LIVE_TESTS=1"));
			},
			other => panic!("expected MissingEnv refusal, got {other:?}"),
		}
	});
}

#[test]
fn run_tool_loop_refuses_without_gate_and_without_network() {
	with_live_tests_env(None, || {
		let token = SecretString::new(TOKEN);
		let error = run_tool_loop(
			ACCOUNT,
			&token,
			"https://example.test/ai/v1",
			"@cf/deepseek-ai/deepseek-v4-flash-0731",
			Duration::from_secs(1),
		)
		.expect_err("the closed gate must refuse before any HTTP");
		match error {
			CloudflareError::MissingEnv { env_var, hint } => {
				assert_eq!(
					env_var, TOOL_LOOP_LIVE_TESTS_ENV,
					"refusal must name AUTH_CLOUDFLARE_LIVE_TESTS"
				);
				assert!(
					hint.to_lowercase().contains("opt-in"),
					"hint must explain the opt-in gate: {hint}"
				);
			},
			other => panic!("expected MissingEnv refusal, got {other:?}"),
		}
	});
}
