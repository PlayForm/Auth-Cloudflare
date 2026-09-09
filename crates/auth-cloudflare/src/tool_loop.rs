//! Tool loop - deterministic multi-turn fake-tool conformance harness
//! (feedback 02, suite `40-multi-turn-tool-loop`).
//!
//! This is the most important Hermes test: a LIVE model must complete a
//! three-step fixture workflow through OpenAI-format tool calls backed by
//! deterministic in-memory fake tools - no filesystem, terminal, or network
//! side effects beyond the one chat-completions POST per assistant turn:
//!
//! 1. `read_fixture` - the runner returns the fixture source;
//! 2. `run_fixture_test` - the runner returns a CONTROLLED failure report;
//! 3. `write_fixture_patch` - the runner returns a success report;
//! 4. the model delivers a concise final answer with no tool calls.
//!
//! Acceptance (feedback 02): correct ordering (read before run before
//! write; re-reading after a successful run is a violation), no invalid tool
//! names, all argument JSON valid and schema-conformant, no duplicate call
//! of the same tool with identical arguments after a successful result, a
//! final answer, and at most [`TOOL_LOOP_MAX_TURNS`] turns. Every violation
//! maps to exactly one [`FailureClass`]: a first turn with no tool calls is
//! [`FailureClass::NoToolCall`], an unknown tool name is
//! [`FailureClass::InvalidToolName`], unparseable or schema-violating
//! arguments are [`FailureClass::InvalidToolArguments`], a repeated
//! (name + identical arguments) call is [`FailureClass::DuplicateToolCall`],
//! and an incomplete or mis-ordered workflow (including a final answer that
//! skips part of the workflow) or a turn-budget breach is
//! [`FailureClass::ToolLoopDidNotConverge`].
//!
//! Cost gate: the live loop is opt-in. [`run_tool_loop`] refuses with
//! [`CloudflareError::MissingEnv`] naming `AUTH_CLOUDFLARE_LIVE_TESTS`
//! unless [`live_tests_enabled`] is true, so CI and unit tests can never
//! trigger a paid call by accident.
//!
//! Security contract: [`ToolLoopOutcome`] carries only tool-call
//! observations (name + arguments + turn) and a truncated final answer -
//! never tool outputs, never prompts, never the token. The Authorization
//! header is built exclusively through [`crate::fetch::auth_header`], and
//! every error string is token-scrubbed before it is returned.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::SecretString;
use crate::error::CloudflareError;
use crate::fetch::auth_header;
use crate::health::{FailureClass, MAX_EXCERPT_CHARS};

/// Environment variable gating the LIVE tool-loop harness. Must equal
/// exactly `"1"` for [`run_tool_loop`] to run; anything else (unset, `"0"`,
/// `"yes"`, whitespace) refuses with [`CloudflareError::MissingEnv`].
pub const LIVE_TESTS_ENV: &str = "AUTH_CLOUDFLARE_LIVE_TESTS";

/// Maximum assistant turns allowed for one tool-loop run (feedback 02:
/// "total turns ≤ 8"). A final answer ON the max turn is within budget; tool
/// calls on the max turn are a breach.
pub const TOOL_LOOP_MAX_TURNS: u32 = 8;

/// System prompt for the tool-loop harness (fixed fixture - never derived
/// from user input, so it can never carry secrets).
pub const TOOL_LOOP_SYSTEM_PROMPT: &str = "You are in a test harness. Complete the fixture workflow: read the fixture, run its test, write the patch, then answer concisely with the final status.";

/// User prompt for the tool-loop harness: names the deterministic target
/// fixture so the enum-constrained `fixture_id` argument is unambiguous.
pub const TOOL_LOOP_USER_PROMPT: &str = "Begin the fixture workflow. The target fixture id is 'calc'.";

/// The two deterministic in-memory fixtures. The enum on `fixture_id`
/// mirrors this list exactly - the wire schema and the validator can never
/// drift apart.
const FIXTURE_ID_VALUES: &[&str] = &["calc", "greeter"];

/// `calc` fixture source: `add()` is intentionally off by one (returns 3 for
/// `add(2, 2)`), so the controlled failure report ("got 3") is coherent with
/// the source the model just read.
const FIXTURE_CALC_SOURCE: &str = concat!(
	"//! calc fixture: add() is intentionally off by one so the test fails until patched.\n",
	"pub fn add(a: i32, b: i32) -> i32 {\n",
	"    a + b - 1\n",
	"}\n",
	"\n",
	"#[test]\n",
	"fn test_add() {\n",
	"    assert_eq!(add(2, 2), 4);\n",
	"}\n",
);

/// `greeter` fixture source: `greet()` is missing the comma, so the
/// controlled failure report is coherent with the source.
const FIXTURE_GREETER_SOURCE: &str = concat!(
	"//! greeter fixture: greet() is missing the comma so the test fails until patched.\n",
	"pub fn greet(name: &str) -> String {\n",
	"    format!(\"Hello {name}!\")\n",
	"}\n",
	"\n",
	"#[test]\n",
	"fn test_greet() {\n",
	"    assert_eq!(greet(\"World\"), \"Hello, World!\");\n",
	"}\n",
);

