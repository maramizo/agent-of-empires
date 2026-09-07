"""AoE monitor client. Available automatically to scripts launched by AoE."""
import json
import os
import subprocess


class AoEError(RuntimeError):
    """An MCP call failed; delivery may be uncertain for live mutations."""


class DryModeSkipped(AoEError):
    """AoE deliberately did not execute a tool in dry mode."""


class AoE:
    def call(self, tool_name, /, **arguments):
        binary = os.environ.get("AOE_MONITOR_BIN", "aoe2")
        command = [binary, "mcp", "serve"]
        if os.environ.get("AOE_MONITOR_DRY_MODE") == "1":
            command.append("--dry-mode")
        request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": tool_name, "arguments": arguments}}
        try:
            process = subprocess.run(command, input=json.dumps(request) + "\n",
                                     text=True, capture_output=True, timeout=70)
            if process.returncode:
                raise AoEError(process.stderr.strip() or "MCP process failed")
            reply = json.loads(process.stdout)
            if "error" in reply:
                raise AoEError(str(reply["error"]))
            result = reply["result"]
            text = result["content"][0]["text"]
            if result.get("isError"):
                raise AoEError(text)
            data = json.loads(text)
            if isinstance(data, dict) and data.get("dry_mode") and data.get("executed") is False:
                raise DryModeSkipped(f"{tool_name}: skipped in dry mode")
            return data
        except (OSError, subprocess.TimeoutExpired, ValueError, KeyError, TypeError, IndexError) as error:
            raise AoEError(f"MCP call failed: {error}") from error

    def list_agents(self):
        return self.call("list_agents")["sessions"]

    def search_agents(self, query, exact=False):
        return self.call("search_agents", query=query, exact=exact)["sessions"]

    def find_agent(self, title):
        matches = self.search_agents(title, exact=True)
        if len(matches) != 1:
            raise AoEError(f"Expected one agent named {title!r}, found {len(matches)}")
        return matches[0]

    def create_agent(self, **parameters):
        return self.call("create_agent", **parameters)

    def start_agent(self, session_id, **options):
        return self.call("start_agent", session_id=session_id, **options)

    def update_monitor(self, monitor_id, **parameters):
        return self.call("update_monitor", monitor_id=monitor_id, **parameters)

    def delete_monitor(self, monitor_id):
        return self.call("delete_monitor", monitor_id=monitor_id)

    def send_message(self, session_id, message):
        return self.call("send_message", session_id=session_id, message=message)

    def queue_message(self, session_id, message, message_id):
        return self.call("queue_message", session_id=session_id, message=message, message_id=message_id)


aoe = AoE()
