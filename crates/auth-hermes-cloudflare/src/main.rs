//! auth-hermes-cloudflare CLI - optional utility around the plugin/binary.
//!
//! The primary runtime artifact is the `auth-cloudflare` executable (doctor,
//! catalog, model verify). This optional binary manages it from the Rust
//! side (feedback 06 "optional utility", feedback 04 command surface):
//!
//! ```text
//! auth-hermes-cloudflare status                 # JSON: plugin + binary state
//! auth-hermes-cloudflare install [options]      # run download.sh, then doctor
//! auth-hermes-cloudflare upgrade [options]      # alias of install
//! auth-hermes-cloudflare doctor                 # run 'auth-cloudflare doctor --format json'
//! auth-hermes-cloudflare uninstall              # remove ~/.hermes/bin/auth-cloudflare
//! auth-hermes-cloudflare unlink                 # alias of uninstall
//! auth-hermes-cloudflare link --source <path>   # symlink/copy the managed binary to <path>
//! ```
//!
//! Exit codes: 0 ok, 1 operational error; `doctor` forwards the engine's exit
//! code. The binary never touches the API token and never prints secrets.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

/// Plugin identity (matches lib.rs / plugin manifest).
const PLUGIN_NAME: &str = "auth-hermes-cloudflare";
const BINARY_NAME: &str = "auth-cloudflare";

/// Managed binary install path (the same path download.sh and the plugin
/// locator use).
fn managed_binary_path() -> PathBuf {
	PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "~".to_string()))
		.join(".hermes")
		.join("bin")
		.join(BINARY_NAME)
}

/// Plugin submodule directory, discovered from common layouts relative to
/// this binary's CARGO_MANIFEST_DIR (repo checkout) or the managed plugins
/// dir (installed profile).
fn plugin_dirs() -> Vec<PathBuf> {
	let mut dirs = Vec::new();
	if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
		let manifest = PathBuf::from(manifest);
		// <repo>/crates/auth-hermes-cloudflare -> <repo>/plugins/auth-hermes-cloudflare
		for candidate in [
			manifest
				.parent()
				.and_then(Path::parent)
				.map(|repo| repo.join("plugins").join(PLUGIN_NAME)),
			manifest.parent().map(|parent| parent.join(PLUGIN_NAME)),
		]
		.into_iter()
		.flatten()
		{
			dirs.push(candidate);
		}
	}
	dirs.push(
		PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "~".to_string()))
			.join(".hermes")
			.join("plugins")
			.join(PLUGIN_NAME),
	);
	dirs
}

/// Locate the auth-cloudflare executable: AUTH_CLOUDFLARE_BIN env → PATH →
/// ~/.hermes/bin → plugin bin/ → plugin binaries/ (mirrors the Python
/// plugin's locator order).
fn locate_binary() -> Option<PathBuf> {
	if let Ok(explicit) = std::env::var("AUTH_CLOUDFLARE_BIN") {
		let candidate = PathBuf::from(explicit);
		if candidate.is_file() {
			return Some(candidate);
		}
	}
	if let Some(found) = which(BINARY_NAME) {
		return Some(found);
	}
	for dir in plugin_dirs() {
		for candidate in [dir.join("bin").join(BINARY_NAME), dir.join("binaries").join(BINARY_NAME)] {
			if candidate.is_file() {
				return Some(candidate);
			}
		}
	}
	let managed = managed_binary_path();
	if managed.is_file() {
		return Some(managed);
	}
	None
}

/// Minimal PATH lookup (no `which` crate dependency).
fn which(name: &str) -> Option<PathBuf> {
	let path = std::env::var("PATH").unwrap_or_default();
	for dir in path.split(':') {
		if dir.is_empty() {
			continue;
		}
		let candidate = PathBuf::from(dir).join(name);
		if candidate.is_file() {
			return Some(candidate);
		}
	}
	None
}

