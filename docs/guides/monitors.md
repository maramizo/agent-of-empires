# Python monitors

Any agent connected to AoE's MCP server can create and manage monitors. There is
no orchestrator-only role. Monitors run on the daemon host in its active profile;
the daemon must remain running for scheduling to work. Python 3 must be available
on that host, or selected explicitly with an interpreter path.

A monitor runs a Python script once per cadence. Scripts can inspect production,
perform arbitrary Python work, and use the same AoE agent tools available to an
orchestrator.

## Create and test

Write a script on the daemon host, then call `create_monitor`:

```json
{
  "name": "production-health",
  "script_path": "/absolute/path/production_health.py",
  "cadence": "5m",
  "enabled": false,
  "dry_mode": true,
  "timeout_seconds": 120
}
```

`name` is unique within the profile. Repeating an identical creation returns the
existing monitor. `cadence` accepts an integer followed by `s`, `m`, `h`, or `d`,
from `1s` through `31d`. Optional `python` selects an executable, including an
absolute virtualenv Python path. `args` supplies additional script arguments.
`working_directory` defaults to the script's parent directory.

Use `run_monitor` with the returned `monitor.id` as `monitor_id`. It works while
paused, returns immediately, and accepts a `dry_mode` override for that run.
Poll `list_monitor_runs` for the run's status, exit code, stdout, and stderr.

After testing, call `update_monitor` with `monitor_id`, `enabled: true`, and
`dry_mode: false` to enable live runs. Creation defaults to enabled and dry mode;
the first scheduled run occurs after one cadence, not during creation. A live
read-only script can also be tested directly with `run_monitor` and
`dry_mode: false`.

## Python and agent actions

AoE places its standard-library-only helper on `PYTHONPATH` automatically:

```python
import argparse
import urllib.request
from aoe_monitor import aoe, AoEError, DryModeSkipped

parser = argparse.ArgumentParser()
parser.add_argument("--dry-mode", action="store_true")
args = parser.parse_args()

# Replace this URL with your application's health check.
try:
    with urllib.request.urlopen("http://127.0.0.1:9000/health", timeout=10) as response:
        healthy = response.status == 200
except OSError:
    healthy = False

if not healthy:
    try:
        matches = aoe.search_agents("Production", exact=True)
        if len(matches) > 1:
            raise AoEError("More than one Production agent; choose an explicit ID")
        if matches:
            target = matches[0]
        else:
            target = aoe.create_agent(
                title="Production", tool="codex", path="/absolute/path/project",
                idempotency_key="production-health-worker",
            )
        # Sending also starts/resumes a stopped agent. To start without a prompt,
        # use aoe.start_agent(target["id"]).
        aoe.send_message(target["id"], "The health check failed. Investigate it.")
    except DryModeSkipped as error:
        print(f"Would request agent assistance: {error}")
    except AoEError as error:
        # A live send timeout can mean delivery is uncertain. Inspect before retrying.
        print(f"Agent action failed: {error}")
```

Monitor messages carry an automatic MCP envelope identifying the monitor by
its exact ID and current name. They are labeled as autonomous communication,
not as user messages. See [MCP sender attribution](mcp-servers.md) for the
format used by agent and monitor senders.

For local terminal Codex agents, `aoe.send_message` uses native
`codex queue --thread <native-session-id> --message <text>`. This is also the
default for the fork's CLI sends, TUI Send Message, API sends, and restart wake
messages. It can start a stopped agent and waits up to ten seconds for its
native thread in that pane's dedicated Codex app server, including before the
first prompt. The terminal and queue command share its local socket. Restart
older terminals once to enable delivery.
It never falls back to typing or pressing Enter if identity or queueing fails.
Sandboxed/remote Codex agents and custom launch commands currently return an
unsupported-delivery error. Other terminal agents retain text-plus-Enter delivery;
interactive terminal keys and permission responses remain direct terminal input.

Success reports `backend: "codex"` and `disposition: "queued"`: Codex accepted
the message, which is not confirmation that the task has finished. Native
queue calls are not idempotent; inspect the worker after an uncertain timeout
instead of blindly retrying. AoE's dry mode still skips the entire MCP call.

