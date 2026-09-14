//! Catalog - Workers AI model records and OpenRouter-format payload
//! normalization.
//!
//! This module owns the typed catalog contract consumed by the CLI
//! (`auth-cloudflare catalog get --format json`), the Hermes plugin, and the
//! conformance harness. The three-state `CapabilityState` deliberately
//! distinguishes *unknown* from *unsupported*: the OpenRouter-format catalog
//! omits `supported_parameters` for models whose docs confirm function
//! calling, and incomplete metadata must never be encoded as `Unsupported`.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Curated fallback catalog - the account-verified 27 Cloudflare-hosted Workers AI chat
/// models, filtered to a practical coding/tool set (minus `llama-guard-3-8b`).
///
/// ORDER IS POLICY: DeepSeek V4 Flash is the development default;
/// GLM-5.3 Flash is experimental and intentionally NOT in the default
/// position (delivery reliability not yet validated).
pub const FALLBACK_MODELS: &[&str] = &[
	"@cf/deepseek-ai/deepseek-v4-flash-0731",
	"@cf/moonshotai/kimi-k2.7-code",
	"@cf/deepseek-ai/deepseek-v4-pro-0813",
	"@cf/openai/gpt-oss-120b",
	"@cf/openai/gpt-oss-20b",
	"@cf/zai-org/glm-5.3",
	"@cf/qwen/qwen3.8-27b",
	"@cf/qwen/qwen3-30b-a3b-fp8",
	"@cf/qwen/qwen2.5-coder-32b-instruct",
	"@cf/meta/llama-4-scout-17b-16e-instruct",
	"@cf/meta/llama-3.3-70b-instruct-fp8-fast",
	"@cf/mistralai/mistral-small-3.1-24b-instruct",
	"@cf/nvidia/nemotron-3-120b-a12b",
	"@cf/ibm-granite/granite-4.0-h-micro",
	"@cf/zai-org/glm-4.7-flash",
	"@cf/moonshotai/kimi-k2.6",
	"@cf/deepseek-ai/deepseek-r1-distill-qwen-32b",
	"@cf/meta/llama-3.1-8b-instruct-fp8",
	"@cf/meta/llama-3.2-1b-instruct",
	"@cf/meta/llama-3.2-3b-instruct",
	"@cf/meta/llama-3.2-11b-vision-instruct",
	"@cf/qwen/qwq-32b",
];

/// Models currently marked experimental by project policy.
/// Delivery conformance below threshold - selectable, never the default.
pub const EXPERIMENTAL_MODELS: &[&str] = &["@cf/zai-org/glm-5.3-flash", "@cf/zai-org/glm-5.3"];

/// Models hidden from the primary agent picker (safety/classification).
pub const HIDDEN_MODELS: &[&str] = &["@cf/meta/llama-guard-3-8b"];

/// Versioned snapshot of the whole catalog - the JSON contract envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CatalogSnapshot {
	pub schema_version: u32,
	pub fetched_at: DateTime<Utc>,
	pub source: CatalogSource,
	pub account_fingerprint: String,
	pub filters: CatalogFilters,
	pub models: Vec<ModelRecord>,
}

/// Where the snapshot data came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
	/// Fresh data from Cloudflare's `/ai/models/search` endpoint.
	Live,
	/// Served from the local atomic cache (stale fallback).
	Cache,
	/// Bundled static fallback list (no network, no cache).
	Fallback,
}

/// Filters applied when the snapshot was produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CatalogFilters {
	pub experimental_included: bool,
	pub deprecated_included: bool,
}

/// Role of a Workers AI model in the provider catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
	/// Suitable as the primary coding/tool agent model.
	CodingAgent,
	/// General chat/reasoning, weaker for tool loops.
	GeneralChat,
	/// Reasoning-heavy model.
	Reasoning,
	/// Vision-capable multimodal model.
	Vision,
	/// Safety/classification model - never a primary agent model.
	Safety,
	/// Non-chat modality (embedding, image, audio, video…).
	UnsupportedPrimaryAgent,
}

/// Availability of a model for this account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
	/// Cloudflare says this account can invoke it.
	CloudflareHosted,
	/// Third-party/gateway model routed through the same chat surface.
	Routed,
}

/// Picker visibility policy for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
	Recommended,
	Available,
	Experimental,
	Degraded,
	Blocked,
	Deprecated,
	Hidden,
}

/// Capability verdict for one model feature.
///
/// `Unknown` is a real state, not a failure: the OpenRouter-format catalog
/// omits `supported_parameters` for models whose docs confirm function
/// calling. Never encode incomplete catalog metadata as `Unsupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
	Confirmed,
	Unsupported,
	Unknown,
}