/// Run `auth-cloudflare version --format json` and parse it.
fn binary_version(bin: &Path) -> Result<serde_json::Value, String> {
	let output = Command::new(bin)
		.args(["version", "--format", "json"])
		.output()
		.map_err(|error| format!("cannot execute {bin:?}: {error}"))?;
	if !output.status.success() {
		return Err(format!("{BINARY_NAME} version exited with {}", output.status));
	}
	serde_json::from_slice(&output.stdout)
		.map_err(|error| format!("{BINARY_NAME} version returned invalid JSON: {error}"))
}

/// Run `auth-cloudflare doctor --format json` (never a network or paid call).
/// Returns `(exit_code, raw_stdout, parsed_json_if_valid)`. The engine's
/// doctor output is already token-redacted.
fn run_doctor(bin: &Path) -> Result<(i32, String, Option<serde_json::Value>), String> {
	let output = Command::new(bin)
		.args(["doctor", "--format", "json"])
		.output()
		.map_err(|error| format!("cannot execute {bin:?}: {error}"))?;
	let raw = String::from_utf8_lossy(&output.stdout).into_owned();
	let code = output.status.code().unwrap_or(1);
	let parsed = serde_json::from_str(&raw).ok();
	Ok((code, raw, parsed))
}

/// Install options shared by `install` and its `upgrade` alias.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct InstallOptions {
	/// Override the directory searched for `download.sh`.
	plugin_dir: Option<PathBuf>,
	/// Passed to `download.sh` as `AUTH_CLOUDFLARE_BIN` (final install path).
	binary: Option<PathBuf>,
	/// Print manual remediation instead of running `download.sh`.
	no_download: bool,
}

/// Parsed command surface (feedback 04).
#[derive(Debug, Clone, PartialEq, Eq)]
enum CliCommand {
	Status,
	Install(InstallOptions),
	Upgrade(InstallOptions),
	Doctor,
	Uninstall,
	Link { source: PathBuf },
	Help,
}

/// Parse command-line arguments into a [`CliCommand`]. Pure (no environment or
/// filesystem access), so it is unit-testable.
fn parse_args(args: &[String]) -> Result<CliCommand, String> {
	let Some((cmd, rest)) = args.split_first() else {
		return Ok(CliCommand::Status);
	};
	match cmd.as_str() {
		"status" => {
			if !rest.is_empty() {
				return Err("status takes no arguments".to_string());
			}
			Ok(CliCommand::Status)
		},
		"install" => Ok(CliCommand::Install(parse_install_options(rest)?)),
		"upgrade" => Ok(CliCommand::Upgrade(parse_install_options(rest)?)),
		"doctor" => {
			if !rest.is_empty() {
				return Err("doctor takes no arguments".to_string());
			}
			Ok(CliCommand::Doctor)
		},
		"uninstall" | "unlink" => Ok(CliCommand::Uninstall),
		"link" => match rest {
			[cmd, path] if cmd.as_str() == "--source" => Ok(CliCommand::Link { source: PathBuf::from(path.as_str()) }),
			_ => Err("link requires --source <path>".to_string()),
		},
		"--help" | "-h" | "help" => Ok(CliCommand::Help),
		other => Err(format!("unknown command {other:?}")),
	}
}

/// Parse the flag surface shared by `install` and `upgrade`.
fn parse_install_options(args: &[String]) -> Result<InstallOptions, String> {
	let mut opts = InstallOptions::default();
	let mut iter = args.iter();
	while let Some(arg) = iter.next() {
		match arg.as_str() {
			"--no-download" => opts.no_download = true,
			"--plugin-dir" => {
				let value = iter.next().ok_or_else(|| "--plugin-dir requires a <dir>".to_string())?;
				opts.plugin_dir = Some(PathBuf::from(value.as_str()));
			},
			"--binary" => {
				let value = iter.next().ok_or_else(|| "--binary requires a <path>".to_string())?;
				opts.binary = Some(PathBuf::from(value.as_str()));
			},
			other => return Err(format!("unknown install option {other:?}")),
		}
	}
	Ok(opts)
}

