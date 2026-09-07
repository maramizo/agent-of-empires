import json
import os
import subprocess
import unittest
from unittest.mock import patch
from aoe_monitor import AoE, AoEError, DryModeSkipped


def reply(data=None, error=None):
    result = {"content": [{"type": "text", "text": error or json.dumps(data)}], "isError": error is not None}
    return subprocess.CompletedProcess([], 0, json.dumps({"result": result}), "")


class MonitorClientTests(unittest.TestCase):
    @patch("aoe_monitor.subprocess.run")
    def test_create_passes_agent_tool_without_colliding_with_rpc_name(self, run):
        run.return_value = reply({"id": "new-agent"})
        result = AoE().create_agent(tool="codex", title="Production", path="/project", idempotency_key="stable")
        self.assertEqual(result["id"], "new-agent")
        request = json.loads(run.call_args.kwargs["input"])
        self.assertEqual(request["params"]["name"], "create_agent")
        self.assertEqual(request["params"]["arguments"]["tool"], "codex")

    @patch("aoe_monitor.subprocess.run")
    def test_start_choices_and_monitor_changes_reach_mcp(self, run):
        run.return_value = reply({"ok": True})
        client = AoE()
        client.start_agent("worker", model="chosen", effort="high", fast_mode=False)
        self.assertEqual(json.loads(run.call_args.kwargs["input"])["params"], {"name": "start_agent", "arguments": {"session_id": "worker", "model": "chosen", "effort": "high", "fast_mode": False}})
        client.update_monitor("watch", cadence="5m", enabled=False)
        self.assertEqual(json.loads(run.call_args.kwargs["input"])["params"], {"name": "update_monitor", "arguments": {"monitor_id": "watch", "cadence": "5m", "enabled": False}})
        client.delete_monitor("watch")
        self.assertEqual(json.loads(run.call_args.kwargs["input"])["params"], {"name": "delete_monitor", "arguments": {"monitor_id": "watch"}})

    @patch.dict(os.environ, {"AOE_MONITOR_DRY_MODE": "1"})
    @patch("aoe_monitor.subprocess.run")
    def test_dry_result_is_a_catchable_failure_without_fabricated_agent(self, run):
        run.return_value = reply({"dry_mode": True, "executed": False})
        with self.assertRaises(DryModeSkipped):
            AoE().search_agents("Production")
        self.assertIn("--dry-mode", run.call_args.args[0])
        self.assertTrue(issubclass(DryModeSkipped, AoEError))

    @patch("aoe_monitor.subprocess.run")
    def test_errors_and_ambiguous_recipients_are_not_success(self, run):
        for result in [reply(error="delivery uncertain"), subprocess.CompletedProcess([], 1, "", "adapter failed"), subprocess.CompletedProcess([], 0, "not json", ""), reply({"sessions": []}), reply({"sessions": [{"id": "a"}, {"id": "b"}]})]:
            run.return_value = result
            with self.assertRaises(AoEError):
                AoE().find_agent("Production")
        run.side_effect = subprocess.TimeoutExpired("aoe", 70)
        with self.assertRaises(AoEError):
            AoE().send_message("agent", "message")


if __name__ == "__main__":
    unittest.main()
