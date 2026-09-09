//! Policy - status, rank, and primary-agent eligibility for known models.
//!
//! This module owns the bundled policy record (feedback 01/02/05/06): the
//! status, rank, default flag, and primary-agent eligibility of every model
//! the project has an explicit opinion about. Unknown models default to
//! `Available`; the policy never claims a model is ineligible unless a
//! policy entry says so. The JSON contract serializes as snake_case so the
//! CLI and the Hermes plugin can consume it without transformation.

use serde::{Deserialize, Serialize};

use crate::catalog::ModelRole;
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
