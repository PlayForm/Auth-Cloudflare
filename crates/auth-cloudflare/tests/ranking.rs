//! Integration tests for the provider ranking score through the PUBLIC
//! `policy` API, driven by the bundled fixtures: unverified and
//! non-Passing records never rank, Llama Guard never ranks, a passing
//! DeepSeek outscores a passing GLM with a lower multi-turn tool-loop rate,
//! and the ranking weights sum to 1.0.

use auth_cloudflare::health::{CONFORMANCE_SUITE_VERSION, ModelVerification, VerificationConfidence, VerificationStatus};
use auth_cloudflare::policy::{ranking_score, ModelPolicy, ModelStatus, RankingBreakdown};
use auth_cloudflare::catalog::ModelRecord;

const DEEPSEEK: &str = include_str!("../fixtures/models/deepseek-v4-flash-0731.json");
const GLM: &str = include_str!("../fixtures/models/glm-5.3-flash.json");
const LLAMA_GUARD: &str = include_str!("../fixtures/models/llama-guard-3-8b.json");

fn record(fixture: &str) -> ModelRecord {
	let value: serde_json::Value = serde_json::from_str(fixture).expect("fixture is valid JSON");
	ModelRecord::from_openrouter(&value).expect("fixture normalizes")
}

/// A verification record with the given status, run counts, multi-turn
/// tool-loop rate, and median latency - everything else fixed.
fn verification(
	status: VerificationStatus,
	total: u32,
	successful: u32,
	multi: Option<f64>,
	latency: Option<u64>,
) -> ModelVerification {
	ModelVerification {
		model_id: "ranking-test".to_string(),
		latest_run_at: None,
		expires_at: None,
		suite_version: CONFORMANCE_SUITE_VERSION.to_string(),
		runner_version: "integration-test".to_string(),
		status,
		agent_eligible: true,
		confidence: VerificationConfidence::ConformanceTested,
		total_runs: total,
		successful_runs: successful,
		text_completion_success_rate: None,
		stream_completion_success_rate: None,
		single_tool_success_rate: Some(0.96),
		multi_turn_tool_success_rate: multi,
		structured_output_success_rate: None,
		median_latency_ms: latency,
		p95_latency_ms: None,
		total_failures: total.saturating_sub(successful),
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
fn unverified_models_never_rank() {
	let deepseek = record(DEEPSEEK);
	assert_eq!(ranking_score(&deepseek, None), None);
	let breakdown = RankingBreakdown::breakdown_for(&deepseek, None);
	assert_eq!(breakdown.score, None);
	assert_eq!(breakdown.model_id, "@cf/deepseek-ai/deepseek-v4-flash-0731");
}

#[test]
fn non_passing_verification_never_ranks() {
	let deepseek = record(DEEPSEEK);
	for status in [
		VerificationStatus::Degraded,
		VerificationStatus::Failing,
		VerificationStatus::Untested,
		VerificationStatus::Expired,
	] {
		let verification = verification(status, 100, 99, Some(0.96), Some(2_000));
		assert_eq!(ranking_score(&deepseek, Some(&verification)), None, "{status:?} must not rank");
	}
}

#[test]
fn llama_guard_never_ranks_even_when_passing() {
	// Llama Guard is hidden and never primary-agent eligible - even with a
	// passing verification record it must not rank.
	let guard = record(LLAMA_GUARD);
	let policy = ModelPolicy::default_policy();
	assert_eq!(policy.status_for(&guard.id), ModelStatus::Hidden);
	assert!(!policy.is_primary_agent_eligible(&guard.id));
	let verification = verification(VerificationStatus::Passing, 100, 100, Some(0.99), Some(1_000));
	assert_eq!(ranking_score(&guard, Some(&verification)), None);
}

#[test]
fn passing_deepseek_beats_passing_glm_on_tool_loop() {
	let deepseek = record(DEEPSEEK);
	let glm = record(GLM);
	// Identical delivery/latency evidence; GLM's multi-turn tool-loop rate
	// is lower, so GLM must rank below DeepSeek.
	let ds_verification = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(2_000));
	let glm_verification = verification(VerificationStatus::Passing, 100, 99, Some(0.70), Some(2_000));
	let ds_score = ranking_score(&deepseek, Some(&ds_verification)).expect("deepseek ranks");
	let glm_score = ranking_score(&glm, Some(&glm_verification)).expect("glm ranks");
	assert!(ds_score > glm_score, "deepseek {ds_score} must beat glm {glm_score}");
}

#[test]
fn ranking_weights_sum_to_one() {
	let deepseek = record(DEEPSEEK);
	let verification = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(2_000));
	let breakdown = RankingBreakdown::breakdown_for(&deepseek, Some(&verification));
	let sum = breakdown.weight_delivery
		+ breakdown.weight_tool_loop
		+ breakdown.weight_context
		+ breakdown.weight_latency
		+ breakdown.weight_price;
	assert!((sum - 1.0).abs() < 1e-12, "weights must sum to 1.0, got {sum}");
	assert_eq!(breakdown.score, ranking_score(&deepseek, Some(&verification)));
}

#[test]
fn degraded_but_cheaper_model_never_ranks() {
	// A very cheap, high-context model still scores None while Degraded.
	let deepseek = record(DEEPSEEK);
	let verification = verification(VerificationStatus::Degraded, 100, 60, Some(0.50), Some(1_000));
	assert_eq!(ranking_score(&deepseek, Some(&verification)), None);
}
