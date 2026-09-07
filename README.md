# Agent of Empires 2

A terminal and web session manager for autonomous coding agents, maintained at
[maramizo/agent-of-empires-2](https://github.com/maramizo/agent-of-empires-2).
The executable is **`aoe2`** and its updater uses this repository's releases.

This fork adds conversation onboarding, native Codex queue delivery, explicit
model/effort/fast-mode launch options, and Python monitors that any agent can
create, update, delete, or run. MCP messages identify their sending session or
monitor so recipients can distinguish autonomous communication from user input.

## Install

Requires tmux. Release binaries include the web dashboard.

```sh
curl -fsSL https://raw.githubusercontent.com/maramizo/agent-of-empires-2/main/scripts/install.sh | bash
aoe2 --version
aoe2
```

To update:

```sh
aoe2 update
```

## Build from source

```sh
git clone https://github.com/maramizo/agent-of-empires-2.git
cd agent-of-empires-2
cargo build --release
./target/release/aoe2
```

Add `--features web` to include the dashboard; this requires Node.js and npm.

## Existing installations

Session history and monitor configuration stay in the existing
`agent-of-empires` app data directory. Existing `AOE_*` environment variables
and tmux session names remain valid. Change configured MCP executable paths to
`aoe2`, then reconnect those clients. No conversation import is needed when
upgrading this fork.

## Documentation

- [Installation and updates](docs/installation.md)
- [CLI reference](docs/cli/reference.md)
- [MCP and orchestration](docs/guides/mcp-servers.md)
- [Python monitors](docs/guides/monitors.md)
- [Conversation resume and onboarding](docs/guides/session-resume.md)
- [API](docs/api.md)
- [Development](AGENTS.md)

## Upstream and license

Forked from [Agent of Empires](https://github.com/agent-of-empires/agent-of-empires).
Original authorship and the [MIT license](LICENSE) are preserved. This fork is
independently maintained and released.
