# Installation

## Prerequisites

- [tmux](https://github.com/tmux/tmux/wiki) (required)
- [Docker](https://www.docker.com/) (optional, for sandboxing agents in containers)
- [Node.js](https://nodejs.org/) (optional, only needed when building the web dashboard from source with `--features web`)

Building from source also needs a C toolchain for the bundled native
dependencies (SQLite, libgit2, OpenSSL, liblzma, and AWS-LC). Most platforms
are covered by a stock `cc`; targets without pre-generated AWS-LC bindings
also need CMake.

## Install Agent of Empires 2

### Quick Install (Recommended)

Run the install script:

```bash
curl -fsSL \
  https://raw.githubusercontent.com/maramizo/agent-of-empires-2/main/scripts/install.sh \
  | bash
```

### Build from Source

```bash
git clone https://github.com/maramizo/agent-of-empires-2
cd agent-of-empires-2
cargo build --release
```

The binary will be at `target/release/aoe2`.

To include the web dashboard (browser access):

```bash
cargo build --release --features web
```

This requires Node.js and npm. The web frontend is built automatically during compilation.

## Verify Installation

```bash
aoe2 --version
```

## Updating

```bash
aoe2 update
```

The `aoe2 update` command detects how aoe2 was installed (the install script, Nix, Cargo, or a custom Homebrew formula) and dispatches to the right upgrade mechanism. For Nix and Cargo it prints the manual upgrade command instead of attempting an automatic update, since those cases need external tooling.

Inside the TUI, press `u` when the update bar is visible to run the same flow without leaving the app. Press `Ctrl+x` to dismiss the bar for the current session.

If you installed shell completions as a static file, regenerate it after an update so it picks up new commands and flags. See [Shell Completions](guides/shell-completions.md) for both the static and the always-fresh eval-on-startup setup.

## Uninstall

```bash
aoe2 uninstall
```

Prompts to remove the binary, configuration (the app data dir), and tmux settings.

## Upgrading from this AoE fork

The executable is now `aoe2`. Releases and updates come from
[`maramizo/agent-of-empires-2`](https://github.com/maramizo/agent-of-empires-2/releases).
Existing app data directories, session IDs, tmux names, monitor files, and
`AOE_*` environment variables are preserved. Update MCP client commands to
`aoe2 mcp serve` and reconnect them to load the new executable.
