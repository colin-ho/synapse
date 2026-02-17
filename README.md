# Synapse

Synapse is an intelligent Zsh completion daemon that renders real-time ghost text suggestions and an on-demand dropdown in your shell.

It is built as:
- A Rust daemon (`src/`) that handles completion requests over a Unix socket.
- A Zsh plugin (`plugin/synapse.zsh`) that captures prompt input and renders suggestions.

## Features

- Real-time ghost text suggestions while typing in Zsh.
- Dropdown suggestion list with keyboard navigation.
- Multi-provider completions from:
  - command history
  - CLI specs
  - filesystem paths
  - environment/PATH commands
  - workflow prediction from recent command transitions
- Spec system with:
  - built-in specs (`specs/builtin/*.toml`)
  - project auto-generated specs
  - optional command discovery from `--help`
  - user project overrides via `.synapse/specs/*.toml`
- Optional LLM-powered features:
  - natural language to command translation (`? ...`)
  - explain generated commands (`Ctrl+E`)
  - contextual argument suggestions
  - workflow prediction enrichment
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

## Default Key Bindings

- `Tab`: accept suggestion (or fall back to normal completion)
- `Right Arrow`: accept full suggestion
- `Ctrl+Right Arrow`: accept one word
- `Esc`: dismiss suggestion
- `Down Arrow`: open dropdown list
- `Ctrl+E`: explain the current NL-generated command (when available)

## Configuration

Copy `config.example.toml` to your platform config location:

- macOS: `~/Library/Application Support/synapse/config.toml`
- Linux: `~/.config/synapse/config.toml`

Important sections:

- `[spec]`: controls auto-generation, `--help` discovery, and generator behavior.
- `[llm]`: provider/model/base URL, plus NL/explain/contextual args settings.
- `[workflow]`: bigram workflow prediction behavior.
- `[security]`: path/env scrubbing and command blocklists.
- `[logging]`: interaction log path and rotation size.

Security note:
`[spec].trust_project_generators` is `false` by default. Keep this disabled unless you trust the repository you are working in.

## Architecture Overview

1. The Zsh plugin sends newline-delimited JSON requests over a Unix socket.
2. The daemon fans out requests across providers concurrently.
3. Suggestions are ranked and returned immediately as TSV protocol frames.
4. Optional phase-2 providers can send async `update` frames when they find a better result.

Core request types include:

- `suggest`
- `list_suggestions`
- `natural_language`
- `explain`
- `interaction`
- `command_executed`
- `ping`

## Specs and Discovery

Spec resolution priority:

1. project user specs (`.synapse/specs/*.toml`)
2. project auto-generated specs
3. built-in specs (`specs/builtin/*.toml`)
4. discovered specs

Discovered specs are persisted under `~/synapse/specs/`.

## Development

### Build and Test

```bash
cargo build
cargo build --release
cargo test
cargo test --test spec_tests
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

- `src/`: Rust daemon, protocol, providers, ranking, specs, workflow logic.
- `plugin/synapse.zsh`: Zsh integration and keybindings.
- `specs/builtin/`: built-in command specs.
- `tests/`: integration and behavior tests.
- `docs/`: operational and testing playbooks.
- `config.example.toml`: full configuration reference.

## License

MIT
