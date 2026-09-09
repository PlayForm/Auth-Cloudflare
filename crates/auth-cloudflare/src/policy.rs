//! Policy - status, rank, and primary-agent eligibility for known models.
//!
//! This module owns the bundled policy record (feedback 01/02/05/06): the
//! status, rank, default flag, and primary-agent eligibility of every model
//! the project has an explicit opinion about. Unknown models default to
//! `Available`; the policy never claims a model is ineligible unless a
//! policy entry says so. The JSON contract serializes as snake_case so the
//! CLI and the Hermes plugin can consume it without transformation.

use serde::{Deserialize, Serialize};

use crate::catalog::{ModelRecord, ModelRole, PricingPerMillion};
use crate::health::{ModelVerification, VerificationStatus};
use crate::{DEFAULT_MODEL, EXPERIMENTAL_MODEL, PREMIUM_CODING_MODEL, PREMIUM_REASONING_MODEL};

/// Version of the bundled default policy document (feedback 02 YAML `version: 1`).
pub const POLICY_VERSION: &str = "1";

/// Picker status for a model (feedback 02 status vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
	/// Top picker tier; shown first.
	Recommended,
	/// Normal picker tier.
	#[default]
	Available,
	/// Selectable but not default; delivery conformance not yet validated.
	Experimental,
	/// Verified delivery below threshold; warning surfaced.
	Degraded,
	/// Not selectable.
	Blocked,
	/// Legacy; superseded.
	Deprecated,
	/// Hidden from the normal picker (safety/classification).
	Hidden,
}

fn default_true() -> bool {
	true
}

/// One policy opinion about one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PolicyEntry {
	pub model_id: String,
	pub status: ModelStatus,
	/// Lower rank = shown higher in the picker (10, 20, 30…).
	pub rank: u32,
	#[serde(default)]
	pub default: bool,
	#[serde(default = "default_true")]
	pub primary_agent_eligible: bool,
	pub roles: Vec<ModelRole>,
	/// Human-readable reason, surfaced in picker/doctor output.
	pub reason: String,
	pub cost_tier: Option<String>,
}

/// Versioned policy document - the JSON contract envelope for `policy get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelPolicy {
	pub version: String,
	pub models: Vec<PolicyEntry>,
}

impl Default for ModelPolicy {
	fn default() -> Self {
		Self::default_policy()
	}
}

impl ModelPolicy {
	/// Bundled default policy (feedback 01/05/06):
	/// DeepSeek V4 Flash recommended/default, GLM-5.3 Flash experimental,
	/// Llama Guard 3 8B hidden and never primary-agent eligible.
	pub fn default_policy() -> Self {
		Self {
			version: POLICY_VERSION.to_string(),
			models: vec![
				PolicyEntry {
					model_id: DEFAULT_MODEL.to_string(),
					status: ModelStatus::Recommended,
					rank: 10,
					default: true,
					primary_agent_eligible: true,
					roles: vec![ModelRole::CodingAgent, ModelRole::GeneralChat],
					reason: "Development default: validated reliability and tool-agent suitability.".to_string(),
					cost_tier: None,
				},
				PolicyEntry {
					model_id: PREMIUM_REASONING_MODEL.to_string(),
					status: ModelStatus::Recommended,
					rank: 20,
					default: false,
					primary_agent_eligible: true,
					roles: vec![ModelRole::Reasoning],
					reason: "Premium reasoning model.".to_string(),
					cost_tier: Some("high".to_string()),
				},
				PolicyEntry {
					model_id: PREMIUM_CODING_MODEL.to_string(),
					status: ModelStatus::Recommended,
					rank: 30,
					default: false,
					primary_agent_eligible: true,
					roles: vec![ModelRole::CodingAgent],
					reason: "Premium coding model.".to_string(),
					cost_tier: Some("high".to_string()),
				},
				PolicyEntry {
					model_id: "@cf/openai/gpt-oss-120b".to_string(),
					status: ModelStatus::Available,
					rank: 40,
					default: false,
					primary_agent_eligible: true,
					roles: vec![ModelRole::GeneralChat],
					reason: "General fallback model.".to_string(),
					cost_tier: None,
				},
				PolicyEntry {
					model_id: "@cf/openai/gpt-oss-20b".to_string(),
					status: ModelStatus::Available,
					rank: 50,
					default: false,
					primary_agent_eligible: true,
					roles: vec![ModelRole::GeneralChat],
					reason: "Budget general model.".to_string(),
					cost_tier: None,
				},
				PolicyEntry {
					model_id: EXPERIMENTAL_MODEL.to_string(),
					status: ModelStatus::Experimental,
					rank: 900,
					default: false,
					primary_agent_eligible: true,
					roles: vec![ModelRole::CodingAgent],
					reason: "Observed delivery failures; requires passing conformance suite.".to_string(),
					cost_tier: None,
				},
				PolicyEntry {
					model_id: "@cf/meta/llama-guard-3-8b".to_string(),
					status: ModelStatus::Hidden,
					rank: 1000,
					default: false,
					primary_agent_eligible: false,
					roles: vec![ModelRole::Safety],
					reason: "Safety classifier.".to_string(),
					cost_tier: None,
				},
			],
		}
	}