/// Locate `download.sh`, honouring an explicit `--plugin-dir` override first.
fn find_download_script(plugin_dir: Option<&Path>) -> Option<PathBuf> {
	if let Some(dir) = plugin_dir {
		let candidate = dir.join("download.sh");
		return candidate.is_file().then_some(candidate);
	}
	plugin_dirs()
		.iter()
		.map(|dir| dir.join("download.sh"))
		.find(|script| script.is_file())
}

/// Token-free manual remediation printed by `install --no-download`.
fn no_download_remediation(script: Option<&Path>) -> String {
	let installer = script
		.map(|script| format!("bash {script:?}"))
		.unwrap_or_else(|| format!("bash <{PLUGIN_NAME}>/download.sh"));
	format!(
		"--no-download: skipping download.sh. Install the engine manually, then re-run 'auth-hermes-cloudflare doctor':\n  1. {installer} [version]\n  2. cargo install auth-cloudflare --locked\n  3. link an existing binary: auth-hermes-cloudflare link --source /path/to/auth-cloudflare"
	)
}

fn cmd_status(out: &mut dyn std::io::Write) -> i32 {
	let bin = locate_binary();
	let version = bin.as_deref().and_then(|path| binary_version(path).ok());
	let managed = managed_binary_path();
	let report = json!({
		"plugin": PLUGIN_NAME,
		"binary": {
			"located": bin.as_ref().map(|path| path.display().to_string()),
			"managed_path": managed.display().to_string(),
			"managed_present": managed.is_file(),
			"version": version,
		},
	});
	match serde_json::to_string_pretty(&report) {
		Ok(json) => {
			let _ = writeln!(out, "{json}");
			0
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: failed to serialize status: {error}");
			1
		},
	}
}

fn cmd_install(opts: &InstallOptions, out: &mut dyn std::io::Write) -> i32 {
	let script = find_download_script(opts.plugin_dir.as_deref());
	if opts.no_download {
		let _ = writeln!(out, "auth-hermes-cloudflare: {}", no_download_remediation(script.as_deref()));
		return 0;
	}
	let Some(script) = script else {
		let _ = writeln!(
			out,
			"auth-hermes-cloudflare: download.sh not found - pass --plugin-dir <dir>, run from a checkout of {PLUGIN_NAME}, or install the plugin first"
		);
		return 1;
	};
	let mut command = Command::new(&script);
	if let Some(bin) = &opts.binary {
		command.env("AUTH_CLOUDFLARE_BIN", bin);
	}
	match command.status() {
		Ok(status) if status.success() => {
			let _ = writeln!(out, "auth-hermes-cloudflare: installed via {script:?}");
			let bin = opts.binary.as_deref().map(Path::to_path_buf).or_else(locate_binary);
			match bin {
				Some(bin) => post_install_doctor(&bin, out),
				None => {
					let _ = writeln!(
						out,
						"auth-hermes-cloudflare: installed but could not locate the binary for a post-install doctor"
					);
				},
			}
			0
		},
		Ok(status) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: {script:?} exited with {status}");
			1
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: cannot run {script:?}: {error}");
			1
		},
	}
}

/// No-cost post-install check: run the installed binary's `doctor --format
/// json` and print a compact status summary. Informational only - never
/// changes the install's exit code.
fn post_install_doctor(bin: &Path, out: &mut dyn std::io::Write) {
	match run_doctor(bin) {
		Ok((_code, _, Some(value))) => {
			let status = value.get("status").and_then(|value| value.as_str()).unwrap_or("unknown");
			let account = value
				.pointer("/account_id/configured")
				.and_then(|value| value.as_bool())
				.unwrap_or(false);
			let token = value
				.pointer("/api_token/configured")
				.and_then(|value| value.as_bool())
				.unwrap_or(false);
			let cache = value
				.pointer("/catalog_cache/present")
				.and_then(|value| value.as_bool())
				.unwrap_or(false);
			let _ = writeln!(
				out,
				"auth-hermes-cloudflare: post-install doctor: status={status} account_configured={account} token_configured={token} catalog_cache_present={cache}"
			);
		},
		Ok((code, _, None)) => {
			let _ = writeln!(
				out,
				"auth-hermes-cloudflare: post-install doctor produced no JSON (exit code {code})"
			);
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: post-install doctor skipped: {error}");
		},
	}
}