/// One parameter of a fake tool. Every parameter in this suite is a string;
/// `enum_values` constrains it when the wire schema carries an `enum`.
struct ParamSpec {
	name: &'static str,
	enum_values: &'static [&'static str],
}

/// One fake tool: its wire schema (`tool_schemas`) and its hand-rolled
/// validator (`arguments_valid_for_tool`) are both generated from this spec,
/// so the two can never disagree.
struct ToolSpec {
	name: &'static str,
	description: &'static str,
	params: &'static [ParamSpec],
	required: &'static [&'static str],
}

/// The three OpenAI-format fake tools. Order matters only for display; the
/// harness validates names against this list.
const TOOL_SPECS: &[ToolSpec] = &[
	ToolSpec {
		name: "read_fixture",
		description: "Read the Rust fixture source for the given fixture_id.",
		params: &[ParamSpec { name: "fixture_id", enum_values: FIXTURE_ID_VALUES }],
		required: &["fixture_id"],
	},
	ToolSpec {
		name: "run_fixture_test",
		description: "Run the fixture's test suite and return the controlled test report for the given fixture_id.",
		params: &[ParamSpec { name: "fixture_id", enum_values: FIXTURE_ID_VALUES }],
		required: &["fixture_id"],
	},
	ToolSpec {
		name: "write_fixture_patch",
		description: "Apply a patch that fixes the fixture source for the given fixture_id.",
		params: &[
			ParamSpec { name: "fixture_id", enum_values: FIXTURE_ID_VALUES },
			ParamSpec { name: "patch", enum_values: &[] },
		],
		required: &["fixture_id", "patch"],
	},
];

/// One observed tool call from one assistant turn (serialized snake_case).
///
/// `arguments` is the model's argument JSON as issued. If the arguments did
/// not parse as JSON, the raw argument string is stored wrapped in a JSON
/// string so the evidence is still preserved (never fabricated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolCallObservation {
	pub name: String,
	pub arguments: serde_json::Value,
	/// Assistant turn number the call was issued in (1-based).
	pub turn: u32,
}

/// Outcome of one tool-loop run.
///
/// Security contract: this struct has NO field for tool outputs or prompts -
/// only tool-call observations and the (truncated) final answer. The final
/// answer is capped at [`MAX_EXCERPT_CHARS`] (512) characters, char-safe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolLoopOutcome {
	pub converged: bool,
	/// Assistant turns actually issued (each assistant message plus its tool
	/// executions counts as one turn).
	pub turns_used: u32,
	pub max_turns: u32,
	pub tool_calls: Vec<ToolCallObservation>,
	pub failure_class: Option<FailureClass>,
	/// The model's final answer when one was given, truncated to 512 chars.
	pub final_answer: Option<String>,
}

impl ToolLoopOutcome {
	/// Build an outcome, truncating the final answer to
	/// [`MAX_EXCERPT_CHARS`] characters (char-safe: never splits a UTF-8
	/// scalar mid-sequence).
	pub fn new(
		converged: bool,
		turns_used: u32,
		max_turns: u32,
		tool_calls: Vec<ToolCallObservation>,
		failure_class: Option<FailureClass>,
		final_answer: Option<String>,
	) -> Self {
		Self {
			converged,
			turns_used,
			max_turns,
			tool_calls,
			failure_class,
			final_answer: final_answer.map(|answer| truncate_final_answer(&answer)),
		}
	}
}

/// Truncate a final answer to [`MAX_EXCERPT_CHARS`] characters, char-safe.
fn truncate_final_answer(answer: &str) -> String {
	if answer.chars().count() <= MAX_EXCERPT_CHARS {
		answer.to_string()
	} else {
		answer.chars().take(MAX_EXCERPT_CHARS).collect()
	}
}

/// True when the live harness is allowed to run: `AUTH_CLOUDFLARE_LIVE_TESTS`
/// must equal exactly `"1"` (no trimming, no aliases). Shared with the
/// sibling `verify.rs` integration runner via the crate root.
pub fn live_tests_enabled() -> bool {
	std::env::var(LIVE_TESTS_ENV).is_ok_and(|value| value == "1")
}

/// The three OpenAI-format fake-tool schemas, built from [`TOOL_SPECS`].
///
/// Every schema uses `additionalProperties: false` and declares its required
/// fields; `fixture_id` is enum-constrained to the deterministic fixtures so
/// the model cannot invent fixture ids.
pub fn tool_schemas() -> Vec<serde_json::Value> {
	TOOL_SPECS
		.iter()
		.map(|spec| {
			let mut properties = serde_json::Map::new();
			for param in spec.params {
				let mut property = serde_json::json!({ "type": "string" });
				if !param.enum_values.is_empty() {
					property["enum"] = serde_json::Value::Array(
						param
							.enum_values
							.iter()
							.map(|value| serde_json::Value::String((*value).to_string()))
							.collect(),
					);
				}
				properties.insert(param.name.to_string(), property);
			}
			serde_json::json!({
				"type": "function",
				"function": {
					"name": spec.name,
					"description": spec.description,
					"parameters": {
						"type": "object",
						"properties": serde_json::Value::Object(properties),
						"required": spec.required,
						"additionalProperties": false,
					},
				},
			})
		})
		.collect()
}

