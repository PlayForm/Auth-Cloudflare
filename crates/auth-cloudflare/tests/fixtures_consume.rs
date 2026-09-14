//! Integration tests: the bundled OpenRouter-format model fixtures must
//! normalize through the PUBLIC `ModelRecord::from_openrouter` path exactly
//! as the real `/ai/models/search` entries do.
//!
//! Zero network: every fixture is embedded with `include_str!` and parsed
//! in memory. The six fixtures exercise the policy statuses the picker
//! depends on: recommended (DeepSeek V4 Flash/Pro, Kimi K2.7 Code),
//! available (GPT-OSS 120B), experimental (GLM-5.3 Flash), hidden (Llama
//! Guard 3 8B).

use auth_cloudflare::catalog::{CapabilityState, ModelRecord, ModelRole, Visibility};
use auth_cloudflare::policy::{ModelPolicy, ModelStatus};

const DEEPSEEK: &str = include_str!("../fixtures/models/deepseek-v4-flash-0731.json");
const GLM: &str = include_str!("../fixtures/models/glm-5.3-flash.json");
const LLAMA_GUARD: &str = include_str!("../fixtures/models/llama-guard-3-8b.json");
const DEEPSEEK_PRO: &str = include_str!("../fixtures/models/deepseek-v4-pro-0813.json");
const KIMI: &str = include_str!("../fixtures/models/kimi-k2.7-code.json");
const GPT_OSS: &str = include_str!("../fixtures/models/gpt-oss-120b.json");

fn normalize(fixture: &str) -> ModelRecord {
	let value: serde_json::Value = serde_json::from_str(fixture).expect("fixture is valid JSON");
	ModelRecord::from_openrouter(&value).expect("fixture normalizes through from_openrouter")
}

#[test]
fn deepseek_fixture_normalizes_to_recommended_default() {
	let record = normalize(DEEPSEEK);
	assert_eq!(record.id, "@cf/deepseek-ai/deepseek-v4-flash-0731");
	assert_eq!(record.display_name, "DeepSeek V4 Flash (070731)");
	assert_eq!(record.publisher, "deepseek-ai");
	assert_eq!(record.role, ModelRole::CodingAgent);
	assert_eq!(record.visibility, Visibility::Recommended);
	assert_eq!(record.capabilities.tools, CapabilityState::Confirmed);
	assert_eq!(record.limits.context_tokens, Some(1_310_720));
	assert!(record.eligible_for_primary_picker());
	assert!(record.is_default());

	// Pricing normalizes per-million from the OpenRouter per-token strings.
	let input = record.pricing.input.expect("input price present");
	let output = record.pricing.output.expect("output price present");
	assert!((input - 0.44).abs() < 1e-9, "input price {input} != 0.44");
	assert!((output - 1.32).abs() < 1e-9, "output price {output} != 1.32");

	let policy = ModelPolicy::default_policy();
	assert_eq!(policy.status_for(&record.id), ModelStatus::Recommended);
	assert!(policy.is_primary_agent_eligible(&record.id));
}

#[test]
fn glm_fixture_normalizes_to_experimental() {
	let record = normalize(GLM);
	assert_eq!(record.id, "@cf/zai-org/glm-5.3-flash");
	assert_eq!(record.publisher, "zai-org");
	assert_eq!(record.role, ModelRole::CodingAgent);
	assert_eq!(record.visibility, Visibility::Experimental);
	assert_eq!(record.capabilities.tools, CapabilityState::Confirmed);
	assert!(record.eligible_for_primary_picker());
	assert!(!record.is_default());

	let policy = ModelPolicy::default_policy();
	assert_eq!(policy.status_for(&record.id), ModelStatus::Experimental);
	assert!(policy.is_primary_agent_eligible(&record.id));
}

#[test]
fn llama_guard_fixture_normalizes_to_hidden_safety() {
	let record = normalize(LLAMA_GUARD);
	assert_eq!(record.id, "@cf/meta/llama-guard-3-8b");
	assert_eq!(record.publisher, "meta");
	assert_eq!(record.role, ModelRole::Safety);
	assert_eq!(record.visibility, Visibility::Hidden);
	assert_eq!(record.capabilities.tools, CapabilityState::Unsupported);
	assert!(!record.eligible_for_primary_picker());

	let policy = ModelPolicy::default_policy();
	assert_eq!(policy.status_for(&record.id), ModelStatus::Hidden);
	assert!(!policy.is_primary_agent_eligible(&record.id));
}