	/// Status for a model; unknown models are `Available`, never blocked by absence.
	pub fn status_for(&self, model_id: &str) -> ModelStatus {
		self.models
			.iter()
			.find(|entry| entry.model_id == model_id)
			.map(|entry| entry.status)
			.unwrap_or_default()
	}

	/// Primary-agent eligibility; unknown models are eligible unless a policy
	/// entry (e.g. Llama Guard) explicitly excludes them.
	pub fn is_primary_agent_eligible(&self, model_id: &str) -> bool {
		self.models
			.iter()
			.find(|entry| entry.model_id == model_id)
			.map(|entry| entry.primary_agent_eligible)
			.unwrap_or(true)
	}

	/// The single default model id, if the policy marks one.
	pub fn default_model(&self) -> Option<&str> {
		self.models
			.iter()
			.find(|entry| entry.default)
			.map(|entry| entry.model_id.as_str())
	}
}

/// Weight of the recent-delivery component in the provider ranking score
/// (feedback 02: score = 0.40R + 0.25T + 0.15C + 0.10L + 0.10P).
pub const WEIGHT_DELIVERY: f64 = 0.40;
/// Weight of the multi-turn tool-loop conformance component.
pub const WEIGHT_TOOL_LOOP: f64 = 0.25;
/// Weight of the context-window suitability component.
pub const WEIGHT_CONTEXT: f64 = 0.15;
/// Weight of the latency component.
pub const WEIGHT_LATENCY: f64 = 0.10;
/// Weight of the price/value component.
pub const WEIGHT_PRICE: f64 = 0.10;

/// Context window (tokens) that normalizes to 1.0 - DeepSeek V4 Flash's
/// advertised window. Anything at or beyond this caps at 1.0.
pub const REFERENCE_CONTEXT_TOKENS: u64 = 1_310_720;
/// Median latency (ms) at or beyond which the latency score is 0.0 (60s).
pub const REFERENCE_LATENCY_MS: f64 = 60_000.0;
/// Average per-million-token price (USD) at or beyond which the price score
/// is 0.0.
pub const REFERENCE_PRICE_PER_MILLION: f64 = 10.0;

/// Evidence breakdown for one model's [`ranking_score`] - the picker renders
/// one line per component from this struct, so every ranking decision is
/// explainable (feedback 02 evidence lines).
///
/// The component fields hold the same normalized 0..1 scores used by
/// [`ranking_score`]: the weighted dot product of components × weights IS
/// the score whenever the score is `Some`. The picker renders RAW evidence
/// (success rates, token counts, dollar prices) directly from
/// [`ModelVerification`]/[`ModelRecord`] alongside these normalized values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RankingBreakdown {
	pub model_id: String,
	/// [`ranking_score`] result: `None` when the model is not primary-agent
	/// eligible, unverified, or not passing - unverified never outranks.
	pub score: Option<f64>,
	pub delivery: f64,
	pub tool_loop: f64,
	pub context: f64,
	pub latency: f64,
	pub price: f64,
	pub weight_delivery: f64,
	pub weight_tool_loop: f64,
	pub weight_context: f64,
	pub weight_latency: f64,
	pub weight_price: f64,
}