/// The fixture source for a known fixture id, if any (deterministic,
/// in-memory only).
pub fn fixture_source(fixture_id: &str) -> Option<String> {
	match fixture_id {
		"calc" => Some(FIXTURE_CALC_SOURCE.to_string()),
		"greeter" => Some(FIXTURE_GREETER_SOURCE.to_string()),
		_ => None,
	}
}

/// Controlled test-report output per fixture (deterministic, in-memory
/// only). Every report is a FAILURE: the model must consume it and react.
fn fixture_test_report(fixture_id: &str) -> Option<String> {
	match fixture_id {
		"calc" => Some("assertion failed: add(2, 2) == 4, got 3".to_string()),
		"greeter" => Some("assertion failed: greet(\"World\") == \"Hello, World!\", got \"Hello World!\"".to_string()),
		_ => None,
	}
}

/// Execute one fake tool in memory. Deterministic per (tool, arguments);
/// never touches the filesystem, terminal, or network.
///
/// - `read_fixture` → `{"status":"success","fixture_id":...,"source":...}`;
/// - `run_fixture_test` → `{"status":"fail","fixture_id":...,"output":...}`
///   (a CONTROLLED failure report - the execution succeeds, the report says
///   the fixture's test failed);
/// - `write_fixture_patch` → `{"status":"success","applied":true,...}`.
pub fn execute_tool(name: &str, arguments: &serde_json::Value) -> Result<serde_json::Value, String> {
	let fixture_id = arguments
		.get("fixture_id")
		.and_then(serde_json::Value::as_str)
		.unwrap_or_default();
	match name {
		"read_fixture" => {
			let source = fixture_source(fixture_id).ok_or_else(|| format!("unknown fixture_id {fixture_id:?}"))?;
			Ok(serde_json::json!({ "status": "success", "fixture_id": fixture_id, "source": source }))
		},
		"run_fixture_test" => {
			let output = fixture_test_report(fixture_id).ok_or_else(|| format!("unknown fixture_id {fixture_id:?}"))?;
			Ok(serde_json::json!({ "status": "fail", "fixture_id": fixture_id, "output": output }))
		},
		"write_fixture_patch" => {
			if fixture_source(fixture_id).is_none() {
				return Err(format!("unknown fixture_id {fixture_id:?}"));
			}
			Ok(serde_json::json!({ "status": "success", "applied": true, "fixture_id": fixture_id }))
		},
		other => Err(format!("unknown tool {other}")),
	}
}

/// Ordering acceptance: the full read → run → write workflow must appear in
/// that order, with no re-read after a run and no run after a write.
///
/// Stage numbering: read=1, run=2, write=3. Valid iff every stage appears at
/// least once AND the stage sequence is non-decreasing. Unknown tool names
/// are ignored here (they are a name-check concern, [`FailureClass::InvalidToolName`],
/// not an ordering concern). A read-only or read→write sequence fails: the
/// workflow is incomplete.
pub fn validate_ordering(calls: &[ToolCallObservation]) -> bool {
	let stages: Vec<u8> = calls.iter().filter_map(|call| stage_of(&call.name)).collect();
	if stages.len() < 3 {
		return false;
	}
	let has_all_stages = stages.contains(&1) && stages.contains(&2) && stages.contains(&3);
	has_all_stages && stages.windows(2).all(|pair| pair[0] <= pair[1])
}

fn stage_of(name: &str) -> Option<u8> {
	match name {
		"read_fixture" => Some(1),
		"run_fixture_test" => Some(2),
		"write_fixture_patch" => Some(3),
		_ => None,
	}
}

/// Duplicate acceptance: references to every call whose (name, identical
/// arguments) pair was already issued earlier in the sequence - including a
/// repeated `run_fixture_test` after a successful run, and a re-read of the
/// same fixture. Argument equality is content-based (JSON object key order
/// does not matter).
pub fn find_duplicates(calls: &[ToolCallObservation]) -> Vec<&ToolCallObservation> {
	let mut seen: Vec<(&str, &serde_json::Value)> = Vec::new();
	let mut duplicates: Vec<&ToolCallObservation> = Vec::new();
	for call in calls {
		let already_seen = seen.iter().any(|(name, args)| *name == call.name && **args == call.arguments);
		if already_seen {
			duplicates.push(call);
		} else {
			seen.push((call.name.as_str(), &call.arguments));
		}
	}
	duplicates
}

/// Argument acceptance: every call's arguments must parse as a JSON object
/// and satisfy the tool's schema - required fields present, correct types,
/// enum values respected, and no extra properties (`additionalProperties:
/// false`). Unknown tool names fail (no schema exists for them). Pure and
/// hand-rolled: no `jsonschema` dependency.
pub fn all_arguments_valid(calls: &[ToolCallObservation]) -> bool {
	calls.iter().all(|call| arguments_valid_for_tool(&call.name, &call.arguments))
}

