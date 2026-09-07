# MCP Servers

Agent of Empires forwards your configured [MCP](https://modelcontextprotocol.io)
servers to structured-view agents (Claude, Gemini, Codex) when a session
starts, so the agent can call those servers' tools. Without this, structured-view
sessions reach no MCP servers at all.

This applies to structured-view / ACP sessions only. tmux sessions run the
agent's own CLI, which loads MCP config through that tool's normal mechanism.

## Configuration

Create `mcp.json` in your AoE app directory:

- **Linux**: `$XDG_CONFIG_HOME/agent-of-empires/mcp.json` (defaults to
  `~/.config/agent-of-empires/mcp.json`)
- **macOS / Windows**: `~/.agent-of-empires/mcp.json`

Debug builds use the `agent-of-empires-dev` namespace instead.

The file uses the standard `.mcp.json` shape, the same `mcpServers` object
Claude, Gemini, and Codex already understand, so you can reuse definitions you
keep elsewhere:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "mcp-server-filesystem",
      "args": ["--root", "/home/me/projects"],
      "env": { "LOG_LEVEL": "info" }
    },
    "github": {
      "type": "http",
      "url": "https://api.example.com/mcp",
      "headers": { "Authorization": "Bearer ghp_..." }
    }
  }
}
```

Each entry is one of:

- **stdio** (default when `type` is omitted): `command` is required; `args` and
  `env` are optional. The agent launches the executable and speaks MCP over its
  stdio.
- **http** (`"type": "http"`): `url` is required; `headers` is optional.
- **sse** (`"type": "sse"`): `url` is required; `headers` is optional.

The same list is forwarded for fresh and resumed sessions.

## Per-profile servers

A profile can carry its own `mcp.json` that adds to, or overrides, the global
one. Create it in the profile's directory:

- `<app_dir>/profiles/<profile-name>/mcp.json`

It uses the exact same `mcpServers` shape as the global file. When a
structured-view session runs under a profile, AoE reads that profile's
`mcp.json` and merges it on top of the global file: a server name defined in
both is taken from the per-profile file (see Precedence below). A missing
per-profile file is normal and simply forwards nothing extra.

Per-profile entries are AoE state only. AoE never writes them back into any
agent's native config; the sync direction is native into AoE, never the
reverse.

## Project-local servers (trusted repos)

A repository can ship its own MCP servers in a `.mcp.json` at its root, the same
ecosystem-standard file other tools read. AoE forwards these as the
highest-precedence layer, but only after you have trusted the repository, because
a project-local stdio server would otherwise launch its `command` the moment a
session starts: opening a cloned, untrusted repo would be a zero-click way to run
its code. This is the same trust gate AoE already applies to repository lifecycle
hooks.

When you create a session for a repo whose `.mcp.json` (or hooks) you have not
yet approved, AoE shows a trust prompt listing the servers it found: each
server's name, transport, command and arguments or URL, and the NAMES of its env
vars and headers. Values are never shown. Approving records the trust; declining
creates the session without forwarding the project servers.

The file is read from the repository root (for a worktree session, the main
repository the worktree was created from), so the servers you reviewed in the
prompt are exactly the servers forwarded. The trust is re-checked on every
session start: if `.mcp.json` changes, AoE re-prompts the next time you create a
session for that repo, and in the meantime the changed servers are skipped.

Two current limitations:

- The trust prompt exists in the TUI and the `aoe2 add` CLI only. Sessions created
  from the web dashboard cannot approve project MCP yet, so their project-local
  `.mcp.json` is skipped (with a log notice) until you approve the repo from the
  TUI or CLI. A web trust surface is tracked separately.
- Per-worktree or per-branch `.mcp.json` divergence is not supported: the main
  repository's file is the one read. Use a per-profile `mcp.json` for servers
  that should differ per worktree.

## Native agent config

If you already declared MCP servers in your agent's own config, AoE reads them
too (read-only), so you do not have to copy them into `mcp.json`. The native
config read per agent:

- **Claude**: `~/.claude.json` (top-level `mcpServers`). A `session.agent_config_dir`
  entry for the agent, else `CLAUDE_CONFIG_DIR` in AoE's own environment, replaces
  the `~` here, so AoE reads the file the launched CLI does.
- **Gemini**: `~/.gemini/settings.json` (`mcpServers`; transport is chosen by
  which key the entry sets, `command` for stdio, `httpUrl` for http, `url` for
  sse).
- **Codex**: `~/.codex/config.toml` (`[mcp_servers.<name>]` tables). AoE honors
  Codex's `enabled` flag: an omitted value or `enabled = true` includes the
  server, while `enabled = false` keeps the native definition known for drift
  bookkeeping but excludes it from the effective and forwarded sets.

## Precedence

When the same server name appears in more than one source, the higher-precedence
source wins (per server, not whole file):

```text
agent-native  <  mcp.json (global)  <  per-profile mcp.json  <  project-local .mcp.json (trusted)
```

So a server defined in both your agent's native config and the global `mcp.json`
is taken from `mcp.json`; one defined in both the global and per-profile files is
taken from the per-profile file; and a trusted project-local server outranks all
of them. Each override is logged. The project-local layer only participates once
the repository is trusted (see Project-local servers above).

## Inspecting the effective set

Because the effective set is merged from up to four layers, AoE gives you one
surface to see exactly which servers an agent will reach and where each came
from. Every value is redacted: you see a server's command, args, or URL, and the
names of its env vars and headers, but never their secret values.

### CLI

```text
aoe2 mcp list                 # effective set for the default tool
aoe2 mcp list --agent gemini  # for a specific agent
aoe2 mcp list --json          # machine-readable, same redaction
```

Each row shows the server name, transport, its winning provenance
(`agent-native:claude`, `global`, `profile:<name>`, `project-local`), and which
lower layers it shadowed on a name collision.

### Web dashboard

The dashboard has an **MCP servers** tab under Settings (when running
`aoe2 serve`). It shows the same merged set with provenance, plus two things the
CLI surfaces read-only:

- **Conflicts.** AoE remembers the last definition it saw for each server in an
  agent's native config. If that file changes on disk, the next time you open the
  surface the changed server is flagged as a conflict, and you choose which side
  wins. Keeping AoE's version stores it in the global `mcp.json` (which outranks
  the native layer); choosing the native version simply accepts the new
  definition. AoE never writes back to an agent-native config.
- **Kept after removal.** If a server disappears from a native config, AoE keeps
  it in view and warns rather than silently dropping it. You can **keep** it
  (promoting it into the global `mcp.json` so it keeps forwarding) or **drop** it.

AoE remembers this last-seen state in `mcp_state.json` in the app directory,
written owner-only. The stored definitions keep their secret values (so a kept
server still works) but those values are redacted everywhere they are displayed.

Live connection status and reconnect / authenticate actions are tracked
separately and are not part of this surface yet.

## Capability gating

Not every agent supports every transport. `stdio` works everywhere. `http` and
`sse` servers are forwarded only when the agent advertises support for them in
its handshake; otherwise that server is dropped (with a warning in the log) so
AoE never sends a request the agent would reject.

## Errors

A missing `mcp.json` (or native config) is normal and forwards nothing. A
malformed file, or a single broken entry inside one, is logged as a warning and
skipped without blocking your sessions. Check `debug.log` in the app directory
if a configured server does not show up.

## Security

`mcp.json` lives in your app directory and is owned by you, so its `command`
entries and any secrets in `env` / `headers` stay out of source control. Treat
it like any file that can launch processes on your behalf: a stdio server runs
its `command` locally when a session starts.

A per-profile `mcp.json` lives in the profile directory under your app
directory, so it is owned by you with the same trust as the global file. Treat
it the same way: its `command` entries can launch processes on your behalf.

The drift store `mcp_state.json` (used by the management surface) also lives in
your app directory, owner-only. It records the last-seen definition of each
agent-native server, including secret values, so keep-on-removal and conflict
resolution can reconstruct a working server; treat it with the same care as
`mcp.json`. Those values are redacted on every surface that displays them.

A project-local `.mcp.json` is repository-provided, so unlike the files above it
is NOT implicitly trusted: a cloned, untrusted repo could otherwise launch its
`command` the moment you open a session. It sits behind the same repo-trust gate
AoE uses for lifecycle hooks, forwarded only after you approve the repo, and
re-checked on every session start so a changed file re-prompts. See Project-local
servers above. The trust fingerprint includes env and header values, so rotating
a secret in a project `.mcp.json` re-prompts; the prompt itself never displays
those values.

## Let an orchestrator manage AoE sessions

In the TUI, right-click the empty area of the sidebar and choose **New
Orchestrator Session**, between **New Session** and **Change Sort**. Choose its
directory and create it normally. This preset launches a Codex terminal with
instructions to plan with you and coordinate agents, projects, and worktrees.
It uses Codex's configured MCP servers; configure `aoe-orchestrator` as below
before creating the session.

This fork also exposes AoE's session API as MCP tools with `aoe2 mcp serve`.
Start an AoE daemon first, then configure this MCP server in the agent you
want to use as your orchestrator:

```json
{
  "mcpServers": {
    "aoe-orchestrator": {
      "command": "/absolute/path/to/aoe",
      "args": ["mcp", "serve", "--url", "http://127.0.0.1:8080"],
      "env": {
        "AOE_DAEMON_TOKEN": "<token from aoe2 serve>"
      }
    }
  }
}
```

Use port `8081` for a debug daemon. The MCP process connects to that daemon;
it does not launch one or use the CLI's local profile. Choose the daemon
whose sessions you intend to manage. Treat the token as a secret. This first
interface has the daemon token's authority over its sessions, without a
separate per-orchestrator ownership boundary. Configure it for the lead agent;
workers do not need this server just to receive tasks.

Plan with the lead agent in its normal conversation. It can use:

| Tool | Behavior |
| --- | --- |
| `list_projects` | List registered projects, scopes, pins, and default base branches. |
| `create_project` | Register an existing directory globally or for the daemon profile. |
| `update_project` | Change its pin or default base branch; `null` clears the branch. |
| `delete_project` | Unregister a project without deleting its files or agents. |
| `assign_agent_project` | Attach a project to an existing agent workspace. |
| `add_agent_worktree` | Add a named worktree, including another branch of a repository already in the workspace. |
| `list_agents` | Read live sessions and their current statuses. |
| `search_agents` / `start_agent` | Find a named agent or start an existing one without a message. |
| `list_monitors` / `create_monitor` / `update_monitor` / `delete_monitor` | Manage Python monitors from any connected agent. |
| `run_monitor` / `list_monitor_runs` / `cancel_monitor_run` | Test or run a monitor, inspect its output, or cancel it. |
| `list_external_conversations` | Discover external Codex or Claude conversations on the daemon host. |
| `onboard_conversation` | Register an exact external conversation and return its AoE session ID without launching it. |
| `create_agent` | Create a worker, optionally in a worktree. `send_message` launches its terminal if needed. Supply a stable `idempotency_key` for retries. Defaults to the normal terminal view. |
| `send_message` | Send its task or a follow-up. Structured delivery returns whether the prompt was sent, steered, or queued. |
| `queue_message` | Queue through native Codex for terminal workers, or the daemon queue for structured workers. Supply `message_id`; native retries are not deduplicated. |
| `list_messages` | Inspect structured queued prompts; native Codex queue listing is unavailable. |
| `read_agent_output` | Read structured events with pagination, or a terminal snapshot. |

For example, ask the lead to create two workers for separate tasks, send each
its instructions, inspect their statuses and outputs, and summarize their
results. Creation and task submission are separate operations: if submission
fails, keep the created session ID and retry or inspect that session instead
of creating another worker. A send timeout is ambiguous; inspect history or
the queue before resending. A successful send is not task completion.

`send_message` and `queue_message` take the recipient's AoE `session_id` and
plain `message` content. AoE adds sender attribution automatically to terminal,
structured, and queued delivery. Do not construct your own sender header.
A managed agent's message arrives in this format:

```text
AoE MCP message (autonomous communication, not a direct user message):
{
  "version": 1,
  "sender": {
    "kind": "agent",
    "session_id": "45d1f28221d24148",
    "session_name": "AoE Agent"
  },
  "content": "Please check the latest build."
}
```

The sender ID comes from the MCP process's `AOE_INSTANCE_ID`, not a tool
argument or a title search. AoE resolves the current name from the connected
daemon on each send. A missing or ambiguous claimed identity fails before
sending. JSON escaping preserves content and keeps newlines in names from
becoming extra header fields.

Monitor calls use `kind: "monitor"`, `monitor_id`, and `monitor_name`, with
null session fields. Calls without either runtime identity use
`kind: "external_mcp"` and null session fields; they are never labeled as the
user. Direct user input is not wrapped by this MCP adapter. Sender metadata
provides attribution, not proof of user authorization. Dry mode skips identity
lookups and delivery. Reconnect existing MCP clients after upgrading to load
this behavior and its tool descriptions.

Creation, sending, and output reads default to normal AoE terminal sessions.
Local Codex terminal messages use native `codex queue` by default, including
first-message delivery. Each managed terminal shares a local Codex app server
with its queue calls. AoE resolves the loaded native thread directly, without
waiting for a first-prompt hook. Restart older terminals once to enable this
connection.
Other terminal agents receive text plus Enter. Native queue acceptance does not
confirm completion, and retries can duplicate messages. Missing identity or queue
errors never fall back to terminal keystrokes.

Codex CLI versions that expose `codex queue` can also queue messages for normal
terminal sessions. For AoE terminals, prefer `aoe2 send` or the MCP tools so the
queue command uses the same local socket as the terminal.
The MCP `queue_message` tool accepts the AoE session ID and resolves the Codex
thread from that pane's dedicated app server on the daemon host. It detects the
session's actual mode automatically. Native queuing requires a running local,
unsandboxed managed Codex terminal and a Codex version exposing
`queue`. Use `send_message` to auto-start the Codex thread
before queueing follow-ups. Text is passed as a literal argument, without shell interpolation.

For native Codex, `message_id` is a correlation ID, not a deduplication key.
A timeout leaves delivery uncertain; inspect the worker before retrying.
`list_messages` reports that native queue listing is unavailable; use
`read_agent_output` to inspect progress. Structured queue behavior is unchanged.

Any connected agent can schedule scripts and use agent actions from Python. See
[Python monitors](./monitors.md) for cadence, dry mode, and the Python helper.

### Onboarding external conversations

The orchestrator can call `list_external_conversations` with `agent: "codex"`
(the default) or `agent: "claude"`. Results contain native `conversation_id`,
original `path`, last-modified time, and directory availability. Discovery uses
the daemon host and its current profile, including that profile's conversation-store
settings.

Pass the selected native ID to `onboard_conversation`:

```json
{
  "agent": "codex",
  "conversation_id": "<native-conversation-id>",
  "title": "Existing work",
  "group": "external"
}
```

The result's `session_id` is the **AoE session ID** for other agent tools;
`conversation_id` remains the native history ID. `created: false` means the
conversation was already registered, so retrying does not create another agent.
The original directory and conversation history are retained.

Onboarding does not interrupt or move the external terminal, and it does not
launch an agent. Finish and close the external agent before opening the AoE
session or using `send_message` to resume it. Inspect the returned `view` when
reusing a session that was already managed. Read-only and CityHall daemons
reject these onboarding endpoints.

### Projects and multiple worktrees

AoE projects are registered directories. Assigning a project gives the agent its
repository in the workspace; it is not merely a label. Register existing paths
with `create_project`, then pass `projects` to `create_agent` instead of `path`:

```json
{
  "projects": ["backend", "frontend"],
  "tool": "codex",
  "title": "Implement checkout",
  "idempotency_key": "checkout-worker-1",
  "worktree_branch": "feature/checkout"
}
```

The first project is primary. Multiple projects default to new managed
worktrees, one per repository. Alternatively, pass `path` plus
`extra_repo_paths`. `base_branch` chooses a shared base; `repo_bases` accepts
`{"repo": "/absolute/repository/path", "base_branch": "develop"}` entries
for individual bases. Registered project defaults apply when no override is
provided. Inspect `list_agents` for each workspace's repository and worktree
paths.

Use `assign_agent_project` with `session_id` and a registered project name or
absolute repository path to attach a project afterward. An in-place checkout
must be clean before conversion; a managed worktree moves with its changes.
Attachment may move the workspace and restart an idle worker. Active turns
are refused. Inspect `worker`, `worker_message`, and `warnings`: an attachment
can succeed even if restarting the worker fails.

For another branch of the same repository, use `add_agent_worktree`:

```json
{
  "session_id": "<aoe-session-id>",
  "project": "backend",
  "name": "backend-review",
  "branch": "review/checkout",
  "base_branch": "main"
}
```

Each worktree needs a unique directory name and a branch not checked out
elsewhere. `attach_existing_branch: true` opts into an existing branch and
preserves it during AoE branch cleanup. Ordinary project assignment continues
to reject duplicates. Attachments are not retry-idempotent; inspect the agent
before repeating a timed-out request.

Project update/delete operations take `name` and optional `scope` (`global` or
`profile`, default `global`). Use the scope returned by `list_projects` when
modifying a profile-specific project.

The MCP process uses stdio and makes authenticated HTTP requests to the
same daemon endpoints used by the dashboard. Existing daemon authentication,
read-only mode, repository trust, and agent capacity checks still apply.
Plugin-specific creation and turn quotas do not apply to this interface.
Native orchestrator roles, automatic result notifications, and a team UI are
not implemented by this initial adapter.

Agent creation and startup accept `model`, `effort`, and Codex `fast_mode` choices.
See [Python monitors](monitors.md#python-and-agent-actions) for supported agents,
resume behavior, and examples, including monitor updates and deletion.