fn cmd_doctor(out: &mut dyn std::io::Write) -> i32 {
	let Some(bin) = locate_binary() else {
		let _ = writeln!(
			out,
			"auth-hermes-cloudflare: {BINARY_NAME} binary not found - run 'auth-hermes-cloudflare install' first, or set AUTH_CLOUDFLARE_BIN to an existing binary"
		);
		return 1;
	};
	match run_doctor(&bin) {
		Ok((code, raw, _)) => {
			let trimmed = raw.trim();
			if trimmed.is_empty() {
				let _ = writeln!(
					out,
					"auth-hermes-cloudflare: {BINARY_NAME} doctor produced no output (exit code {code})"
				);
			} else {
				let _ = writeln!(out, "{trimmed}");
			}
			code
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: {error}");
			1
		},
	}
}

/// Outcome of linking the managed binary to a source path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkOutcome {
	Symlinked,
	Copied,
}

#[cfg(unix)]
fn create_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
	std::os::unix::fs::symlink(source, target)
}

#[cfg(not(unix))]
fn create_symlink(_source: &Path, _target: &Path) -> std::io::Result<()> {
	Err(std::io::Error::new(
		std::io::ErrorKind::Unsupported,
		"symlinks are not supported on this platform",
	))
}

/// Symlink `managed` to `source`, falling back to a copy when symlinking is
/// unavailable. Pure of environment, only touches the two given paths.
fn link_binary(source: &Path, managed: &Path) -> Result<LinkOutcome, String> {
	match create_symlink(source, managed) {
		Ok(()) => Ok(LinkOutcome::Symlinked),
		Err(symlink_error) => std::fs::copy(source, managed)
			.map(|_| LinkOutcome::Copied)
			.map_err(|copy_error| format!("symlink failed ({symlink_error}) and copy failed ({copy_error})")),
	}
}

fn cmd_link(source: &Path, out: &mut dyn std::io::Write) -> i32 {
	if !source.is_file() {
		let _ = writeln!(out, "auth-hermes-cloudflare: link source not found (not a file): {source:?}");
		return 1;
	}
	let managed = managed_binary_path();
	if let Some(parent) = managed.parent() {
		if let Err(error) = std::fs::create_dir_all(parent) {
			let _ = writeln!(out, "auth-hermes-cloudflare: cannot create {parent:?}: {error}");
			return 1;
		}
	}
	// Replace any existing entry (file or symlink) so linking is idempotent.
	if std::fs::symlink_metadata(&managed).is_ok() {
		if let Err(error) = std::fs::remove_file(&managed) {
			let _ = writeln!(out, "auth-hermes-cloudflare: cannot replace {managed:?}: {error}");
			return 1;
		}
	}
	match link_binary(source, &managed) {
		Ok(LinkOutcome::Symlinked) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: linked {managed:?} -> {source:?}");
			0
		},
		Ok(LinkOutcome::Copied) => {
			let _ = writeln!(
				out,
				"auth-hermes-cloudflare: copied {source:?} -> {managed:?} (symlink unavailable)"
			);
			0
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: cannot link {managed:?} to {source:?}: {error}");
			1
		},
	}
}

fn cmd_unlink(out: &mut dyn std::io::Write) -> i32 {
	let managed = managed_binary_path();
	if !managed.is_file() {
		let _ = writeln!(out, "auth-hermes-cloudflare: nothing to remove at {managed:?}");
		return 0;
	}
	match std::fs::remove_file(&managed) {
		Ok(()) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: removed {managed:?}");
			0
		},
		Err(error) => {
			let _ = writeln!(out, "auth-hermes-cloudflare: cannot remove {managed:?}: {error}");
			1
		},
	}
}