fn spec_for(name: &str) -> Option<&'static ToolSpec> {
	TOOL_SPECS.iter().find(|spec| spec.name == name)
}

fn arguments_valid_for_tool(name: &str, arguments: &serde_json::Value) -> bool {
	let Some(spec) = spec_for(name) else {
		return false;
	};
	let serde_json::Value::Object(map) = arguments else {
		return false;
	};
	if spec.required.iter().any(|required| !map.contains_key(*required)) {
		return false;
	}
	for (key, value) in map.iter() {
		let Some(param) = spec.params.iter().find(|param| param.name == key.as_str()) else {
			return false;
		};
		if !value.is_string() {
			return false;
		}
		if !param.enum_values.is_empty() {
			let Some(actual) = value.as_str() else {
				return false;
			};
			if !param.enum_values.contains(&actual) {
				return false;
			}
		}
	}
	true
}

/// Pure convergence decision for one assistant turn - factored out of the
/// live loop so the turn-counting rules are unit-testable without a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnVerdict {
	/// First turn carried no tool calls: the model skipped the workflow.
	NoToolCall,
	/// A later turn carried no tool calls: the model delivered a final answer.
	FinalAnswer,
	/// The turn carried tool calls and the budget allows another turn.
	Continue,
	/// The max turn carried tool calls: a further turn would breach the
	/// budget, so the loop cannot converge.
	TurnLimitBreached,
}

fn assess_turn(has_tool_calls: bool, turn: u32, max_turns: u32) -> TurnVerdict {
	if !has_tool_calls {
		if turn == 1 { TurnVerdict::NoToolCall } else { TurnVerdict::FinalAnswer }
	} else if turn >= max_turns {
		TurnVerdict::TurnLimitBreached
	} else {
		TurnVerdict::Continue
	}
}

