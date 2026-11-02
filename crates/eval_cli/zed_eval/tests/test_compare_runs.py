import json
import tempfile
import unittest
from pathlib import Path

from zed_eval import compare_runs


class CompareRunsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.conditions = {key: "same" for key in compare_runs.CONDITIONS}
        self.conditions["model"] = "model"

    def write(self, name, data):
        path = self.root / name
        path.write_text(json.dumps(data))
        return path

    def run_data(self, name, result, reward, **extra):
        self.write(name + "-result.json", {"model": "model", "status": "completed", **result})
        self.write(name + "-verifier.json", {"verifier_result": {"rewards": reward}})
        manifest = {"conditions": self.conditions, "trials": [{"task_id": "private task",
            "attempt": 1, "result": name + "-result.json", "verifier": name + "-verifier.json", **extra}]}
        return compare_runs.load_run(self.write(name + ".json", manifest))

    def test_paired_metrics_missing_cache_and_regressions(self):
        before = self.run_data("before", {"input_tokens": 100, "step_count": 3}, {"reward": 1}, repeat_count=2, safety_pass=True)
        after = self.run_data("after", {"input_tokens": 80, "step_count": 5, "cache_read_input_tokens": 0}, {"reward": 0}, repeat_count=1, safety_pass=False)
        comparison = compare_runs.comparison(before, after)
        self.assertEqual(comparison["metrics"]["input_tokens"]["mean_delta"], -20)
        self.assertEqual(comparison["metrics"]["cache_read_input_tokens"]["paired_n"], 0)
        self.assertEqual(comparison["metrics"]["repeat_count"]["mean_delta"], -1)
        self.assertEqual(comparison["passed"]["regressions"], 1)
        self.assertEqual(comparison["safety"]["regressions"], 1)
        self.assertNotIn("private task", json.dumps(comparison))

    def test_mismatched_conditions_and_model_rejected(self):
        before = self.run_data("before", {}, {"reward": 1})
        after = self.run_data("after", {}, {"reward": 1})
        after[0]["cache_policy"] = "different"
        with self.assertRaises(ValueError):
            compare_runs.comparison(before, after)
        self.write("wrong.json", {"model": "different"})
        manifest = {"conditions": self.conditions, "trials": [{"task_id": "a", "attempt": 1, "result": "wrong.json"}]}
        with self.assertRaises(ValueError):
            compare_runs.load_run(self.write("manifest.json", manifest))

    def test_unpaired_and_unknown_verdict(self):
        before = self.run_data("before", {"duration_secs": 1}, {"reward": 1})
        after = self.run_data("after", {"duration_secs": 2}, {"reward": 1})
        after[1][("other", 1)] = after[1].pop(("private task", 1))
        result = compare_runs.comparison(before, after)
        self.assertEqual((result["paired_trials"], result["unpaired_before"], result["unpaired_after"]), (0, 1, 1))
        self.assertIsNone(result["metrics"]["duration_secs"]["mean_delta"])
        self.write("exception.json", {"exception_info": {"exception_type": "AgentTimeoutError"}, "verifier_result": {"rewards": {"reward": 1}}})
        self.assertIsNone(compare_runs.verdict(self.root / "exception.json"))

    def test_component_rewards_do_not_imply_overall_success(self):
        path = self.write("components.json", {"verifier_result": {"rewards": {"tests_reward": 1, "must_have_pass": 0}}})
        self.assertIsNone(compare_runs.verdict(path))
        path = self.write("overall.json", {"verifier_result": {"rewards": {"reward": 0, "tests_reward": 1}}})
        self.assertFalse(compare_runs.verdict(path))
        path = self.write("boolean.json", {"verifier_result": {"rewards": {"reward": True}}})
        self.assertIsNone(compare_runs.verdict(path))

    def test_duplicate_and_invalid_repeat_rejected(self):
        self.write("result.json", {"model": "model"})
        trial = {"task_id": "a", "attempt": 1, "result": "result.json"}
        manifest = {"conditions": self.conditions, "trials": [trial, trial]}
        with self.assertRaises(ValueError):
            compare_runs.load_run(self.write("manifest.json", manifest))
        manifest["trials"] = [{**trial, "repeat_count": True}]
        with self.assertRaises(ValueError):
            compare_runs.load_run(self.write("manifest.json", manifest))


if __name__ == "__main__":
    unittest.main()
