# Synapse

Tab completions and natural language translation for Zsh.

Most CLI tools ship without Zsh completions. Synapse fills the gap — it generates them automatically from `--help` output and project files, and lets you describe commands in plain English with the `? query` prefix.

### Auto-generated completions

Run `synapse add <cmd>` to generate tab completions for any command, or `synapse scan` to pick up project-level targets (Makefile, package.json, Cargo.toml, docker-compose, Justfile).

https://github.com/user-attachments/assets/b019d91f-5532-4492-a7b7-0c30793e7e6c


### Natural language mode

Type `? find large files` and get a dropdown of real shell commands, powered by any OpenAI-compatible LLM (local or cloud).

https://github.com/user-attachments/assets/2925a3ea-5ca7-497e-875b-f8b54ea0e269


## Requirements

- macOS or Linux
- Zsh
- An OpenAI-compatible LLM endpoint (for NL mode only — completions work without it)

## Installation

**Quick install:**

```bash
curl -fsSL https://raw.githubusercontent.com/colin-ho/synapse/main/scripts/install.sh | sh
```

**From source** (requires the [Rust toolchain](https://rustup.rs/)):

```bash
git clone https://github.com/colin-ho/synapse.git
cd synapse
cargo install --path .
```

Then add Synapse to your shell:

```bash
synapse install   # adds eval "$(synapse)" to ~/.zshrc
```

Restart your shell, or run `eval "$(synapse)"` to activate immediately.

## Quick Start

```bash
# Generate completions for a command
synapse add cargo

# Generate completions from project files in cwd
synapse scan

# Natural language mode (after configuring an LLM)
# Type in your shell:
? find files larger than 100MB
```

## Configuration

Copy the example config:

```bash
mkdir -p ~/.config/synapse
cp config.example.toml ~/.config/synapse/config.toml
```

### NL translation

The `? query` prefix requires an OpenAI-compatible endpoint. Any provider that exposes `/v1/chat/completions` works — OpenAI, LM Studio, Ollama, etc.

**Local (LM Studio, no API key needed):**

```toml
[llm]
enabled = true
provider = "openai"
base_url = "http://127.0.0.1:1234"
model = "qwen2.5-coder-7b-instruct-mlx"
```

**Cloud (OpenAI):**

```toml
[llm]
enabled = true
provider = "openai"
api_key_env = "OPENAI_API_KEY"
model = "gpt-4o-mini"
```

See [`config.example.toml`](config.example.toml) for all options.

## CLI Reference

| Command | Description |
|---|---|
| `synapse` | Show help (terminal) or output init code (piped) |
| `synapse install` | Add `eval "$(synapse)"` to `~/.zshrc` |
| `synapse add <cmd>` | Generate completions for a command |
| `synapse scan` | Generate completions from project files |
| `synapse translate <query>` | Translate NL to shell command (TSV) |

## Key Bindings

After typing `? query` and pressing Enter:

| Key | Action |
|---|---|
| `Up/Down` | Navigate results |
| `Enter/Tab` | Accept selected command |
| `Esc` | Dismiss |