/// Run the LIVE multi-turn tool-loop conformance suite for one model.
///
/// Gated: refuses with [`CloudflareError::MissingEnv`] naming
/// [`LIVE_TESTS_ENV`] unless [`live_tests_enabled`] is true, so this can
/// never fire a paid call by accident. The loop POSTs OpenAI-format chat
/// completions to `{base_url}/chat/completions` (Bearer auth via
/// [`auth_header`]), executes the model's tool calls in memory, appends
/// `role: tool` results with matching `tool_call_id`, and continues until a
/// final answer (no tool calls), a violation, or the turn budget is spent.
/// On violation the run stops immediately with the specific [`FailureClass`].
pub fn run_tool_loop(
	account_id: &str,
	token: &SecretString,
	base_url: &str,
	model_id: &str,
	timeout: Duration,
) -> Result<ToolLoopOutcome, CloudflareError> {
	if !live_tests_enabled() {
		return Err(CloudflareError::MissingEnv {
			env_var: LIVE_TESTS_ENV,
			hint: format!(
				"live multi-turn tool-loop conformance for {model_id} on account {account_id} is opt-in and paid: export {LIVE_TESTS_ENV}=1 to run it (and set AUTH_CLOUDFLARE_MAX_COST_USD to cap spend). Never enable it in CI."
			),
		});
	}
	if token.as_ref().trim().is_empty() {
		return Err(CloudflareError::MissingEnv {
			env_var: crate::auth::TOKEN_ENV,
			hint: "the API token is empty - export a scoped Workers AI token (Account → Workers AI → Write)"
				.to_string(),
		});
	}

	let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
	let agent = ureq::AgentBuilder::new().timeout(timeout).build();
	let mut messages: Vec<serde_json::Value> = vec![
		serde_json::json!({ "role": "system", "content": TOOL_LOOP_SYSTEM_PROMPT }),
		serde_json::json!({ "role": "user", "content": TOOL_LOOP_USER_PROMPT }),
	];
	let mut calls: Vec<ToolCallObservation> = Vec::new();
	let mut turn: u32 = 0;
	loop {
		turn += 1;
		let request = serde_json::json!({
			"model": model_id,
			"messages": messages,
			"tools": tool_schemas(),
			"tool_choice": "auto",
		});
		let response = post_chat_completion(&agent, &url, token, &request)?;
		let message = response
			.get("choices")
			.and_then(|choices| choices.as_array())
			.and_then(|choices| choices.first())
			.and_then(|choice| choice.get("message"))
			.cloned()
			.ok_or_else(|| CloudflareError::Http("tool-loop response missing choices[0].message".to_string()))?;
		let tool_calls = message.get("tool_calls").and_then(|calls| calls.as_array()).cloned();
		let has_tool_calls = tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
		let final_answer = message.get("content").and_then(serde_json::Value::as_str).map(str::to_string);
		match assess_turn(has_tool_calls, turn, TOOL_LOOP_MAX_TURNS) {
			TurnVerdict::NoToolCall => {
				return Ok(ToolLoopOutcome::new(
					false,
					turn,
					TOOL_LOOP_MAX_TURNS,
					calls,
					Some(FailureClass::NoToolCall),
					final_answer,
				));
			},
			TurnVerdict::FinalAnswer => {
				// The final answer is only a pass when the full workflow ran
				// in order, with no invalid arguments and no duplicates; an
				// incomplete or mis-ordered workflow is a non-convergence.
				let converged =
					validate_ordering(&calls) && find_duplicates(&calls).is_empty() && all_arguments_valid(&calls);
				let failure_class = if converged { None } else { Some(FailureClass::ToolLoopDidNotConverge) };
				return Ok(ToolLoopOutcome::new(
					converged,
					turn,
					TOOL_LOOP_MAX_TURNS,
					calls,
					failure_class,
					final_answer,
				));
			},
			TurnVerdict::TurnLimitBreached => {
				return Ok(ToolLoopOutcome::new(
					false,
					turn,
					TOOL_LOOP_MAX_TURNS,
					calls,
					Some(FailureClass::ToolLoopDidNotConverge),
					None,
				));
			},
			TurnVerdict::Continue => {
				let tool_calls = tool_calls.expect("Continue implies non-empty tool calls");
				let mut turn_calls: Vec<ToolCallObservation> = Vec::new();
				let mut results: Vec<(String, serde_json::Value)> = Vec::new();
				for tool_call in &tool_calls {
					let name = tool_call
						.pointer("/function/name")
						.and_then(serde_json::Value::as_str)
						.unwrap_or("<missing>");
					let raw_arguments = tool_call
						.pointer("/function/arguments")
						.and_then(serde_json::Value::as_str)
						.unwrap_or_default();
					let call_id = tool_call.get("id").and_then(serde_json::Value::as_str).unwrap_or_default();
					// 1. The tool must be one of the three fake tools.
					if spec_for(name).is_none() {
						calls.push(ToolCallObservation {
							name: name.to_string(),
							arguments: serde_json::Value::String(raw_arguments.to_string()),
							turn,
						});
						return Ok(ToolLoopOutcome::new(
							false,
							turn,
							TOOL_LOOP_MAX_TURNS,
							calls,
							Some(FailureClass::InvalidToolName),
							None,
						));
					}
					// 2. The arguments must parse as JSON.
					let arguments: serde_json::Value = match serde_json::from_str(raw_arguments) {
						Ok(value) => value,
						Err(_) => {
							calls.push(ToolCallObservation {
								name: name.to_string(),
								arguments: serde_json::Value::String(raw_arguments.to_string()),
								turn,
							});
							return Ok(ToolLoopOutcome::new(
								false,
								turn,
								TOOL_LOOP_MAX_TURNS,
								calls,
								Some(FailureClass::InvalidToolArguments),
								None,
							));
						},
					};
					// 3. The arguments must satisfy the tool schema.
					if !arguments_valid_for_tool(name, &arguments) {
						calls.push(ToolCallObservation { name: name.to_string(), arguments, turn });
						return Ok(ToolLoopOutcome::new(
							false,
							turn,
							TOOL_LOOP_MAX_TURNS,
							calls,
							Some(FailureClass::InvalidToolArguments),
							None,
						));
					}
					// 4. The same tool with identical arguments must not be
					// issued twice (checked against every executed call).
					let duplicated = calls
						.iter()
						.chain(turn_calls.iter())
						.any(|prior| prior.name == name && prior.arguments == arguments);
					if duplicated {
						calls.push(ToolCallObservation { name: name.to_string(), arguments, turn });
						return Ok(ToolLoopOutcome::new(
							false,
							turn,
							TOOL_LOOP_MAX_TURNS,
							calls,
							Some(FailureClass::DuplicateToolCall),
							None,
						));
					}
					// Schema validation guarantees execution succeeds; the
					// error arm is a defense-in-depth safety net.
					let result = execute_tool(name, &arguments)
						.map_err(|message| CloudflareError::Http(redact_token(&message, token.as_ref())))?;
					turn_calls.push(ToolCallObservation { name: name.to_string(), arguments, turn });
					results.push((call_id.to_string(), result));
				}
				// Echo the assistant message with its tool calls, then append
				// one `role: tool` result per call (OpenAI format).
				messages.push(serde_json::json!({ "role": "assistant", "content": null, "tool_calls": tool_calls }));
				for (call_id, result) in results {
					messages.push(serde_json::json!({
						"role": "tool",
						"tool_call_id": call_id,
						"content": result.to_string(),
					}));
				}
				calls.extend(turn_calls);
			},
		}
	}
}

/// POST one chat-completions request with Bearer auth and map failures to
/// the typed error taxonomy. Every text source is token-scrubbed before it
/// reaches an error string.
fn post_chat_completion(
	agent: &ureq::Agent,
	url: &str,
	token: &SecretString,
	body: &serde_json::Value,
) -> Result<serde_json::Value, CloudflareError> {
	let request = agent
		.post(url)
		.set("Authorization", &auth_header(token))
		.set("Accept", "application/json")
		.set("Content-Type", "application/json");
	let payload = body.to_string();
	let (status, response) = match request.send_string(&payload) {
		Ok(response) => (response.status(), response),
		Err(ureq::Error::Status(status, response)) => (status, response),
		Err(transport) => {
			return Err(CloudflareError::Http(redact_token(&transport.to_string(), token.as_ref())));
		},
	};
	let raw = response.into_string().map_err(|error| {
		CloudflareError::Http(redact_token(&format!("read response body: {error}"), token.as_ref()))
	})?;
	let raw = redact_token(&raw, token.as_ref());
	if status != 200 {
		return Err(map_http_error(status, &raw));
	}
	serde_json::from_str(&raw).map_err(|_| CloudflareError::Http("tool-loop response was not valid JSON".to_string()))
}