/// Capability matrix for a model - the picker's tool/vision/reasoning view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelCapabilities {
	pub chat: CapabilityState,
	pub tools: CapabilityState,
	pub parallel_tools: CapabilityState,
	pub structured_output: CapabilityState,
	pub reasoning: CapabilityState,
	pub vision_input: CapabilityState,
	pub streaming: CapabilityState,
}

impl Default for ModelCapabilities {
	fn default() -> Self {
		Self {
			chat: CapabilityState::Confirmed,
			tools: CapabilityState::Unknown,
			parallel_tools: CapabilityState::Unknown,
			structured_output: CapabilityState::Unknown,
			reasoning: CapabilityState::Unknown,
			vision_input: CapabilityState::Unknown,
			streaming: CapabilityState::Unknown,
		}
	}
}

/// Per-million-token pricing (normalized to USD).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PricingPerMillion {
	pub input: Option<f64>,
	pub cached_input: Option<f64>,
	pub output: Option<f64>,
}

/// Context/output limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelLimits {
	pub context_tokens: Option<u64>,
	pub max_output_tokens: Option<u64>,
}

/// Protocol facts for one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelProtocol {
	pub api_mode: String,
	pub base_url: String,
	pub request_path: String,
}

/// Where each capability/field fact came from (provenance).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CapabilityProvenance {
	pub pricing: String,
	pub context_tokens: String,
	pub tools: String,
	pub reasoning: String,
}

/// Normalized Workers AI model record - one source of truth for the
/// picker, the fallback list, and generated YAML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
	pub id: String,
	pub display_name: String,
	pub publisher: String,
	pub role: ModelRole,
	pub availability: Availability,
	pub visibility: Visibility,
	pub protocol: ModelProtocol,
	pub pricing: PricingPerMillion,
	pub limits: ModelLimits,
	pub capabilities: ModelCapabilities,
	pub provenance: CapabilityProvenance,
	pub documentation_url: Option<String>,
	pub catalog_added_at: Option<NaiveDate>,
	#[serde(skip)]
	pub raw: serde_json::Value,
}

/// Publishers/models the Cloudflare docs confirm for function calling.
const TOOL_CAPABLE_MARKERS: &[&str] = &[
	"deepseek-v4-flash",
	"deepseek-v4-pro",
	"kimi-k2.7-code",
	"kimi-k2.6",
	"gpt-oss",
	"glm-5.3-flash",
	"glm-5.3",
	"glm-5.2-flash",
	"glm-5.2",
	"qwen3-30b-a3b-fp8",
	"qwen3.8-27b",
	"nemotron-3-120b",
	"mistral-small-3.1-24b",
	"llama-3.3-70b-instruct-fp8-fast",
	"llama-4-scout-17b-16e-instruct",
	"granite-4.0-h-micro",
];

/// Safety/classification models - filtered from the primary picker.
const SAFETY_MARKERS: &[&str] = &["llama-guard-3-8b"];

/// Reasoning-heavy models (DeepSeek-R1/QwQ families).
const REASONING_MARKERS: &[&str] = &["r1", "qwq"];

/// Non-chat model families - never selectable through `/chat/completions`.
const NON_CHAT_MARKERS: &[&str] = &[
	"embedding",
	"rerank",
	"flux",
	"wan",
	"eleven",
	"universal-",
	"whisper",
	"tts",
	"stable-",
	"sdxl",
	"qwen-image",
	"seedance",
	"ltx-",
	"grok-imagine",
	"qwen3-vl",
	"kimi-k3",
];