fn print_help(out: &mut dyn std::io::Write) {
	let _ = writeln!(
		out,
		"auth-hermes-cloudflare - optional Hermes plugin / {BINARY_NAME} binary utility"
	);
	let _ = writeln!(out);
	let _ = writeln!(out, "Usage:");
	let _ = writeln!(
		out,
		"  auth-hermes-cloudflare status                 JSON plugin + binary state"
	);
	let _ = writeln!(
		out,
		"  auth-hermes-cloudflare install [options]      run download.sh, then doctor"
	);
	let _ = writeln!(out, "  auth-hermes-cloudflare upgrade [options]      alias of install");
	let _ = writeln!(
		out,
		"  auth-hermes-cloudflare doctor                 run '{BINARY_NAME} doctor --format json'"
	);
	let _ = writeln!(
		out,
		"  auth-hermes-cloudflare uninstall              remove ~/.hermes/bin/{BINARY_NAME} (alias: unlink)"
	);
	let _ = writeln!(
		out,
		"  auth-hermes-cloudflare link --source <path>   symlink/copy ~/.hermes/bin/{BINARY_NAME} to <path>"
	);
	let _ = writeln!(out);
	let _ = writeln!(out, "Install options:");
	let _ = writeln!(out, "  --plugin-dir <dir>   find download.sh under <dir>");
	let _ = writeln!(
		out,
		"  --binary <path>      install to <path> (sets AUTH_CLOUDFLARE_BIN for download.sh)"
	);
	let _ = writeln!(
		out,
		"  --no-download        print manual install instructions instead of download.sh"
	);
	let _ = writeln!(out);
	let _ = writeln!(
		out,
		"Exit codes: 0 ok, 1 operational error; doctor forwards the engine's exit code."
	);
}

fn main() {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let code = match parse_args(&args) {
		Ok(CliCommand::Status) => cmd_status(&mut std::io::stdout()),
		Ok(CliCommand::Install(opts)) | Ok(CliCommand::Upgrade(opts)) => cmd_install(&opts, &mut std::io::stdout()),
		Ok(CliCommand::Doctor) => cmd_doctor(&mut std::io::stdout()),
		Ok(CliCommand::Uninstall) => cmd_unlink(&mut std::io::stdout()),
		Ok(CliCommand::Link { source }) => cmd_link(&source, &mut std::io::stdout()),
		Ok(CliCommand::Help) => {
			print_help(&mut std::io::stdout());
			0
		},
		Err(message) => {
			eprintln!("auth-hermes-cloudflare: {message} (try --help)");
			1
		},
	};
	std::process::exit(code);
}

#[cfg(test)]
mod tests {
	use super::*;

	fn args(items: &[&str]) -> Vec<String> {
		items.iter().map(|item| item.to_string()).collect()
	}

	#[test]
	fn managed_binary_path_is_hermes_bin() {
		let path = managed_binary_path();
		assert!(path.ends_with(".hermes/bin/auth-cloudflare"));
	}

	#[test]
	fn locate_binary_falls_back_to_managed_path() {
		// With AUTH_CLOUDFLARE_BIN unset and no PATH hit, the locator must
		// still check ~/.hermes/bin (and not panic when it is absent).
		let found = locate_binary();
		if let Some(path) = &found {
			assert!(path.is_file(), "located binary must exist: {path:?}");
		}
	}

	#[test]
	fn parse_no_args_is_status() {
		assert_eq!(parse_args(&[]), Ok(CliCommand::Status));
	}

	#[test]
	fn parse_status() {
		assert_eq!(parse_args(&args(&["status"])), Ok(CliCommand::Status));
	}

	#[test]
	fn parse_status_rejects_extra_args() {
		assert!(parse_args(&args(&["status", "x"])).is_err());
	}

	#[test]
	fn parse_install_defaults() {
		assert_eq!(
			parse_args(&args(&["install"])),
			Ok(CliCommand::Install(InstallOptions::default()))
		);
	}

