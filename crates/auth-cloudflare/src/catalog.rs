//! Catalog - Cloudflare Workers AI model records and OpenRouter-format
//! payload normalization.

use serde::{Deserialize, Serialize};

/// Curated fallback catalog - the account-verified 27 Workers AI chat models,
/// filtered to a practical coding/tool set (the same list the user's generated
/// YAML confirmed, minus `llama-guard-3-8b`).
pub const FALLBACK_MODELS: &[&str] = &[
	"@cf/zai-org/glm-5.3-flash",
	"@cf/moonshotai/kimi-k2.7-code",
	"@cf/deepseek-ai/deepseek-v4-flash-0731",
	"@cf/deepseek-ai/deepseek-v4-pro-0813",
	"@cf/zai-org/glm-5.3",
	"@cf/openai/gpt-oss-120b",
	"@cf/openai/gpt-oss-20b",
	"@cf/qwen/qwen3.8-27b",
	"@cf/qwen/qwen3-30b-a3b-fp8",
	"@cf/qwen/qwen2.5-coder-32b-instruct",
	"@cf/meta/llama-4-scout-17b-16e-instruct",
	"@cf/meta/llama-3.3-70b-instruct-fp8-fast",
	"@cf/mistralai/mistral-small-3.1-24b-instruct",
	"@cf/nvidia/nemotron-3-120b-a12b",
	"@cf/ibm-granite/granite-4.0-h-micro",
	"@cf/zai-org/glm-5.2",
	"@cf/zai-org/glm-4.7-flash",
	"@cf/moonshotai/kimi-k2.6",
	"@cf/deepseek-ai/deepseek-r1-distill-qwen-32b",
	"@cf/meta/llama-3.1-8b-instruct-fp8",
	"@cf/meta/llama-3.2-1b-instruct",
	"@cf/meta/llama-3.2-3b-instruct",
	"@cf/meta/llama-3.2-11b-vision-instruct",
	"@cf/qwen/qwq-32b",
	"@cf/zai-org/glm-5.2-flash",
];

/// Role of a Workers AI model in the provider catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
	/// Suitable as the primary coding/tool agent model.
	Coding,
	/// General chat/reasoning, weaker for tool loops.
	Chat,
	/// Vision-capable multimodal model.
	Vision,
	/// Safety/classification model - never a primary agent model.
	Safety,
	/// Non-chat modality (embedding, image, audio, video…).
	NonChat,
}

/// Capability verdict for one model feature.
///
/// `Unknown` is a real state, not a failure: the OpenRouter-format catalog
/// omits `supported_parameters` for models whose docs confirm function
/// calling. Capability unknown ≠ capability unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
	Yes,
	No,
	Unknown,
}

/// Normalized Workers AI model record - one source of truth for the picker,
/// the fallback list, and generated YAML.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
	pub id: String,
	pub name: String,
	pub publisher: String,
	pub role: ModelRole,
	pub context_length: Option<u64>,
	pub catalog_added_at: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pricing: Option<OpenRouterPricing>,
	pub function_calling: CapabilityState,
	#[serde(skip)]
	pub raw: serde_json::Value,
}

/// OpenRouter-compatible per-model pricing (per token, from the catalog).
///
/// Cloudflare sends numeric fields as JSON strings ("0.00000015"), so the
/// raw values are kept and converted on read.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpenRouterPricing {
	pub prompt: Option<f64>,
	pub completion: Option<f64>,
}

impl OpenRouterPricing {
	/// Deserialize pricing tolerant of string-or-number encodings.
	pub fn parse(value: &serde_json::Value) -> Option<Self> {
		let as_f64 = |key: &str| -> Option<f64> {
			match value.get(key) {
				Some(serde_json::Value::Number(n)) => n.as_f64(),
				Some(serde_json::Value::String(s)) => s.trim().parse::<f64>().ok(),
				_ => None,
			}
		};
		let prompt = as_f64("prompt");
		let completion = as_f64("completion");
		if prompt.is_none() && completion.is_none() {
			return None;
		}
		Some(Self { prompt, completion })
	}
}