impl ModelRecord {
	/// Normalize one OpenRouter-format catalog entry.
	pub fn from_openrouter(item: &serde_json::Value) -> Option<Self> {
		let id = item.get("id")?.as_str()?.to_string();
		if id.is_empty() {
			return None;
		}
		let lower = id.to_lowercase();

		let role = if SAFETY_MARKERS.iter().any(|m| lower.contains(m)) {
			ModelRole::Safety
		} else if REASONING_MARKERS.iter().any(|m| lower.contains(m)) {
			ModelRole::Reasoning
		} else if NON_CHAT_MARKERS.iter().any(|m| lower.contains(m)) {
			ModelRole::UnsupportedPrimaryAgent
		} else {
			ModelRole::CodingAgent
		};

		let capabilities = ModelCapabilities {
			tools: if role == ModelRole::Safety {
				CapabilityState::Unsupported
			} else if TOOL_CAPABLE_MARKERS.iter().any(|m| lower.contains(m)) {
				CapabilityState::Confirmed
			} else {
				CapabilityState::Unknown
			},
			..ModelCapabilities::default()
		};

		let pricing = item.get("pricing").and_then(PricingPerMillion::parse);

		let publisher = id
			.strip_prefix("@cf/")
			.and_then(|rest| rest.split('/').next())
			.unwrap_or("cloudflare")
			.to_string();

		let role_based_visibility = if SAFETY_MARKERS.iter().any(|m| lower.contains(m)) {
			Visibility::Hidden
		} else if EXPERIMENTAL_MODELS.contains(&id.as_str()) {
			Visibility::Experimental
		} else if FALLBACK_MODELS.first().is_some_and(|m| *m == id.as_str()) {
			Visibility::Recommended
		} else {
			Visibility::Available
		};

		// Derive the docs URL from the last `@cf/<org>/<model>` path segment;
		// non-`@cf/` (routed) and malformed ids yield `None` gracefully.
		let documentation_url = id
			.strip_prefix("@cf/")
			.and_then(|rest| rest.split('/').next_back())
			.filter(|segment| !segment.is_empty())
			.map(|segment| format!("https://developers.cloudflare.com/workers-ai/models/{segment}/"));

		Some(Self {
			display_name: item.get("name").and_then(|n| n.as_str()).unwrap_or(&id).to_string(),
			publisher,
			id: id.clone(),
			role,
			availability: if id.starts_with("@cf/") {
				Availability::CloudflareHosted
			} else {
				Availability::Routed
			},
			visibility: role_based_visibility,
			protocol: ModelProtocol {
				api_mode: "chat_completions".to_string(),
				base_url: String::new(), // filled by the CLI from account credentials
				request_path: "/chat/completions".to_string(),
			},
			capabilities,
			pricing: pricing.unwrap_or(PricingPerMillion { input: None, cached_input: None, output: None }),
			limits: ModelLimits {
				context_tokens: item.get("context_length").and_then(|c| c.as_u64()),
				max_output_tokens: None,
			},
			provenance: CapabilityProvenance {
				pricing: "cloudflare_catalog_api".to_string(),
				context_tokens: "cloudflare_catalog_api".to_string(),
				tools: "cloudflare_model_docs_or_schema".to_string(),
				reasoning: "cloudflare_model_docs_or_schema".to_string(),
			},
			catalog_added_at: item.get("created").and_then(|c| c.as_i64()).and_then(Self::date_from_epoch),
			documentation_url,
			raw: item.clone(),
		})
	}

	/// True when the model may appear in the primary coding picker.
	pub fn eligible_for_primary_picker(&self) -> bool {
		self.role != ModelRole::Safety
			&& self.role != ModelRole::UnsupportedPrimaryAgent
			&& self.visibility != Visibility::Hidden
			&& self.id.starts_with("@cf/")
	}

	/// True when the model is the project's development default.
	pub fn is_default(&self) -> bool {
		self.id == FALLBACK_MODELS[0]
	}

	/// Epoch seconds → date (catalog added date).
	fn date_from_epoch(epoch: i64) -> Option<NaiveDate> {
		let days = epoch.div_euclid(86_400);
		let z = days + 719_468;
		let era = z.div_euclid(146_097);
		let doe = z.rem_euclid(146_097);
		let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
		let year = yoe + era * 400;
		let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
		let mp = (5 * doy + 2) / 153;
		let day = doy - (153 * mp + 2) / 5 + 1;
		let month = if mp < 10 { mp + 3 } else { mp - 9 };
		let year = if month <= 2 { year + 1 } else { year };
		NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32)
	}
}

impl PricingPerMillion {
	/// Deserialize per-token string/number pricing → per-million floats.
	pub fn parse(value: &serde_json::Value) -> Option<Self> {
		let as_f64 = |key: &str| -> Option<f64> {
			match value.get(key) {
				Some(serde_json::Value::Number(n)) => n.as_f64(),
				Some(serde_json::Value::String(s)) => s.trim().parse::<f64>().ok(),
				_ => None,
			}
		};
		let per_token = as_f64("prompt").or_else(|| as_f64("input"));
		let output = as_f64("completion").or_else(|| as_f64("output"));
		let cached = as_f64("cached_input");
		if per_token.is_none() && output.is_none() {
			return None;
		}
		Some(Self {
			input: per_token.map(|v| v * 1_000_000.0),
			cached_input: cached.map(|v| v * 1_000_000.0),
			output: output.map(|v| v * 1_000_000.0),
		})
	}
}

/// Normalize a full OpenRouter-format payload → picker model list.
pub fn picker_models_from_openrouter(payload: &serde_json::Value) -> Vec<String> {
	let Some(items) = payload.get("data").and_then(|d| d.as_array()) else {
		return FALLBACK_MODELS.iter().map(|m| (*m).to_string()).collect();
	};
	let mut models: Vec<String> = items
		.iter()
		.filter_map(ModelRecord::from_openrouter)
		.filter(|record| record.eligible_for_primary_picker())
		.map(|record| record.id)
		.collect();
	if models.is_empty() {
		return FALLBACK_MODELS.iter().map(|m| (*m).to_string()).collect();
	}
	models.sort();
	models
}