#[test]
fn deepseek_pro_fixture_normalizes_to_recommended() {
	let record = normalize(DEEPSEEK_PRO);
	assert_eq!(record.id, "@cf/deepseek-ai/deepseek-v4-pro-0813");
	assert_eq!(record.publisher, "deepseek-ai");
	assert_eq!(record.role, ModelRole::CodingAgent);
	assert_eq!(record.visibility, Visibility::Available);
	assert_eq!(record.capabilities.tools, CapabilityState::Confirmed);
	assert_eq!(record.limits.context_tokens, Some(1_048_576));
	assert!(record.eligible_for_primary_picker());
	assert!(!record.is_default());

	let input = record.pricing.input.expect("input price present");
	let output = record.pricing.output.expect("output price present");
	assert!((input - 1.1).abs() < 1e-9, "input price {input} != 1.1");
	assert!((output - 4.4).abs() < 1e-9, "output price {output} != 4.4");

	let policy = ModelPolicy::default_policy();
	assert_eq!(policy.status_for(&record.id), ModelStatus::Recommended);
	assert!(policy.is_primary_agent_eligible(&record.id));
}

#[test]
fn kimi_fixture_normalizes_to_recommended() {
	let record = normalize(KIMI);
	assert_eq!(record.id, "@cf/moonshotai/kimi-k2.7-code");
	assert_eq!(record.publisher, "moonshotai");
	assert_eq!(record.role, ModelRole::CodingAgent);
	assert_eq!(record.visibility, Visibility::Available);
	assert_eq!(record.capabilities.tools, CapabilityState::Confirmed);
	assert_eq!(record.limits.context_tokens, Some(262_144));
	assert!(record.eligible_for_primary_picker());
	assert!(!record.is_default());

	let input = record.pricing.input.expect("input price present");
	let output = record.pricing.output.expect("output price present");
	assert!((input - 0.6).abs() < 1e-9, "input price {input} != 0.6");
	assert!((output - 2.4).abs() < 1e-9, "output price {output} != 2.4");

	let policy = ModelPolicy::default_policy();
	assert_eq!(policy.status_for(&record.id), ModelStatus::Recommended);
	assert!(policy.is_primary_agent_eligible(&record.id));
}

#[test]
fn gpt_oss_fixture_normalizes_to_available() {
	let record = normalize(GPT_OSS);
	assert_eq!(record.id, "@cf/openai/gpt-oss-120b");
	assert_eq!(record.publisher, "openai");
	assert_eq!(record.role, ModelRole::CodingAgent);
	assert_eq!(record.visibility, Visibility::Available);
	assert_eq!(record.capabilities.tools, CapabilityState::Confirmed);
	assert_eq!(record.limits.context_tokens, Some(128_000));
	assert!(record.eligible_for_primary_picker());
	assert!(!record.is_default());

	let input = record.pricing.input.expect("input price present");
	let output = record.pricing.output.expect("output price present");
	assert!((input - 0.6).abs() < 1e-9, "input price {input} != 0.6");
	assert!((output - 1.8).abs() < 1e-9, "output price {output} != 1.8");

	let policy = ModelPolicy::default_policy();
	assert_eq!(policy.status_for(&record.id), ModelStatus::Available);
	assert!(policy.is_primary_agent_eligible(&record.id));
}

#[test]
fn the_three_fixtures_exercise_the_three_policy_statuses() {
	let policy = ModelPolicy::default_policy();
	let deepseek = policy.status_for("@cf/deepseek-ai/deepseek-v4-flash-0731");
	let glm = policy.status_for("@cf/zai-org/glm-5.3-flash");
	let guard = policy.status_for("@cf/meta/llama-guard-3-8b");
	assert_eq!(deepseek, ModelStatus::Recommended);
	assert_eq!(glm, ModelStatus::Experimental);
	assert_eq!(guard, ModelStatus::Hidden);
	// The three statuses are pairwise distinct - the fixtures really do
	// cover the recommended/experimental/hidden spectrum.
	assert_ne!(deepseek, glm);
	assert_ne!(glm, guard);
	assert_ne!(deepseek, guard);
}

#[test]
fn every_fixture_is_cloudflare_hosted_and_chat_shaped() {
	for fixture in [DEEPSEEK, GLM, LLAMA_GUARD, DEEPSEEK_PRO, KIMI, GPT_OSS] {
		let record = normalize(fixture);
		assert_eq!(record.protocol.api_mode, "chat_completions");
		assert_eq!(record.protocol.request_path, "/chat/completions");
		assert_eq!(record.provenance.pricing, "cloudflare_catalog_api");
		// Every `@cf/` fixture now derives a Workers AI docs URL.
		assert!(record.documentation_url.is_some(), "{} should derive a docs URL", record.id);
	}
}
