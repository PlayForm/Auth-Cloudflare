#!/usr/bin/env python3
"""Regenerate catalog fixtures and plugin defaults from a Workers AI catalog snapshot.

Usage:
  python3 scripts/regenerate-fixtures.py --snapshot <path>   # offline, from a recorded snapshot
  python3 scripts/regenerate-fixtures.py --live              # run the auth-cloudflare binary for a fresh snapshot
  python3 scripts/regenerate-fixtures.py --snapshot <path> --dry-run   # print what would change without writing

Owned outputs (this script touches ONLY these):
  - crates/auth-cloudflare/fixtures/models/<id>.json      (6 files: pricing + context_length from live)
  - crates/auth-cloudflare/src/catalog.rs                 (FALLBACK_MODELS / EXPERIMENTAL_MODELS /
                                                           HIDDEN_MODELS / VISION_CONFIRMED only)
  - plugins/auth-hermes-cloudflare/fixtures/workers_ai_catalog.json
  - plugins/auth-hermes-cloudflare/fixtures/plugin_defaults.json

Stdlib only. Never prints API tokens; the binary redacts its own output.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURE_DIR = os.path.join(REPO_ROOT, "crates", "auth-cloudflare", "fixtures", "models")
CATALOG_RS = os.path.join(REPO_ROOT, "crates", "auth-cloudflare", "src", "catalog.rs")
WORKERS_AI_CATALOG = os.path.join(
    REPO_ROOT, "plugins", "auth-hermes-cloudflare", "fixtures", "workers_ai_catalog.json"
)
PLUGIN_DEFAULTS = os.path.join(
    REPO_ROOT, "plugins", "auth-hermes-cloudflare", "fixtures", "plugin_defaults.json"
)

DEFAULT_MODEL = "@cf/deepseek-ai/deepseek-v4-flash-0731"

# The six bundled Rust model fixtures (fixture filename = id after "@cf/<org>/").
SIX_MODEL_IDS = [
    "@cf/deepseek-ai/deepseek-v4-flash-0731",
    "@cf/deepseek-ai/deepseek-v4-pro-0813",
    "@cf/zai-org/glm-5.3-flash",
    "@cf/openai/gpt-oss-120b",
    "@cf/moonshotai/kimi-k2.7-code",
    "@cf/meta/llama-guard-3-8b",
]

# The four OpenRouter-shape records in the plugin fixture.
OPENROUTER_FOUR_IDS = [
    "@cf/deepseek-ai/deepseek-v4-flash-0731",
    "@cf/zai-org/glm-5.3-flash",
    "@cf/moonshotai/kimi-k2.7-code",
    "@cf/meta/llama-guard-3-8b",
]

# Curated policy opinions that must survive regeneration (mirrors
# plugins/auth-hermes-cloudflare/__init__.py MODEL_POLICY).
CURATED_RANKS = {
    "@cf/deepseek-ai/deepseek-v4-flash-0731": 10,
    "@cf/deepseek-ai/deepseek-v4-pro-0813": 20,
    "@cf/moonshotai/kimi-k2.7-code": 30,
}
CURATED_REASONS = {
    "@cf/deepseek-ai/deepseek-v4-flash-0731": "Validated development default.",
    "@cf/deepseek-ai/deepseek-v4-pro-0813": "Premium reasoning.",
    "@cf/moonshotai/kimi-k2.7-code": "Premium coding.",
    "@cf/zai-org/glm-5.3-flash": "Observed delivery failures; requires passing conformance suite.",
    "@cf/meta/llama-guard-3-8b": "Safety classifier.",
}

# The plugin's current primary-agent allow list (order is policy rank order).
PRIMARY_AGENT_MODELS = [
    "@cf/deepseek-ai/deepseek-v4-flash-0731",
    "@cf/deepseek-ai/deepseek-v4-pro-0813",
    "@cf/moonshotai/kimi-k2.7-code",
    "@cf/openai/gpt-oss-120b",
    "@cf/openai/gpt-oss-20b",
    "@cf/qwen/qwen3-30b-a3b-fp8",
    "@cf/qwen/qwen3.8-27b",
    "@cf/zai-org/glm-5.3",
    "@cf/zai-org/glm-5.3-flash",
]


def fmt_price(v):
    """Per-token string rule: ("%.10f" % v).rstrip("0").rstrip(".")."""
    return ("%.10f" % v).rstrip("0").rstrip(".")


def per_token_string(per_million):
    """Convert a per-million float to the fixture's per-token string."""
    return fmt_price(per_million / 1_000_000.0)