	#[test]
	fn parse_install_flags() {
		assert_eq!(
			parse_args(&args(&[
				"install",
				"--plugin-dir",
				"/tmp/p",
				"--binary",
				"/tmp/b",
				"--no-download",
			])),
			Ok(CliCommand::Install(InstallOptions {
				plugin_dir: Some(PathBuf::from("/tmp/p")),
				binary: Some(PathBuf::from("/tmp/b")),
				no_download: true,
			}))
		);
	}

	#[test]
	fn parse_upgrade_is_install_alias() {
		assert_eq!(
			parse_args(&args(&["upgrade", "--no-download"])),
			Ok(CliCommand::Upgrade(InstallOptions {
				no_download: true,
				..InstallOptions::default()
			}))
		);
	}

	#[test]
	fn parse_doctor() {
		assert_eq!(parse_args(&args(&["doctor"])), Ok(CliCommand::Doctor));
	}

	#[test]
	fn parse_uninstall_and_unlink_alias() {
		assert_eq!(parse_args(&args(&["uninstall"])), Ok(CliCommand::Uninstall));
		assert_eq!(parse_args(&args(&["unlink"])), Ok(CliCommand::Uninstall));
	}

	#[test]
	fn parse_link_requires_source() {
		assert!(parse_args(&args(&["link"])).is_err());
		assert!(parse_args(&args(&["link", "--other", "x"])).is_err());
		assert!(parse_args(&args(&["link", "--source"])).is_err());
	}

	#[test]
	fn parse_link_with_source() {
		assert_eq!(
			parse_args(&args(&["link", "--source", "/opt/auth-cloudflare"])),
			Ok(CliCommand::Link { source: PathBuf::from("/opt/auth-cloudflare") })
		);
	}

	#[test]
	fn parse_help_forms() {
		assert_eq!(parse_args(&args(&["--help"])), Ok(CliCommand::Help));
		assert_eq!(parse_args(&args(&["-h"])), Ok(CliCommand::Help));
		assert_eq!(parse_args(&args(&["help"])), Ok(CliCommand::Help));
	}

	#[test]
	fn parse_unknown_command_errors() {
		assert!(parse_args(&args(&["frobnicate"])).is_err());
	}

	#[test]
	fn parse_install_requires_option_values() {
		assert!(parse_args(&args(&["install", "--plugin-dir"])).is_err());
		assert!(parse_args(&args(&["install", "--binary"])).is_err());
		assert!(parse_args(&args(&["install", "--bogus"])).is_err());
	}

	#[test]
	fn no_download_remediation_is_actionable() {
		let text = no_download_remediation(Some(Path::new("/p/download.sh")));
		assert!(text.contains("cargo install auth-cloudflare"), "{text}");
		assert!(text.contains("link --source"), "{text}");
		assert!(text.contains("doctor"), "{text}");
	}

	#[test]
	fn help_mentions_every_command() {
		let mut buf = Vec::new();
		print_help(&mut buf);
		let text = String::from_utf8(buf).unwrap();
		for command in [
			"status",
			"install",
			"upgrade",
			"doctor",
			"uninstall",
			"link",
			"--plugin-dir",
			"--binary",
			"--no-download",
		] {
			assert!(text.contains(command), "help missing {command:?}: {text}");
		}
	}

	#[test]
	fn link_binary_symlinks_or_copies_to_a_working_binary() {
		let nanos = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_nanos();
		let dir = std::env::temp_dir().join(format!("ahc-link-test-{}-{nanos}", std::process::id()));
		let source = dir.join("source-bin");
		let managed = dir.join("managed-bin");
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(&source, b"#!/bin/sh\necho ok\n").unwrap();
		let outcome = link_binary(&source, &managed).unwrap();
		assert!(matches!(outcome, LinkOutcome::Symlinked | LinkOutcome::Copied));
		assert_eq!(std::fs::read_to_string(&managed).unwrap(), "#!/bin/sh\necho ok\n");
		let _ = std::fs::remove_dir_all(&dir);
	}
}