impl RankingBreakdown {
	/// Render the full evidence breakdown for one model + verification pair
	/// (normalized component scores, the fixed weights, and the resulting
	/// score). Components are still rendered when the score is `None` so the
	/// picker can explain WHY the model did not rank.
	pub fn breakdown_for(model: &ModelRecord, verification: Option<&ModelVerification>) -> Self {
		let delivery = verification.map(delivery_score).unwrap_or(0.0);
		let tool_loop = verification.and_then(|v| v.multi_turn_tool_success_rate).unwrap_or(0.0);
		let context = context_score(model);
		let latency = verification.map(latency_score).unwrap_or(0.0);
		let price = price_score(&model.pricing);
		let score = ranking_score(model, verification);
		Self {
			model_id: model.id.clone(),
			score,
			delivery,
			tool_loop,
			context,
			latency,
			price,
			weight_delivery: WEIGHT_DELIVERY,
			weight_tool_loop: WEIGHT_TOOL_LOOP,
			weight_context: WEIGHT_CONTEXT,
			weight_latency: WEIGHT_LATENCY,
			weight_price: WEIGHT_PRICE,
		}
	}
}

/// Provider ranking score (feedback 02: score = 0.40R + 0.25T + 0.15C +
/// 0.10L + 0.10P).
///
/// Returns `None` - and therefore NEVER outranks anything - when:
///
/// - the model is not primary-agent eligible per
///   [`ModelPolicy::default_policy()`] (e.g. Llama Guard);
/// - `verification` is `None` (unverified never outranks, even for cheaper
///   or larger-context models);
/// - `verification.status` is not [`VerificationStatus::Passing`] (a
///   Degraded-but-cheaper model still scores `None`).
///
/// Components, with the exact normalization formulas (documented so the
/// picker can explain every number):
///
/// - `R` delivery = `successful_runs / total_runs.max(1)` (a zero-run
///   record contributes 0.0, never a bonus);
/// - `T` tool-loop = `multi_turn_tool_success_rate`, missing → 0.0;
/// - `C` context = `min(context_tokens / 1_310_720, 1.0)`, missing → 0.0
///   (bigger window up to the reference window is better; beyond it the
///   score is capped at 1.0);
/// - `L` latency = `1.0 - min(median_latency_ms / 60_000, 1.0)`, missing →
///   0.0 (lower median latency is better; 60s or more scores 0.0);
/// - `P` price = `1.0 - min(avg_price_per_million / 10.0, 1.0)` where
///   `avg_price_per_million = (input + output) / 2` when both prices are
///   present, else the present one, else 0.0 - a model with NO price data
///   scores 0.0 (missing is never treated as free); cheaper is better.
pub fn ranking_score(model: &ModelRecord, verification: Option<&ModelVerification>) -> Option<f64> {
	let policy = ModelPolicy::default_policy();
	if !policy.is_primary_agent_eligible(&model.id) {
		return None;
	}
	let verification = verification?;
	if verification.status != VerificationStatus::Passing {
		return None;
	}
	Some(
		WEIGHT_DELIVERY * delivery_score(verification)
			+ WEIGHT_TOOL_LOOP * verification.multi_turn_tool_success_rate.unwrap_or(0.0)
			+ WEIGHT_CONTEXT * context_score(model)
			+ WEIGHT_LATENCY * latency_score(verification)
			+ WEIGHT_PRICE * price_score(&model.pricing),
	)
}

/// Delivery component: `successful_runs / total_runs.max(1)` - a zero-run
/// record contributes 0.0, never a bonus.
fn delivery_score(verification: &ModelVerification) -> f64 {
	verification.successful_runs as f64 / verification.total_runs.max(1) as f64
}

/// Context component: `min(context_tokens / 1_310_720, 1.0)`, missing → 0.0.
fn context_score(model: &ModelRecord) -> f64 {
	model
		.limits
		.context_tokens
		.map(|tokens| (tokens as f64 / REFERENCE_CONTEXT_TOKENS as f64).min(1.0))
		.unwrap_or(0.0)
}

/// Latency component: `1.0 - min(median_latency_ms / 60_000, 1.0)`, missing
/// → 0.0; lower is better.
fn latency_score(verification: &ModelVerification) -> f64 {
	verification
		.median_latency_ms
		.map(|ms| 1.0 - (ms as f64 / REFERENCE_LATENCY_MS).min(1.0))
		.unwrap_or(0.0)
}

