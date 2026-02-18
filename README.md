# Synapse

Synapse is an intelligent Zsh completion daemon that auto-discovers CLI specs and exports them as compsys completion functions, with built-in natural language to command translation.

It is built as:
- A Rust daemon (`src/`) that discovers specs, generates compsys completions, and handles NL translation over a Unix socket.
- A Zsh plugin (`plugin/synapse.zsh`) that provides NL translation mode, command execution reporting, and daemon lifecycle management.

## Features

- Compsys completion generation from CLI specs (gap-filling for commands without existing zsh completions).
- Natural language to command translation (`? query` prefix).
- Spec system with:
  - project auto-generated specs (Makefile, package.json, Cargo.toml, docker-compose, Justfile)
  - command discovery from `--help` (writes compsys files directly)
  - user project overrides via `.synapse/specs/*.toml`
- Optional LLM-powered features:
  - natural language to command translation (`? ...`)
- Built-in probe tool for protocol-level debugging (`synapse probe`).

## Requirements

- macOS or Linux
- Zsh
- Rust toolchain (if building from source)

## Quick Start

### 1) Build

```bash
cargo build --release
```

### 2) Activate in Zsh

Option A (if `synapse` is on your `PATH`): add to `~/.zshrc` automatically

```bash
./target/release/synapse install
```

Option B (works without installing to `PATH`): manual line in `~/.zshrc`

```bash
eval "$(/absolute/path/to/synapse)"
```

Then restart your shell.

For local development from this repository:

```bash
source dev/test.sh
```

## CLI

```bash
synapse start [--foreground] [-v|-vv|-vvv] [--log-file PATH] [--socket-path PATH]
synapse status [--socket-path PATH]
synapse stop [--socket-path PATH]
synapse install
synapse generate-completions [--output-dir PATH] [--force] [--no-gap-check]
synapse complete <command> [context...] [--cwd PATH]
synapse probe --request '<json>' [--socket-path PATH]
synapse probe --stdio [--socket-path PATH]
```

Common examples:

```bash
# Run daemon in foreground with debug logs
cargo run -- start --foreground -vv

# Check daemon status
synapse status

# Ping the daemon protocol
synapse probe --request '{"type":"ping"}'
```

## Key Bindings

In NL mode (after typing `? query`):

- `Enter`: translate query and show results dropdown
- `Up/Down Arrow`: navigate NL results
- `Enter/Tab`: accept selected result
- `Esc`: dismiss dropdown

Tab completion uses standard zsh compsys bindings (works with fzf-tab, zsh-autocomplete, etc.).

### Recommended Companion Tools

Synapse is designed to complement these tools:

- **[zsh-autosuggestions](https://github.com/zsh-users/zsh-autosuggestions)** — inline ghost text suggestions from history
- **[fzf-tab](https://github.com/Aloxaf/fzf-tab)** — fuzzy Tab completion menu (works with Synapse's generated completions)
- **[Atuin](https://atuin.sh/)** — enhanced shell history with cross-machine sync

## Configuration

Copy `config.example.toml` to your platform config location:

- macOS: `~/Library/Application Support/synapse/config.toml`
- Linux: `~/.config/synapse/config.toml`

Important sections:

- `[spec]`: controls auto-generation, `--help` discovery, and generator behavior.
- `[llm]`: provider/model/base URL, plus NL settings.
- `[completions]`: output directory, gap-only mode.
- `[security]`: path/env scrubbing and command blocklists.
- `[logging]`: interaction log path and rotation size.

Security note:
`[spec].trust_project_generators` is `false` by default. Keep this disabled unless you trust the repository you are working in.

## Architecture Overview

1. The Zsh plugin sends newline-delimited JSON requests over a Unix socket.
2. The daemon resolves specs and returns results as TSV protocol frames.

Core request types:

- `natural_language`
- `command_executed`
- `complete`
- `ping`
- `shutdown`
- `reload_config`
- `clear_cache`

## Specs and Discovery

Spec resolution priority (for the `Complete` handler):

1. User project specs (`.synapse/specs/*.toml`)
2. Project auto-generated specs (Makefile, package.json, etc.)

Discovery of unknown commands (triggered by `command_executed`) writes compsys completion files directly to `~/.local/share/synapse/completions/`. The compsys file IS the persistent cache.

## Development

### Build and Test

```bash
cargo build
cargo build --release
cargo test
cargo test --test integration_tests
cargo clippy -- -D warnings
cargo fmt --check
```

### Tooling

```bash
# Install pre-commit hook (fmt + clippy)
./scripts/setup-hooks.sh

# Generate coverage reports (requires cargo-llvm-cov)
./scripts/coverage
```

## Repository Layout

- `src/`: Rust daemon, protocol, spec engine, compsys export, NL translation.
- `plugin/synapse.zsh`: Zsh integration and keybindings.
- `tests/`: integration and behavior tests.
- `docs/`: operational and testing playbooks.
- `config.example.toml`: full configuration reference.

## License

MIT