`aoe.find_agent(title)` requires exactly one matching title and raises `AoEError`
for missing or ambiguous recipients. `search_agents(query, exact=False)` searches
titles, IDs, and groups. `aoe.call(tool_name, **arguments)` exposes every AoE MCP
tool, including project management, output inspection, queues, onboarding, and
monitor management. A monitor can explicitly create a fresh agent by providing a
new creation idempotency key.

Both `create_agent` and `start_agent` accept optional `model`, `effort`, and
`fast_mode` choices. Terminal Codex supports all three; terminal Claude supports
model and effort. Structured creation uses its adapter's existing model/effort
support and rejects `fast_mode`. Unsupported combinations fail explicitly.

```python
agent = aoe.create_agent(
    title="Production", tool="codex", path="/absolute/path/project",
    idempotency_key="production-worker", model="your-model-id",
    effort="high", fast_mode=True,
)
aoe.start_agent(agent["id"])
# After stopping the agent, a later start can override selected settings:
# aoe.start_agent(agent["id"], effort="medium", fast_mode=False)
```

Choose a model and effort level supported by the installed agent and your account.
Codex fast mode maps to priority service; `False` explicitly selects standard
service. See the [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).
Omitted fields retain stored choices, or inherit the native agent defaults if
nothing was selected. Choices persist across restarts and resume the same saved
conversation. `start_agent` rejects changing settings on a running agent; stop
it first. Repeating `create_agent` with an existing idempotency key returns that
agent and does not update it. `list_agents` exposes stored `launch_options`.

Monitor edits and removal also have Python helpers:

```python
aoe.update_monitor(monitor_id, cadence="10m", enabled=False)
aoe.delete_monitor(monitor_id)
```

Updates accept any creation setting, including script path, cadence, arguments,
timeout, and dry mode. Deletion preserves the Python script and requires the
current run to finish or be canceled first. Both helpers obey dry mode.

The script receives `AOE_MONITOR_ID`, `AOE_MONITOR_RUN_ID`,
`AOE_MONITOR_DRY_MODE` (`1` or `0`), and the MCP connection environment. Use the
monitor ID for stable correlation; each invocation has a distinct run ID.

## Dry mode

Dry mode does both of the following:

1. Adds `--dry-mode` to the Python script's arguments.
2. Makes all tool calls through AoE's MCP bridge return
   `{"dry_mode": true, "executed": false, ...}` without contacting the daemon.
   This includes reads and searches, not just mutations. The Python helper raises
   `DryModeSkipped`, a subclass of `AoEError`, instead of inventing successful
   results or agent IDs.

`aoe2 mcp serve --dry-mode` provides the same behavior outside a scheduled run.
Inside a monitor, `AOE_MONITOR_DRY_MODE` enables it automatically, including when
a script starts the MCP adapter directly. Protocol initialization and tool
listing remain available, and invalid tool arguments still fail validation.

Dry mode governs the AoE MCP bridge. Arbitrary Python HTTP requests, database
writes, subprocesses, and other MCP clients remain the script's responsibility;
use `args.dry_mode` when those operations need different behavior. Dry runs do not
receive the daemon token from the monitor runner.

## Scheduling and operations

- Runs of one monitor never overlap while managed by the daemon. Up to four
  scripts run concurrently per daemon; due jobs wait for a free slot.
- Missed intervals are skipped instead of replaying a backlog. A long-running
  script can run again after completion when its next cadence is already due.
- Failed and timed-out runs are recorded. Later cadences continue normally;
  scripts should deduplicate repeated alerts and agent creation themselves.
- `enabled: false` pauses future runs. It does not interrupt the current run.
  `cancel_monitor_run` terminates that run's process group. Deletion is rejected
  until the active run ends and never deletes the script file.
- Graceful daemon shutdown cancels active runs and preserves schedules. Startup
  marks abandoned run records interrupted. Registry writes and per-monitor run
  claims use filesystem locks to coordinate callers.
- The ten latest runs per monitor are retained, with up to 32 KiB each of stdout
  and stderr. Output is available after completion; `output_truncated` identifies
  clipped tails. The default timeout is 300 seconds, configurable up to 86400.
- Scripts are read anew each run. Editing a script affects subsequent invocations.
- Monitor endpoints require daemon authentication and are disabled in read-only
  and CityHall modes.
