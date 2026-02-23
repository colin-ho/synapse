# Synapse

Synapse is a spec engine and NL translation layer for Zsh. It auto-discovers CLI specs and exports them as compsys completion functions, with built-in natural language to command translation.

- A Rust CLI (`src/`) that discovers specs, generates compsys completions, runs generators, and translates NL queries.
- A Zsh plugin (`plugin/synapse.zsh`) that provides NL translation mode (`? query` prefix) and dropdown UI.

## Features

- Compsys completion generation from CLI specs (gap-filling for commands without existing zsh completions).
- Natural language to command translation (`? query` prefix via LLM).
- Spec system with:
  - project auto-generated specs (Makefile, package.json, Cargo.toml, docker-compose, Justfile)
  - command discovery from `--help` (writes compsys files directly)

## Requirements

- macOS or Linux
- Zsh

## Installation

### Quick install

```bash
curl -fsSL https://raw.githubusercontent.com/colin-ho/synapse/main/scripts/install.sh | sh
```

### From source

Requires the [Rust toolchain](https://rustup.rs/).

```bash
git clone https://github.com/colin-ho/synapse.git
cd synapse
cargo install --path .
```

### Shell setup

Run the installer to add Synapse to your `~/.zshrc`:

```bash
synapse install
```

This adds `eval "$(synapse)"` to your shell config (before `compinit` if present, so completions load correctly). Then restart your shell or run:

```bash
eval "$(synapse)"
```

Alternatively, add the eval line to `~/.zshrc` manually — useful if `synapse` isn't on your `PATH`:

```bash
# In ~/.zshrc
eval "$(/path/to/synapse)"
```

### Development

For local development, build and activate in one step:

```bash
source dev/test.sh
```

## CLI

```bash
synapse                               # Show help (terminal) or output init code (piped)
synapse install                       # Add eval "$(synapse)" to ~/.zshrc
synapse add <command>                 # Discover completions for a command via --help
synapse scan                          # Generate completions from project files (Makefile, etc.)
synapse run-generator <cmd>           # Run a generator command with timeout
synapse translate <query> --cwd <dir> # Translate NL to shell command (TSV output)
```

Common examples:

```bash
# Add completions for a specific command
synapse add curl

# Generate project completions
synapse scan

# Translate natural language (usually called by the plugin, not directly)
synapse translate "find large files" --cwd /home/user
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

Copy `config.example.toml` to `~/.config/synapse/config.toml` and customize.

Important sections:

- `[spec]`: controls auto-generation, `--help` discovery, and generator behavior.
- `[llm]`: provider/model/base URL, plus NL settings.
- `[completions]`: output directory, gap-only mode.
- `[security]`: command blocklists.

## Architecture

Synapse uses a **one-shot CLI model** — no daemon, no persistent process. The plugin calls `synapse translate` as a subprocess for NL queries. Completions are generated as static compsys files.

- **`src/cli/`** — Clap-based CLI: `add`, `scan`, `run-generator`, `translate`, `shell`, `install`.
- **`src/spec.rs`** — Data model: `CommandSpec`, `SubcommandSpec`, `OptionSpec`, `ArgSpec`, `GeneratorSpec`.
- **`src/spec_store.rs`** — Spec lookup and caching (project specs, discovered specs).
- **`src/spec_autogen.rs`** — Auto-generation from project files (Makefile, package.json, etc.).
- **`src/compsys_export/`** — Converts specs to zsh `_arguments` completion functions.
- **`src/llm/`** — LLM client, prompt construction, response parsing, path scrubbing.
- **`src/zsh_completion/`** — Scans fpath for existing completions (gap detection).
- **`plugin/synapse.zsh`** — Shell integration: NL mode, dropdown UI, keybindings, `synapse` wrapper.

Discovery writes compsys files directly — the compsys file IS the persistent cache. Discovery is user-driven via `synapse add`.

## Development

### Build and Test

```bash
cargo build
cargo test
cargo clippy
cargo fmt --check
```

### Pre-commit Hooks

```bash
./scripts/setup-hooks.sh
```

## Repository Layout

- `src/`: Rust CLI, spec engine, compsys export, NL translation.
- `plugin/synapse.zsh`: Zsh integration and keybindings.
- `tests/`: integration and unit tests.
- `config.example.toml`: full configuration reference.

## License

MIT