/// Publishers/models the Workers AI docs confirm for function calling.
const TOOL_CAPABLE_MARKERS: &[&str] = &[
	"glm-5.3-flash",
	"glm-5.3",
	"glm-5.2-flash",
	"glm-5.2",
	"kimi-k2.7-code",
	"kimi-k2.6",
	"deepseek-v4-flash",
	"deepseek-v4-pro",
	"gpt-oss",
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
		} else if NON_CHAT_MARKERS.iter().any(|m| lower.contains(m)) {
			ModelRole::NonChat
		} else {
			ModelRole::Coding
		};

		let function_calling = if role == ModelRole::Safety {
			CapabilityState::No
		} else if TOOL_CAPABLE_MARKERS.iter().any(|m| lower.contains(m)) {
			CapabilityState::Yes
		} else {
			CapabilityState::Unknown
		};

		let pricing = item.get("pricing").and_then(OpenRouterPricing::parse);

		Some(Self {
			name: item.get("name").and_then(|n| n.as_str()).unwrap_or(&id).to_string(),
			publisher: id
				.strip_prefix("@cf/")
				.and_then(|rest| rest.split('/').next())
				.unwrap_or("cloudflare")
				.to_string(),
			id,
			role,
			function_calling,
			context_length: item.get("context_length").and_then(|c| c.as_u64()),
			catalog_added_at: item.get("created").and_then(|c| c.as_i64()).map(Self::iso_date),
			pricing,
			raw: item.clone(),
		})
	}

	/// True when the model may appear in the primary coding picker.
	pub fn eligible_for_primary_picker(&self) -> bool {
		self.role == ModelRole::Coding && self.id.starts_with("@cf/")
	}

	/// Epoch seconds → `YYYY-MM-DD` (catalog added date).
	fn iso_date(epoch: i64) -> String {
		// Civil-from-days (Howard Hinnant) - dependency-free date conversion.
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
		format!("{year:04}-{month:02}-{day:02}")
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
		let entry = catalog_entry("@cf/zai-org/glm-5.3-flash", 1_310_720, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("glm entry normalizes");
		assert_eq!(record.publisher, "zai-org");
		assert_eq!(record.role, ModelRole::Coding);
		assert_eq!(record.context_length, Some(1_310_720));
		assert_eq!(record.function_calling, CapabilityState::Yes);
		assert!(record.eligible_for_primary_picker());
	}

	#[test]
	fn safety_model_is_filtered() {
		let entry = catalog_entry("@cf/meta/llama-guard-3-8b", 131_072, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("guard entry normalizes");
		assert_eq!(record.role, ModelRole::Safety);
		assert_eq!(record.function_calling, CapabilityState::No);
		assert!(!record.eligible_for_primary_picker());
	}

	#[test]
	fn unknown_capability_stays_unknown() {
		let entry = catalog_entry("@cf/some-org/new-model", 32_768, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("new-model normalizes");
		assert_eq!(record.function_calling, CapabilityState::Unknown);
	}

	#[test]
	fn non_chat_family_is_excluded() {
		let entry = catalog_entry("@cf/baai/bge-m3-embedding", 8192, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("embedding normalizes");
		assert_eq!(record.role, ModelRole::NonChat);
		assert!(!record.eligible_for_primary_picker());
	}

	#[test]
	fn picker_normalizes_payload() {
		let payload = serde_json::json!({
			"data": [
				catalog_entry("@cf/zai-org/glm-5.3-flash", 1_310_720, 1_788_800_000),
				catalog_entry("@cf/meta/llama-guard-3-8b", 131_072, 1_788_800_000),
				catalog_entry("@cf/baai/bge-m3-embedding", 8192, 1_788_800_000),
				serde_json::json!({ "id": "@cf/third-party/gemini-3.8-flash" }),
			],
		});
		let models = picker_models_from_openrouter(&payload);
		// Third-party gateway models with an `@cf/` prefix are kept (they are
		// account-invocable through the same chat surface); safety + non-chat
		// are filtered.
		assert_eq!(models, vec!["@cf/third-party/gemini-3.8-flash", "@cf/zai-org/glm-5.3-flash"]);
	}

	#[test]
	fn picker_falls_back_on_empty() {
		let models = picker_models_from_openrouter(&serde_json::json!({ "data": [] }));
		assert!(!models.is_empty());
		assert_eq!(models[0], "@cf/zai-org/glm-5.3-flash");
	}

	#[test]
	fn picker_falls_back_on_missing_data() {
		let models = picker_models_from_openrouter(&serde_json::json!({}));
		assert_eq!(models[0], "@cf/zai-org/glm-5.3-flash");
	}

	#[test]
	fn iso_date_converts_epoch() {
		assert_eq!(ModelRecord::iso_date(1_788_800_000), "2026-09-07");
		assert_eq!(ModelRecord::iso_date(0), "1970-01-01");
	}

	#[test]
	fn pricing_per_token_rounds_to_million() {
		let entry = catalog_entry("@cf/zai-org/glm-5.3-flash", 1_310_720, 1_788_800_000);
		let record = ModelRecord::from_openrouter(&entry).expect("record");
		let pricing = record.pricing.expect("pricing");
		assert_eq!(pricing.prompt, Some(0.000_000_15));
		assert_eq!(pricing.completion, Some(0.000_000_5));
	}

	#[test]
	fn fallback_first_model_is_default() {
		assert_eq!(FALLBACK_MODELS[0], "@cf/zai-org/glm-5.3-flash");
	}
}