/// Price component: `1.0 - min(avg_price_per_million / 10.0, 1.0)` where
/// `avg_price_per_million = (input + output) / 2` when both prices are
/// present, else the present one, else 0.0; a model with NO price data
/// scores 0.0 (missing is never free); cheaper is better.
fn price_score(pricing: &PricingPerMillion) -> f64 {
	match (pricing.input, pricing.output) {
		(None, None) => 0.0,
		(Some(input), Some(output)) => 1.0 - (((input + output) / 2.0) / REFERENCE_PRICE_PER_MILLION).min(1.0),
		(Some(input), None) => 1.0 - (input / REFERENCE_PRICE_PER_MILLION).min(1.0),
		(None, Some(output)) => 1.0 - (output / REFERENCE_PRICE_PER_MILLION).min(1.0),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn defaults_match_feedback() {
		let policy = ModelPolicy::default_policy();
		let deepseek = policy
			.models
			.iter()
			.find(|entry| entry.model_id == DEFAULT_MODEL)
			.expect("deepseek entry present");
		assert_eq!(deepseek.status, ModelStatus::Recommended);
		assert_eq!(deepseek.rank, 10);
		assert!(deepseek.default);

		let glm = policy
			.models
			.iter()
			.find(|entry| entry.model_id == EXPERIMENTAL_MODEL)
			.expect("glm entry present");
		assert_eq!(glm.status, ModelStatus::Experimental);
		assert_eq!(glm.rank, 900);
		assert!(!glm.default);
		assert_eq!(glm.reason, "Observed delivery failures; requires passing conformance suite.");

		let guard = policy
			.models
			.iter()
			.find(|entry| entry.model_id == "@cf/meta/llama-guard-3-8b")
			.expect("guard entry present");
		assert_eq!(guard.status, ModelStatus::Hidden);
		assert!(!guard.primary_agent_eligible);
		assert_eq!(guard.reason, "Safety classifier.");
	}

	#[test]
	fn status_for_unknown_is_available() {
		let policy = ModelPolicy::default_policy();
		assert_eq!(policy.status_for("@cf/some-org/unknown-model"), ModelStatus::Available);
		assert_eq!(policy.status_for(DEFAULT_MODEL), ModelStatus::Recommended);
		assert_eq!(policy.status_for(EXPERIMENTAL_MODEL), ModelStatus::Experimental);
		assert_eq!(policy.status_for("@cf/meta/llama-guard-3-8b"), ModelStatus::Hidden);
	}

	#[test]
	fn primary_eligibility_follows_entries() {
		let policy = ModelPolicy::default_policy();
		assert!(policy.is_primary_agent_eligible(DEFAULT_MODEL));
		assert!(policy.is_primary_agent_eligible("@cf/openai/gpt-oss-120b"));
		assert!(policy.is_primary_agent_eligible("@cf/unknown/not-in-policy"));
		assert!(!policy.is_primary_agent_eligible("@cf/meta/llama-guard-3-8b"));
	}

	#[test]
	fn default_model_is_deepseek() {
		let policy = ModelPolicy::default_policy();
		assert_eq!(policy.default_model(), Some(DEFAULT_MODEL));
	}

	#[test]
	fn policy_roundtrips_serde() {
		let policy = ModelPolicy::default_policy();
		let json = serde_json::to_string(&policy).expect("serializes");
		let back: ModelPolicy = serde_json::from_str(&json).expect("deserializes");
		assert_eq!(policy, back);
		let value: serde_json::Value = serde_json::from_str(&json).expect("parses");
		assert_eq!(value["version"], "1");
		assert_eq!(value["models"][0]["status"], "recommended");
		assert_eq!(value["models"][5]["status"], "experimental");
		assert_eq!(value["models"][6]["status"], "hidden");
	}
}

#[cfg(test)]
mod ranking_tests {
	use super::*;
	use crate::health::{CONFORMANCE_SUITE_VERSION, VerificationConfidence};

	/// Build a normalized catalog record with the given id, context window and
	/// per-million prices (through the real OpenRouter normalization path).
	fn record(id: &str, context: u64, input: f64, output: f64) -> ModelRecord {
		let entry = serde_json::json!({
			"id": id,
			"name": id,
			"context_length": context,
			"created": 1_788_800_000,
			"pricing": {
				"prompt": format!("{:.8}", input / 1_000_000.0),
				"completion": format!("{:.8}", output / 1_000_000.0),
			},
		});
		ModelRecord::from_openrouter(&entry).expect("record normalizes")
	}

	/// Build a verification record with the given status, run counts,
	/// multi-turn tool-loop rate, and median latency.
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
			runner_version: "test".to_string(),
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
		let model = record(DEFAULT_MODEL, 1_310_720, 0.15, 0.5);
		assert_eq!(ranking_score(&model, None), None);
		let breakdown = RankingBreakdown::breakdown_for(&model, None);
		assert_eq!(breakdown.score, None);
		assert_eq!(breakdown.model_id, DEFAULT_MODEL);
	}

	#[test]
	fn non_passing_verification_never_ranks() {
		let model = record(DEFAULT_MODEL, 1_310_720, 0.15, 0.5);
		for status in [
			VerificationStatus::Degraded,
			VerificationStatus::Failing,
			VerificationStatus::Untested,
			VerificationStatus::Expired,
		] {
			let verification = verification(status, 100, 99, Some(0.96), Some(2_000));
			assert_eq!(ranking_score(&model, Some(&verification)), None, "{status:?} must not rank");
		}
	}

	#[test]
	fn non_primary_agent_models_never_rank() {
		// Llama Guard is hidden and never primary-agent eligible - even with
		// a passing verification record it must not rank.
		let model = record("@cf/meta/llama-guard-3-8b", 131_072, 0.05, 0.05);
		let verification = verification(VerificationStatus::Passing, 100, 100, Some(0.99), Some(1_000));
		assert_eq!(ranking_score(&model, Some(&verification)), None);
	}

	#[test]
	fn passing_deepseek_like_outscores_passing_glm_like_on_tool_loop() {
		let deepseek = record(DEFAULT_MODEL, 1_310_720, 0.15, 0.5);
		let glm = record(EXPERIMENTAL_MODEL, 1_310_720, 0.15, 0.5);
		let ds_verification = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(2_000));
		let glm_verification = verification(VerificationStatus::Passing, 100, 99, Some(0.70), Some(2_000));
		let ds_score = ranking_score(&deepseek, Some(&ds_verification)).expect("deepseek ranks");
		let glm_score = ranking_score(&glm, Some(&glm_verification)).expect("glm ranks");
		assert!(ds_score > glm_score, "deepseek {ds_score} must beat glm {glm_score}");
		// Everything else is identical, so the whole gap is the 0.25-weighted
		// tool-loop component (0.96 vs 0.70).
		let gap = ds_score - glm_score;
		assert!((gap - WEIGHT_TOOL_LOOP * 0.26).abs() < 1e-9, "gap {gap}");
	}

	#[test]
	fn breakdown_weights_sum_to_one() {
		let model = record(DEFAULT_MODEL, 1_310_720, 0.15, 0.5);
		let verification = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(2_000));
		let breakdown = RankingBreakdown::breakdown_for(&model, Some(&verification));
		let sum = breakdown.weight_delivery
			+ breakdown.weight_tool_loop
			+ breakdown.weight_context
			+ breakdown.weight_latency
			+ breakdown.weight_price;
		assert!((sum - 1.0).abs() < 1e-12, "weights must sum to 1.0, got {sum}");
		assert_eq!(breakdown.score, ranking_score(&model, Some(&verification)));
		// The score IS the weighted dot product of the components.
		let dot = WEIGHT_DELIVERY * breakdown.delivery
			+ WEIGHT_TOOL_LOOP * breakdown.tool_loop
			+ WEIGHT_CONTEXT * breakdown.context
			+ WEIGHT_LATENCY * breakdown.latency
			+ WEIGHT_PRICE * breakdown.price;
		assert!((dot - breakdown.score.expect("score present")).abs() < 1e-9);
	}

	#[test]
	fn bigger_context_increases_score_other_components_held() {
		let small = record(DEFAULT_MODEL, 262_144, 0.15, 0.5);
		let big = record(DEFAULT_MODEL, 1_310_720, 0.15, 0.5);
		let verification = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(2_000));
		let small_score = ranking_score(&small, Some(&verification)).expect("small ranks");
		let big_score = ranking_score(&big, Some(&verification)).expect("big ranks");
		assert!(big_score > small_score);
		// The context score caps at 1.0: a 2M window equals the reference.
		let capped = record(DEFAULT_MODEL, 2_000_000, 0.15, 0.5);
		let capped_score = ranking_score(&capped, Some(&verification)).expect("capped ranks");
		assert!((capped_score - big_score).abs() < 1e-9);
	}

	#[test]
	fn lower_latency_increases_score_other_components_held() {
		let model = record(DEFAULT_MODEL, 1_310_720, 0.15, 0.5);
		let fast = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(2_000));
		let slow = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(30_000));
		let fast_score = ranking_score(&model, Some(&fast)).expect("fast ranks");
		let slow_score = ranking_score(&model, Some(&slow)).expect("slow ranks");
		assert!(fast_score > slow_score);
		// Missing latency contributes 0.0 (2s → 0.9667, 30s → 0.5, none → 0.0).
		let missing = verification(VerificationStatus::Passing, 100, 99, Some(0.96), None);
		let missing_score = ranking_score(&model, Some(&missing)).expect("missing-latency ranks");
		assert!((missing_score - (slow_score - WEIGHT_LATENCY * 0.5)).abs() < 1e-9);
		// Median latency at or beyond 60s scores 0.0.
		let maxed = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(61_000));
		let maxed_score = ranking_score(&model, Some(&maxed)).expect("maxed-latency ranks");
		assert!((maxed_score - (slow_score - WEIGHT_LATENCY * 0.5)).abs() < 1e-9);
	}

	#[test]
	fn lower_price_increases_score_other_components_held() {
		let cheap = record(DEFAULT_MODEL, 1_310_720, 0.15, 0.5);
		let expensive = record(DEFAULT_MODEL, 1_310_720, 10.0, 12.0);
		let verification = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(2_000));
		let cheap_score = ranking_score(&cheap, Some(&verification)).expect("cheap ranks");
		let expensive_score = ranking_score(&expensive, Some(&verification)).expect("expensive ranks");
		assert!(cheap_score > expensive_score);
		// Missing price data scores 0.0 - unknown price is never free.
		let no_price = ModelRecord {
			pricing: crate::catalog::PricingPerMillion { input: None, cached_input: None, output: None },
			..cheap.clone()
		};
		let no_price_score = ranking_score(&no_price, Some(&verification)).expect("no-price ranks");
		assert!(no_price_score < cheap_score);
		let cheap_breakdown = RankingBreakdown::breakdown_for(&cheap, Some(&verification));
		let no_price_breakdown = RankingBreakdown::breakdown_for(&no_price, Some(&verification));
		assert!(cheap_breakdown.price > no_price_breakdown.price);
		assert_eq!(no_price_breakdown.price, 0.0);
	}

	#[test]
	fn degraded_but_cheaper_never_ranks() {
		// A very cheap, high-context model still scores None while Degraded.
		let model = record(DEFAULT_MODEL, 1_310_720, 0.05, 0.10);
		let verification = verification(VerificationStatus::Degraded, 100, 60, Some(0.50), Some(1_000));
		assert_eq!(ranking_score(&model, Some(&verification)), None);
	}

	#[test]
	fn breakdown_serde_is_snake_case() {
		let model = record(DEFAULT_MODEL, 1_310_720, 0.15, 0.5);
		let verification = verification(VerificationStatus::Passing, 100, 99, Some(0.96), Some(2_000));
		let breakdown = RankingBreakdown::breakdown_for(&model, Some(&verification));
		let json = serde_json::to_string(&breakdown).expect("ser");
		// The JSON text carries the snake_case contract.
		let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
		for key in [
			"model_id",
			"score",
			"delivery",
			"tool_loop",
			"context",
			"latency",
			"price",
			"weight_delivery",
			"weight_tool_loop",
			"weight_context",
			"weight_latency",
			"weight_price",
		] {
			assert!(value.get(key).is_some(), "missing snake_case key {key}");
		}
		// f64 values lose ~1 ulp through the JSON text roundtrip (serde_json
		// arbitrary_precision), so compare floats with tolerance.
		let back: RankingBreakdown = serde_json::from_str(&json).expect("de");
		assert_eq!(back.model_id, breakdown.model_id);
		assert!(close_f64(back.score, breakdown.score), "score {back:?} vs {breakdown:?}");
		for (actual, expected) in [
			(back.delivery, breakdown.delivery),
			(back.tool_loop, breakdown.tool_loop),
			(back.context, breakdown.context),
			(back.latency, breakdown.latency),
			(back.price, breakdown.price),
			(back.weight_delivery, breakdown.weight_delivery),
			(back.weight_tool_loop, breakdown.weight_tool_loop),
			(back.weight_context, breakdown.weight_context),
			(back.weight_latency, breakdown.weight_latency),
			(back.weight_price, breakdown.weight_price),
		] {
			assert!((actual - expected).abs() < 1e-12, "{actual} vs {expected}");
		}
	}

	fn close_f64(actual: Option<f64>, expected: Option<f64>) -> bool {
		match (actual, expected) {
			(Some(actual), Some(expected)) => (actual - expected).abs() < 1e-12,
			(None, None) => true,
			_ => false,
		}
	}
}