def per_million_string(per_million):
    """Convert a per-million float to the OpenRouter fixture's per-M string."""
    return fmt_price(per_million)


def fixture_path(model_id):
    short = model_id.split("/")[-1]
    return os.path.join(FIXTURE_DIR, short + ".json")


def load_snapshot(path_or_live):
    """Load the snapshot: either a JSON file path or the live CLI stdout."""
    if path_or_live:
        with open(path_or_live, "r", encoding="utf-8") as fh:
            return json.load(fh)
    binary = os.environ.get("AUTH_CLOUDFLARE_BIN") or shutil.which("auth-cloudflare")
    if not binary:
        sys.exit("ERROR: --live requires the auth-cloudflare binary (PATH or AUTH_CLOUDFLARE_BIN)")
    proc = subprocess.run(
        [binary, "catalog", "get", "--format", "json"],
        check=False,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        sys.exit("ERROR: auth-cloudflare catalog get failed (exit %d): %s" % (proc.returncode, proc.stderr.strip()))
    return json.loads(proc.stdout)


def snapshot_index(snapshot):
    models = snapshot["models"]
    by_id = {}
    for record in models:
        by_id[record["id"]] = record
    return by_id


def ordered_status_ids(by_id, status):
    return [mid for mid in by_id if by_id[mid]["status"] == status]


def derive_fallback(by_id):
    """Non-hidden models: recommended (default first), available, experimental - snapshot order."""
    recommended = ordered_status_ids(by_id, "recommended")
    recommended = [DEFAULT_MODEL] + [m for m in recommended if m != DEFAULT_MODEL]
    available = ordered_status_ids(by_id, "available")
    experimental = ordered_status_ids(by_id, "experimental")
    return recommended + available + experimental


# ---------------------------------------------------------------- fixtures

def regenerate_six_fixtures(by_id, dry_run, notes):
    changed = []
    for model_id in SIX_MODEL_IDS:
        path = fixture_path(model_id)
        record = by_id.get(model_id)
        if record is None:
            notes.append("fixture %s: id ABSENT from snapshot - file left unchanged" % model_id)
            continue
        with open(path, "r", encoding="utf-8") as fh:
            data = json.load(fh)
        pricing = record["pricing_per_million"]
        new_pricing = {
            "prompt": per_token_string(pricing["input"]),
            "completion": per_token_string(pricing["output"]),
            "request": "0",
        }
        # Preserve curated fields verbatim: name, vendor_name, description, created.
        data["context_length"] = record["context_tokens"]
        data["pricing"] = new_pricing
        payload = json.dumps(data, indent=2, ensure_ascii=False) + "\n"
        current = ""
        if os.path.exists(path):
            with open(path, "r", encoding="utf-8") as fh:
                current = fh.read()
        if payload == current:
            continue
        changed.append((path, "pricing/context_length from snapshot (%s/%s)" % (
            new_pricing["prompt"], new_pricing["completion"])))
        if not dry_run:
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(payload)
    return changed


# ---------------------------------------------------------------- catalog.rs

def update_status_lists(src, by_id, dry_run, notes):
    fallback = derive_fallback(by_id)

    def members_in_current_order(const_name, status):
        """Members of *const_name* in the file's current order, filtered to the snapshot's *status*."""
        m = re.search(r"pub const %s: &\[&str\] = &\[([^\]]*)\];" % const_name, src)
        if not m:
            return []
        current = [x.strip().strip('"') for x in m.group(1).split(",") if x.strip()]
        return [mid for mid in current if mid in by_id and by_id[mid]["status"] == status]

    experimental = members_in_current_order("EXPERIMENTAL_MODELS", "experimental")
    hidden = members_in_current_order("HIDDEN_MODELS", "hidden")
    vision = re.search(r"pub const VISION_CONFIRMED: &\[&str\] = &\[([^\]]*)\];", src)
    vision_members = [m.strip().strip('"') for m in vision.group(1).split(",") if m.strip()]
    vision_keep = [mid for mid in vision_members if mid in by_id]
    dropped = [mid for mid in vision_members if mid not in by_id]

    def fmt_array(ids, indent):
        return "".join("\t" + json.dumps(mid) + ",\n" for mid in ids) if indent else ", ".join(json.dumps(mid) for mid in ids)

    src_new = re.sub(
        r"(pub const FALLBACK_MODELS: &\[&str\] = &\[\n)(.*?)(\n\];)",
        lambda m: m.group(1) + fmt_array(fallback, True) + "];",
        src,
        flags=re.DOTALL,
    )
    src_new = re.sub(
        r"(pub const EXPERIMENTAL_MODELS: &\[&str\] = &\[)([^\]]*)(\];)",
        lambda m: m.group(1) + fmt_array(experimental, False) + m.group(3),
        src_new,
    )
    src_new = re.sub(
        r"(pub const HIDDEN_MODELS: &\[&str\] = &\[)([^\]]*)(\];)",
        lambda m: m.group(1) + fmt_array(hidden, False) + m.group(3),
        src_new,
    )
    src_new = re.sub(
        r"(pub const VISION_CONFIRMED: &\[&str\] = &\[)([^\]]*)(\];)",
        lambda m: m.group(1) + fmt_array(vision_keep, False) + m.group(3),
        src_new,
    )

    if dropped:
        notes.append("VISION_CONFIRMED: dropped %s (absent from snapshot)" % ", ".join(dropped))

    if src_new == src:
        return []
    if not dry_run:
        with open(CATALOG_RS, "w", encoding="utf-8") as fh:
            fh.write(src_new)
    return [(CATALOG_RS, "status lists regenerated (fallback=%d experimental=%d hidden=%d)" % (
        len(fallback), len(experimental), len(hidden)))]


def report_table_ids_absent(src, by_id, notes):
    """Report-only: ids in MODEL_FAMILIES_BY_ID / REASONING_EFFORTS_BY_MODEL absent from the snapshot."""
    for table in ("MODEL_FAMILIES_BY_ID", "REASONING_EFFORTS_BY_MODEL"):
        m = re.search(r"pub const %s: &\[\(&str, .*?\] = &\[\n(.*?)\n\];" % table, src, flags=re.DOTALL)
        if not m:
            continue
        ids = re.findall(r'\(\s*"([^"]+)"', m.group(1))
        absent = [mid for mid in ids if mid not in by_id]
        if absent:
            notes.append("REPORT-ONLY: %s references ids absent from the snapshot: %s" % (table, ", ".join(absent)))
        else:
            notes.append("REPORT-ONLY: %s: all %d referenced ids present in the snapshot" % (table, len(ids)))


# ---------------------------------------------------------------- plugin fixtures

def regenerate_workers_ai_catalog(by_id, dry_run, notes):
    with open(WORKERS_AI_CATALOG, "r", encoding="utf-8") as fh:
        data = json.load(fh)
    changed = []
    for item in data["data"]:
        model_id = item["id"]
        record = by_id.get(model_id)
        if record is None:
            notes.append("workers_ai_catalog %s: id ABSENT from snapshot - record left unchanged" % model_id)
            continue
        pricing = record["pricing_per_million"]
        item["pricing"]["prompt"] = per_million_string(pricing["input"])
        item["pricing"]["completion"] = per_million_string(pricing["output"])
        item["context_length"] = record["context_tokens"]
    payload = json.dumps(data, indent=2, ensure_ascii=False) + "\n"
    with open(WORKERS_AI_CATALOG, "r", encoding="utf-8") as fh:
        current = fh.read()
    if payload != current:
        changed.append((WORKERS_AI_CATALOG, "pricing/context_length updated from snapshot"))
        if not dry_run:
            with open(WORKERS_AI_CATALOG, "w", encoding="utf-8") as fh:
                fh.write(payload)
    return changed


def build_plugin_defaults(snapshot, by_id, notes):
    model_policy = {}
    next_available_rank = 100
    next_recommended_rank = 40
    for model_id, record in by_id.items():
        status = record["status"]
        if status == "recommended":
            rank = CURATED_RANKS.get(model_id)
            if rank is None:
                rank = next_recommended_rank
                next_recommended_rank += 10
            default = model_id == DEFAULT_MODEL
        elif status == "available":
            rank = next_available_rank
            next_available_rank += 10
            default = False
        elif status == "experimental":
            rank = 900
            default = False
        else:  # hidden
            rank = None
            default = False
        if status == "hidden":
            reason = CURATED_REASONS.get(model_id, "Safety classifier - not selectable.")
        elif status == "experimental":
            reason = CURATED_REASONS.get(model_id, "Experimental - requires conformance.")
        else:
            reason = CURATED_REASONS.get(model_id, "Available on Cloudflare Workers AI.")
        entry = {
            "status": status,
            "rank": rank,
            "default": default,
            "primary_agent_eligible": bool(record.get("primary_agent_eligible", False)),
            "reason": reason,
        }
        model_policy[model_id] = entry

    primary = [mid for mid in PRIMARY_AGENT_MODELS if mid in by_id]
    absent = [mid for mid in PRIMARY_AGENT_MODELS if mid not in by_id]
    if absent:
        notes.append("primary_agent_models: dropped %s (absent from snapshot)" % ", ".join(absent))

    return {
        "schema_version": 1,
        "generated_at": snapshot.get("fetched_at", ""),
        "source": "cloudflare-workers-ai",
        "default_model": DEFAULT_MODEL,
        "model_policy": model_policy,
        "primary_agent_models": primary,
        "fallback_models": derive_fallback(by_id),
    }


def regenerate_plugin_defaults(snapshot, by_id, dry_run, notes):
    payload = json.dumps(build_plugin_defaults(snapshot, by_id, notes), indent=2, ensure_ascii=False) + "\n"
    current = ""
    if os.path.exists(PLUGIN_DEFAULTS):
        with open(PLUGIN_DEFAULTS, "r", encoding="utf-8") as fh:
            current = fh.read()
    if payload == current:
        return []
    if not dry_run:
        with open(PLUGIN_DEFAULTS, "w", encoding="utf-8") as fh:
            fh.write(payload)
    return [(PLUGIN_DEFAULTS, "new plugin defaults (schema_version=1, %d policy entries)" % len(by_id))]


# ---------------------------------------------------------------- main

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--snapshot", metavar="PATH", help="recorded snapshot JSON file (offline)")
    group.add_argument("--live", action="store_true", help="fetch a fresh snapshot via the auth-cloudflare binary")
    parser.add_argument("--dry-run", action="store_true", help="print what would change without writing")
    args = parser.parse_args()

    snapshot = load_snapshot(args.snapshot)
    by_id = snapshot_index(snapshot)
    notes = []

    with open(CATALOG_RS, "r", encoding="utf-8") as fh:
        catalog_src = fh.read()

    changes = []
    changes += regenerate_six_fixtures(by_id, args.dry_run, notes)
    changes += update_status_lists(catalog_src, by_id, args.dry_run, notes)
    changes += regenerate_workers_ai_catalog(by_id, args.dry_run, notes)
    changes += regenerate_plugin_defaults(snapshot, by_id, args.dry_run, notes)
    report_table_ids_absent(catalog_src, by_id, notes)

    if not changes:
        print("No changes - all fixtures and status lists already match the snapshot.")
    for path, why in changes:
        print(("WOULD WRITE" if args.dry_run else "WROTE") + " %s (%s)" % (os.path.relpath(path, REPO_ROOT), why))
    for note in notes:
        print("NOTE: " + note)


if __name__ == "__main__":
    main()