#[cfg(test)]
mod tests {
	use super::*;

	fn catalog_entry(id: &str, context: u64, created: i64) -> serde_json::Value {
		serde_json::json!({
			"id": id,
			"name": id,
			"context_length": context,
			"created": created,
			"pricing": { "prompt": "0.00000015", "completion": "0.0000005" },
		})
	}

	#[test]
	fn normalizes_openrouter_entry() {
		let entry = catalog_entry("@cf/deepseek-ai/deepseek-v4-flash-0731", 1_310_720, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("deepseek entry normalizes");
		assert_eq!(record.publisher, "deepseek-ai");
		assert_eq!(record.role, ModelRole::CodingAgent);
		assert_eq!(record.limits.context_tokens, Some(1_310_720));
		assert_eq!(record.capabilities.tools, CapabilityState::Confirmed);
		assert!(record.eligible_for_primary_picker());
		assert!(record.is_default());
	}

	#[test]
	fn glm_flash_is_experimental() {
		let entry = catalog_entry("@cf/zai-org/glm-5.3-flash", 1_310_720, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("glm entry normalizes");
		assert_eq!(record.visibility, Visibility::Experimental);
		assert!(!record.is_default());
	}

	#[test]
	fn safety_model_is_hidden() {
		let entry = catalog_entry("@cf/meta/llama-guard-3-8b", 131_072, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("guard entry normalizes");
		assert_eq!(record.role, ModelRole::Safety);
		assert_eq!(record.visibility, Visibility::Hidden);
		assert_eq!(record.capabilities.tools, CapabilityState::Unsupported);
		assert!(!record.eligible_for_primary_picker());
	}

	#[test]
	fn unknown_capability_stays_unknown() {
		let entry = catalog_entry("@cf/some-org/new-model", 32_768, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("new-model normalizes");
		assert_eq!(record.capabilities.tools, CapabilityState::Unknown);
	}

	#[test]
	fn non_chat_family_is_excluded() {
		let entry = catalog_entry("@cf/baai/bge-m3-embedding", 8192, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("embedding normalizes");
		assert_eq!(record.role, ModelRole::UnsupportedPrimaryAgent);
		assert!(!record.eligible_for_primary_picker());
	}

	#[test]
	fn picker_normalizes_payload() {
		let payload = serde_json::json!({
			"data": [
				catalog_entry("@cf/deepseek-ai/deepseek-v4-flash-0731", 1_310_720, 1_788_800_000),
				catalog_entry("@cf/meta/llama-guard-3-8b", 131_072, 1_788_800_000),
				catalog_entry("@cf/baai/bge-m3-embedding", 8192, 1_788_800_000),
				serde_json::json!({ "id": "@cf/third-party/gemini-3.8-flash" }),
			],
		});
		let models = picker_models_from_openrouter(&payload);
		assert_eq!(
			models,
			vec!["@cf/deepseek-ai/deepseek-v4-flash-0731", "@cf/third-party/gemini-3.8-flash"]
		);
	}

	#[test]
	fn picker_falls_back_on_empty() {
		let models = picker_models_from_openrouter(&serde_json::json!({ "data": [] }));
		assert!(!models.is_empty());
		assert_eq!(models[0], "@cf/deepseek-ai/deepseek-v4-flash-0731");
	}

	#[test]
	fn pricing_per_token_rounds_to_million() {
		let entry = catalog_entry("@cf/deepseek-ai/deepseek-v4-flash-0731", 1_310_720, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("record");
		let pricing = record.pricing;
		assert_eq!(pricing.input, Some(0.15));
		assert_eq!(pricing.output, Some(0.5));
	}

	#[test]
	fn fallback_first_model_is_default() {
		assert_eq!(FALLBACK_MODELS[0], "@cf/deepseek-ai/deepseek-v4-flash-0731");
	}

	#[test]
	fn date_from_epoch_converts() {
		assert_eq!(ModelRecord::date_from_epoch(1_788_800_000), NaiveDate::from_ymd_opt(2026, 9, 7));
		assert_eq!(ModelRecord::date_from_epoch(0), NaiveDate::from_ymd_opt(1970, 1, 1));
	}

	#[test]
	fn documentation_url_derives_from_cf_id_segment() {
		let entry = catalog_entry("@cf/deepseek-ai/deepseek-v4-flash-0731", 1_310_720, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("normalizes");
		assert_eq!(
			record.documentation_url.as_deref(),
			Some("https://developers.cloudflare.com/workers-ai/models/deepseek-v4-flash-0731/")
		);

		// Routed (non-`@cf/`) ids carry no Workers AI docs URL.
		let routed = catalog_entry("deepseek/deepseek-chat", 128_000, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&routed).expect("normalizes");
		assert_eq!(record.documentation_url, None);
	}
}