/// Map a non-200 chat-completions status to the typed error taxonomy,
/// carrying the Cloudflare envelope message when present.
fn map_http_error(status: u16, body: &str) -> CloudflareError {
	let labeled = |code: u16, label: &str| -> CloudflareError {
		CloudflareError::Api {
			code: u32::from(code),
			message: format!(
				"{label} (HTTP {code}){}",
				envelope_message(body).map(|m| format!(": {m}")).unwrap_or_default()
			),
		}
	};
	match status {
		401 => labeled(401, "unauthorized"),
		403 => labeled(403, "forbidden"),
		429 => labeled(429, "rate limited"),
		500..=599 => {
			CloudflareError::Http(format!("tool-loop endpoint returned HTTP {status} (transient server error)"))
		},
		other => CloudflareError::Api { code: u32::from(other), message: format!("HTTP {other}: {body}") },
	}
}

/// First Cloudflare envelope `errors[0].message`, if the body parses.
fn envelope_message(body: &str) -> Option<String> {
	serde_json::from_str::<serde_json::Value>(body)
		.ok()?
		.get("errors")?
		.as_array()?
		.first()?
		.get("message")?
		.as_str()
		.map(str::to_string)
}

/// Replace the token with a redaction marker in any text that could reach an
/// error string (defense in depth - mirrors `fetch.rs`).
fn redact_token(text: &str, token: &str) -> String {
	if token.is_empty() {
		text.to_string()
	} else {
		text.replace(token, "<redacted>")
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	fn obs(name: &str, arguments: serde_json::Value, turn: u32) -> ToolCallObservation {
		ToolCallObservation { name: name.to_string(), arguments, turn }
	}

	#[test]
	fn ordering_accepts_the_full_workflow() {
		let calls = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("run_fixture_test", json!({"fixture_id": "calc"}), 2),
			obs(
				"write_fixture_patch",
				json!({"fixture_id": "calc", "patch": "fix the off-by-one"}),
				3,
			),
		];
		assert!(validate_ordering(&calls));
	}

	#[test]
	fn ordering_allows_extra_reads_before_the_run() {
		// Two different fixture reads, then run, then write: non-decreasing.
		let calls = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("read_fixture", json!({"fixture_id": "greeter"}), 1),
			obs("run_fixture_test", json!({"fixture_id": "calc"}), 2),
			obs("write_fixture_patch", json!({"fixture_id": "calc", "patch": "fix"}), 3),
		];
		assert!(validate_ordering(&calls));
	}

	#[test]
	fn ordering_rejects_reread_after_run() {
		// Re-reading after a successful run is a violation.
		let calls = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("run_fixture_test", json!({"fixture_id": "calc"}), 2),
			obs("read_fixture", json!({"fixture_id": "calc"}), 3),
		];
		assert!(!validate_ordering(&calls));
	}

	#[test]
	fn ordering_rejects_run_before_read_and_run_after_write() {
		let run_first = vec![
			obs("run_fixture_test", json!({"fixture_id": "calc"}), 1),
			obs("read_fixture", json!({"fixture_id": "calc"}), 2),
			obs("write_fixture_patch", json!({"fixture_id": "calc", "patch": "fix"}), 3),
		];
		assert!(!validate_ordering(&run_first));
		let write_then_run = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("write_fixture_patch", json!({"fixture_id": "calc", "patch": "fix"}), 2),
			obs("run_fixture_test", json!({"fixture_id": "calc"}), 3),
		];
		assert!(!validate_ordering(&write_then_run));
	}

	#[test]
	fn ordering_rejects_incomplete_workflows() {
		// Read-only: the model stopped before running or patching.
		let read_only = vec![obs("read_fixture", json!({"fixture_id": "calc"}), 1)];
		assert!(!validate_ordering(&read_only));
		// Read → write, skipping the run: the workflow is incomplete.
		let skipped_run = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("write_fixture_patch", json!({"fixture_id": "calc", "patch": "fix"}), 2),
		];
		assert!(!validate_ordering(&skipped_run));
		assert!(!validate_ordering(&[]));
	}

	#[test]
	fn ordering_ignores_unknown_tool_names() {
		// Unknown names are a name-check concern, not an ordering concern.
		let calls = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("some_bogus_tool", json!({}), 1),
			obs("run_fixture_test", json!({"fixture_id": "calc"}), 2),
			obs("write_fixture_patch", json!({"fixture_id": "calc", "patch": "fix"}), 3),
		];
		assert!(validate_ordering(&calls));
	}

	#[test]
	fn duplicates_are_detected_by_name_and_identical_arguments() {
		let repeated_read = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("read_fixture", json!({"fixture_id": "calc"}), 2),
		];
		let duplicates = find_duplicates(&repeated_read);
		assert_eq!(duplicates.len(), 1);
		assert_eq!(duplicates[0].name, "read_fixture");
		assert_eq!(duplicates[0].turn, 2);

		// Same tool, different arguments: NOT a duplicate.
		let different_args = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("read_fixture", json!({"fixture_id": "greeter"}), 1),
		];
		assert!(find_duplicates(&different_args).is_empty());

		// Repeated run after a successful run is a duplicate too.
		let repeated_run = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("run_fixture_test", json!({"fixture_id": "calc"}), 2),
			obs("run_fixture_test", json!({"fixture_id": "calc"}), 3),
		];
		assert_eq!(find_duplicates(&repeated_run).len(), 1);

		// JSON object key order does not matter for identical arguments.
		let reordered = vec![
			obs("write_fixture_patch", json!({"fixture_id": "calc", "patch": "fix"}), 1),
			obs("write_fixture_patch", json!({"patch": "fix", "fixture_id": "calc"}), 2),
		];
		assert_eq!(find_duplicates(&reordered).len(), 1);
	}

	#[test]
	fn argument_validation_accepts_exact_calls() {
		let calls = vec![
			obs("read_fixture", json!({"fixture_id": "calc"}), 1),
			obs("run_fixture_test", json!({"fixture_id": "greeter"}), 2),
			obs("write_fixture_patch", json!({"fixture_id": "calc", "patch": "fix add"}), 3),
		];
		assert!(all_arguments_valid(&calls));
	}

	#[test]
	fn argument_validation_rejects_missing_required_fields() {
		assert!(!all_arguments_valid(&[obs("read_fixture", json!({}), 1)]));
		assert!(!all_arguments_valid(&[obs(
			"write_fixture_patch",
			json!({"fixture_id": "calc"}),
			1
		)]));
	}

	#[test]
	fn argument_validation_rejects_wrong_enum_value() {
		assert!(!all_arguments_valid(&[obs("read_fixture", json!({"fixture_id": "nope"}), 1)]));
	}

	#[test]
	fn argument_validation_rejects_extra_properties() {
		// additionalProperties: false must be respected.
		assert!(!all_arguments_valid(&[obs(
			"read_fixture",
			json!({"fixture_id": "calc", "extra": 1}),
			1
		)]));
	}

	#[test]
	fn argument_validation_rejects_wrong_types_and_unknown_tools() {
		assert!(!all_arguments_valid(&[obs("read_fixture", json!({"fixture_id": 42}), 1)]));
		assert!(!all_arguments_valid(&[obs("bogus_tool", json!({}), 1)]));
		assert!(!all_arguments_valid(&[obs(
			"read_fixture",
			serde_json::Value::String("not an object".to_string()),
			1
		)]));
	}

	#[test]
	fn convergence_counting_on_synthetic_turns() {
		// Turns 1..3 carry tools, turn 4 is the final answer: 4 turns used.
		assert_eq!(assess_turn(true, 1, 8), TurnVerdict::Continue);
		assert_eq!(assess_turn(true, 2, 8), TurnVerdict::Continue);
		assert_eq!(assess_turn(true, 3, 8), TurnVerdict::Continue);
		assert_eq!(assess_turn(false, 4, 8), TurnVerdict::FinalAnswer);
		// Turn 1 without tools: NoToolCall, even though prose was produced.
		assert_eq!(assess_turn(false, 1, 8), TurnVerdict::NoToolCall);
		// Tools on the max turn: the budget is breached.
		assert_eq!(assess_turn(true, 8, 8), TurnVerdict::TurnLimitBreached);
		// A final answer ON the max turn is within budget (turns ≤ 8).
		assert_eq!(assess_turn(false, 8, 8), TurnVerdict::FinalAnswer);
	}

	#[test]
	fn outcome_serde_roundtrip_and_snake_case() {
		let outcome = ToolLoopOutcome::new(
			true,
			4,
			TOOL_LOOP_MAX_TURNS,
			vec![
				obs("read_fixture", json!({"fixture_id": "calc"}), 1),
				obs("run_fixture_test", json!({"fixture_id": "calc"}), 2),
				obs("write_fixture_patch", json!({"fixture_id": "calc", "patch": "fix"}), 3),
			],
			None,
			Some("final status: pass".to_string()),
		);
		let json = serde_json::to_string(&outcome).expect("ser");
		let back: ToolLoopOutcome = serde_json::from_str(&json).expect("de");
		assert_eq!(back, outcome);
		let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
		for key in [
			"converged",
			"turns_used",
			"max_turns",
			"tool_calls",
			"failure_class",
			"final_answer",
		] {
			assert!(value.get(key).is_some(), "missing snake_case key {key}");
		}
		assert_eq!(value["tool_calls"][0]["arguments"]["fixture_id"], "calc");
		assert!(value["tool_calls"][0].get("turn").is_some());
		assert_eq!(value["converged"], true);
		// The outcome must never carry tool outputs or prompts.
		let rendered = json.to_lowercase();
		assert!(
			!rendered.contains("assertion failed"),
			"tool outputs must never enter the outcome"
		);
		assert!(!rendered.contains("test harness"), "prompts must never enter the outcome");
		assert!(!rendered.contains("\"source\""), "tool outputs must never enter the outcome");
	}

	#[test]
	fn final_answer_is_truncated_char_safe() {
		let long = "€".repeat(600);
		let outcome = ToolLoopOutcome::new(true, 1, 8, vec![], None, Some(long.clone()));
		let answer = outcome.final_answer.expect("answer present");
		assert_eq!(answer.chars().count(), MAX_EXCERPT_CHARS);
		assert_eq!(answer, "€".repeat(MAX_EXCERPT_CHARS));
		let short = ToolLoopOutcome::new(true, 1, 8, vec![], None, Some("ok".to_string()));
		assert_eq!(short.final_answer.as_deref(), Some("ok"));
	}

	#[test]
	fn live_gate_closed_returns_refusal_without_network() {
		// Ensure the gate is closed regardless of the developer's environment,
		// then prove the refusal happens BEFORE any network is attempted.
		with_live_tests_env(None, || {
			let token = SecretString::new("cfut_test_synthetic_token_0001");
			let error = run_tool_loop(
				"0123456789abcdef0123456789abcdef",
				&token,
				"https://example.test/ai/v1",
				"@cf/deepseek-ai/deepseek-v4-flash-0731",
				Duration::from_secs(1),
			)
			.expect_err("gate must refuse without AUTH_CLOUDFLARE_LIVE_TESTS=1");
			match error {
				CloudflareError::MissingEnv { env_var, hint } => {
					assert_eq!(env_var, LIVE_TESTS_ENV);
					assert!(hint.contains(LIVE_TESTS_ENV), "hint must name the env var: {hint}");
					assert!(
						hint.to_lowercase().contains("opt-in"),
						"hint must explain the opt-in gate: {hint}"
					);
				},
				other => panic!("expected MissingEnv refusal, got {other:?}"),
			}
		});
	}

	#[test]
	fn live_tests_enabled_matches_exactly_one() {
		with_live_tests_env(Some("1"), || assert!(live_tests_enabled()));
		with_live_tests_env(None, || assert!(!live_tests_enabled()));
		with_live_tests_env(Some("0"), || assert!(!live_tests_enabled()));
		with_live_tests_env(Some("yes"), || assert!(!live_tests_enabled()));
		with_live_tests_env(Some("1 "), || {
			assert!(!live_tests_enabled(), "whitespace is not exactly '1'");
		});
	}

	#[test]
	fn fake_tools_are_deterministic_and_in_memory() {
		let read = execute_tool("read_fixture", &json!({"fixture_id": "calc"})).expect("read succeeds");
		assert_eq!(read["status"], "success");
		let source = read["source"].as_str().expect("source present");
		assert!(source.contains("fn add"));
		assert!(source.contains("assert_eq!(add(2, 2), 4)"));
		assert_eq!(
			execute_tool("read_fixture", &json!({"fixture_id": "calc"})).expect("deterministic"),
			read
		);

		let run = execute_tool("run_fixture_test", &json!({"fixture_id": "calc"})).expect("run succeeds");
		assert_eq!(run["status"], "fail", "the report is a controlled failure");
		assert_eq!(run["output"], "assertion failed: add(2, 2) == 4, got 3");

		let write = execute_tool("write_fixture_patch", &json!({"fixture_id": "calc", "patch": "fix"}))
			.expect("write succeeds");
		assert_eq!(write["status"], "success");
		assert_eq!(write["applied"].as_bool(), Some(true));

		assert!(execute_tool("read_fixture", &json!({"fixture_id": "nope"})).is_err());
		assert!(execute_tool("bogus", &json!({})).is_err());
	}

	#[test]
	fn tool_schemas_are_openai_function_shaped() {
		let schemas = tool_schemas();
		assert_eq!(schemas.len(), 3);
		let names: Vec<&str> = schemas.iter().map(|s| s["function"]["name"].as_str().unwrap()).collect();
		assert_eq!(names, vec!["read_fixture", "run_fixture_test", "write_fixture_patch"]);
		for schema in &schemas {
			assert_eq!(schema["type"], "function");
			assert_eq!(schema["function"]["parameters"]["type"], "object");
			assert_eq!(schema["function"]["parameters"]["additionalProperties"].as_bool(), Some(false));
			let required = schema["function"]["parameters"]["required"]
				.as_array()
				.expect("required present");
			assert!(!required.is_empty());
		}
		// fixture_id is enum-constrained to the deterministic fixture ids.
		assert_eq!(
			schemas[0]["function"]["parameters"]["properties"]["fixture_id"]["enum"],
			json!(["calc", "greeter"])
		);
	}

	/// `std::env` is process-global and tests run in parallel - serialize env
	/// mutation through a static mutex and restore prior values after.
	static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

	fn with_live_tests_env(value: Option<&str>, f: impl FnOnce()) {
		let _guard = ENV_LOCK.lock().unwrap();
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
}
