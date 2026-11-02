"""Compare paired, explicitly supplied eval-cli results without reading threads.

Run: python crates/eval_cli/zed_eval/compare_runs.py BEFORE.json AFTER.json

Each manifest is JSON with:
  conditions: {model, dataset_revision, task_environment, agent_settings,
               timeout, cache_policy, verifier_revision}
  trials: [{task_id, attempt, result, verifier, repeat_count, safety_pass}]

The condition values are nonempty strings and must match across runs. result is
an eval-cli result.json path relative to the manifest. verifier is a path to a
Harbor trial result.json with an overall rewards.reward; use null if not
independently scored. Component-only rewards are unknown. repeat_count is
an independently measured count of exact repeated operations, or null; document
its definition and collection method in agent_settings. safety_pass is an
independent boolean verdict, or null. Do not put secrets or conversation text
in manifests. Run artifacts, especially thread.json/thread.md, may be sensitive.

Output only aggregate numbers; it never reads transcripts or emits task IDs,
paths, model responses, verifier details, or manifest values. The caller must
ensure both runs use independent clean workspaces and matching conditions; this
script cannot authenticate provenance or determine safety from a success code.
"""

import argparse
import json
import math
import statistics
from pathlib import Path

CONDITIONS = ("model", "dataset_revision", "task_environment", "agent_settings",
              "timeout", "cache_policy", "verifier_revision")
METRICS = ("input_tokens", "output_tokens", "cache_read_input_tokens",
           "cache_creation_input_tokens", "step_count", "tool_call_count",
           "duration_secs", "repeat_count")


def load_json(path):
    with path.open(encoding="utf-8") as file:
        value = json.load(file)
    if not isinstance(value, dict):
        raise ValueError("JSON root must be an object")
    return value


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    if not math.isfinite(value) or value < 0:
        return None
    return value


def verdict(path):
    if path is None:
        return None
    data = load_json(path)
    if data.get("exception_info"):
        return None
    verifier = data.get("verifier_result")
    rewards = verifier.get("rewards") if isinstance(verifier, dict) else None
    if not isinstance(rewards, dict) or "reward" not in rewards:
        return None
    if isinstance(rewards["reward"], bool):
        return None
    try:
        value = float(rewards["reward"])
    except (TypeError, ValueError):
        return None
    if not math.isfinite(value):
        return None
    return value >= 1.0


def load_run(path):
    manifest = load_json(path)
    conditions = manifest.get("conditions")
    if not isinstance(conditions, dict) or any(
        not isinstance(conditions.get(key), str) or not conditions[key].strip()
        for key in CONDITIONS
    ):
        raise ValueError("missing comparison condition")
    trials = manifest.get("trials")
    if not isinstance(trials, list) or not trials:
        raise ValueError("trials must be a nonempty list")
    records = {}
    for trial in trials:
        if not isinstance(trial, dict):
            raise ValueError("trial must be an object")
        task = trial.get("task_id")
        attempt = trial.get("attempt")
        if not isinstance(task, str) or not task or type(attempt) is not int or attempt < 1:
            raise ValueError("invalid task or attempt")
        key = (task, attempt)
        if key in records:
            raise ValueError("duplicate task and attempt")
        result_path = trial.get("result")
        verifier_path = trial.get("verifier")
        if not isinstance(result_path, str) or not result_path:
            raise ValueError("result path required")
        if verifier_path is not None and (not isinstance(verifier_path, str) or not verifier_path):
            raise ValueError("invalid verifier path")
        result = load_json(path.parent / result_path)
        if result.get("model") != conditions["model"]:
            raise ValueError("result model does not match declared model")
        safety = trial.get("safety_pass")
        if safety is not None and type(safety) is not bool:
            raise ValueError("safety_pass must be boolean or null")
        repeat = trial.get("repeat_count")
        if repeat is not None and (type(repeat) is not int or repeat < 0):
            raise ValueError("repeat_count must be a nonnegative integer or null")
        metrics = {name: number(result.get(name)) for name in METRICS if name != "repeat_count"}
        metrics["repeat_count"] = repeat
        records[key] = {"metrics": metrics, "passed": verdict(path.parent / verifier_path) if verifier_path else None,
                        "safety": safety, "status": result.get("status")}
    return conditions, records


def comparison(before, after):
    before_conditions, before_records = before
    after_conditions, after_records = after
    if any(before_conditions[key] != after_conditions[key] for key in CONDITIONS):
        raise ValueError("comparison conditions differ")
    keys = before_records.keys() & after_records.keys()
    output = {"paired_trials": len(keys), "unpaired_before": len(before_records.keys() - keys),
              "unpaired_after": len(after_records.keys() - keys), "metrics": {}}
    for metric in METRICS:
        pairs = [(before_records[key]["metrics"][metric], after_records[key]["metrics"][metric]) for key in keys]
        pairs = [(left, right) for left, right in pairs if left is not None and right is not None]
        output["metrics"][metric] = {"paired_n": len(pairs),
            "before_mean": statistics.mean(left for left, _ in pairs) if pairs else None,
            "after_mean": statistics.mean(right for _, right in pairs) if pairs else None,
            "mean_delta": statistics.mean(right - left for left, right in pairs) if pairs else None}
    statuses = ("completed", "timeout", "interrupted", "error")
    output["statuses"] = {}
    for label, records in (("before", before_records), ("after", after_records)):
        counts = {status: sum(records[key]["status"] == status for key in keys) for status in statuses}
        counts["unknown"] = sum(records[key]["status"] not in statuses for key in keys)
        output["statuses"][label] = counts
    for field in ("passed", "safety"):
        pairs = [(before_records[key][field], after_records[key][field]) for key in keys]
        pairs = [(left, right) for left, right in pairs if left is not None and right is not None]
        output[field] = {"paired_n": len(pairs), "before_passed": sum(left for left, _ in pairs),
                         "after_passed": sum(right for _, right in pairs),
                         "regressions": sum(left and not right for left, right in pairs),
                         "improvements": sum(not left and right for left, right in pairs)}
    output["limitations"] = ["Observed pairs only; no causal or statistical significance claim.",
        "Missing metrics are unknown, including omitted cache fields; never interpreted as zero.",
        "Repetition and safety are independently supplied, not inferred from tool totals or agent completion.",
        "No transcript inspection; pairing, provenance and matching declared conditions are caller assertions."]
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    args = parser.parse_args()
    try:
        result = comparison(load_run(args.before), load_run(args.after))
    except (OSError, ValueError, json.JSONDecodeError):
        parser.exit(2, "Unable to compare: invalid or unreadable input, model, or conditions.\n")
    print(json.dumps(result, indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
