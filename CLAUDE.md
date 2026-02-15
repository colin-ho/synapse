# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Synapse is an intelligent Zsh command suggestion daemon that provides real-time ghost text completions (like GitHub Copilot for the terminal). It consists of a Rust daemon communicating over a Unix domain socket with a Zsh plugin.

## Build & Development Commands

```bash
cargo build                          # Debug build
cargo build --release                # Release build
cargo test                           # Run all tests
cargo test test_name                 # Run a single test by name
cargo test --test history_tests      # Run a specific test file
cargo test -- --nocapture            # Run tests with stdout visible
cargo clippy                         # Lint
cargo fmt                            # Format
```

Run the daemon in foreground for development:
```bash
cargo run -- daemon start --foreground -vv
```

## Architecture

### Two-Process Model

1. **Zsh Plugin** (`plugin/synapse.zsh`) — Thin shell layer using `zle` widgets to capture keystrokes, render ghost text via `POSTDISPLAY`, and communicate with the daemon over a Unix socket. Parses JSON responses with regex (no `jq` dependency).

2. **Rust Daemon** (`src/main.rs`) — Single long-running Tokio async process serving all terminal sessions concurrently over a Unix domain socket at `$XDG_RUNTIME_DIR/synapse.sock`.

### Suggestion Pipeline (3-Layer Cascade)

All providers implement the `SuggestionProvider` trait (`src/providers/mod.rs`):

- **History** (`src/providers/history.rs`) — BTreeMap prefix search + Levenshtein fuzzy matching. Target: <5ms.
- **Context** (`src/providers/context.rs`) — Scans project files (Makefile, package.json, Cargo.toml, etc.) walking up from cwd. Detects package managers from lockfiles. Target: <20ms.
- **AI** (`src/providers/ai.rs`) — LLM calls to Ollama (local), Anthropic, or OpenAI. Rate-limited with token bucket + semaphore. Target: <500ms local, <1s API.

### Two-Phase Response Flow

Phase 1 (sync): History + Context run in parallel via `tokio::join!`, best result returned immediately.
Phase 2 (async): AI provider spawned as a separate tokio task with debounce. If it produces a higher-scoring suggestion and the buffer hasn't changed, it pushes an `Update` message over the socket. Zsh receives it via `zle -F` callback.

### Key Subsystems

- **Ranking** (`src/ranking.rs`) — Weighted score merging (history: 0.35, context: 0.2, ai: 0.3, recency: 0.15).
- **Security** (`src/security.rs`) — Scrubs paths, env vars, and sensitive commands before sending to external AI APIs. Skipped for local Ollama.
- **Caching** (`src/cache.rs`) — `moka::future::Cache` LRU with TTL. Context cache keyed by cwd (5min TTL), AI cache keyed by (buffer_prefix, cwd, project_type, git_branch) (10min TTL).
- **Sessions** (`src/session.rs`) — Per-session state (cwd, recent commands, last buffer) identified by 12-char hex IDs.
- **Protocol** (`src/protocol.rs`) — Newline-delimited JSON. Request types: Suggest, Interaction, Ping, Shutdown, ReloadConfig, ClearCache. Response types: Suggestion, Update, Pong, Ack, Error.
- **Logging** (`src/logging.rs`) — Append-only JSONL interaction log at `~/.local/share/synapse/interactions.jsonl` with rotation at 50MB.

### Config

User config at `~/.config/synapse/config.toml`. See `config.example.toml` for all options. Parsed in `src/config.rs`.

## Testing Patterns

- Integration tests live in `tests/` (protocol, history, context, security).
- Tests that mutate env vars use `Mutex<()>` for serialization.
- Daemon lifecycle tests create in-process `UnixListener` instances.
- `tempfile::NamedTempFile` is used for isolated history file tests.

## Design Reference

`docs/design-doc.md` contains the full architecture spec including protocol details, performance targets, security model, and development phases.
