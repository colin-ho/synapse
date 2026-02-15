# Synapse — Design Document

## Overview

**Synapse** is a Zsh plugin that provides intelligent, real-time command suggestions by combining shell history, contextual project awareness, spec-based CLI completions, and AI-powered suggestions. It offers two interaction modes: inline ghost text as the user types, and an on-demand dropdown menu showing multiple ranked suggestions.

The goal is to reduce the cognitive load of remembering exact commands, flags, and workflows — especially across different project types and tools.

---

## Goals & Non-Goals

### Goals
- Provide fast (<50ms p95) suggestions for the common case (history + context + spec)
- Gracefully augment with AI suggestions when simpler strategies lack confidence
- Be project-aware: understand git state, package managers, Makefiles, Docker, etc.
- Offer structured CLI completions via a spec system (subcommands, options, arguments)
- Provide both inline ghost text and an optional dropdown menu for exploring multiple suggestions
- Work offline (history + context + spec layers function without network)
- Be easy to install and configure

### Non-Goals
- Full shell replacement (this is a plugin, not a new terminal)
- IDE-level code intelligence (we're completing commands, not source code)
- Windows support (Zsh is Unix-only; targeting macOS and Linux)
- Plugin ecosystem for custom context providers (may revisit later)

---

## Architecture

```
┌─────────────────────────────────────────────────┐
│                   Zsh Shell                      │
│                                                  │
│  ┌─────────────┐    ┌────────────────────────┐   │
│  │  User types  │───▶│  zle widget            │   │
│  │  a command   │    │  (precmd / keypress)   │   │
│  └─────────────┘    └────────┬───────────────┘   │
│                              │ sends context      │
│                              ▼                    │
│                     Unix domain socket            │
│                              │                    │
└──────────────────────────────┼────────────────────┘
                               │
                               ▼
┌──────────────────────────────────────────────────┐
│                synapse daemon                     │
│         (single process, multi-session)           │
│                                                   │
│  ┌────────────────────────────────────────────┐   │
│  │           Suggestion Pipeline              │   │
│  │                                            │   │
│  │  ┌────────┐ ┌────────┐ ┌──────┐ ┌──────┐  │   │
│  │  │History │ │Context │ │ Spec │ │  AI  │  │   │
│  │  │ <5ms   │ │ <20ms  │ │<10ms │ │<500ms│  │   │
│  │  └───┬────┘ └───┬────┘ └──┬───┘ └──┬───┘  │   │
│  │      │          │         │        │       │   │
│  │      └──────────┼─────────┼────────┘       │   │
│  │                 ▼         ▼                │   │
│  │           Ranking / Merge / Dedup          │   │
│  │                    │                       │   │
│  └────────────────────┼──────────────────────┘   │
│                       ▼                          │
│  ┌──────────────────────────────────────────┐    │
│  │        Interaction Logger                 │    │
│  │  (records accept / dismiss / ignore)      │    │
│  └──────────────────────────────────────────┘    │
│                                                   │
│          Best suggestion / Suggestion list        │
└──────────────────────────────────────────────────┘
```

The system is split into two processes:

1. **Zsh widget** — a thin shell-script layer that captures input and renders suggestions
2. **Daemon** — a single long-running background process (Rust binary) that serves all terminal sessions

They communicate over a **Unix domain socket** at `$XDG_RUNTIME_DIR/synapse.sock` (or `/tmp/synapse-$UID.sock` as fallback).

### Single Daemon, Multiple Sessions

The daemon serves all terminal sessions from one process. Each connection is tracked with a session ID assigned on connect.

**Pros:**
- Shared history index, context caches, and spec store across sessions — lower total memory
- AI rate limiting is centralized — one token bucket for all sessions
- Single process to manage (start/stop/monitor)
- Cross-session learning: a command run in session A immediately improves suggestions in session B

**Cons:**
- A daemon crash affects all sessions (mitigated by graceful degradation — see below)
- Slightly more complex connection management (session routing, per-session state)
- Lock contention under heavy concurrent use (mitigated by per-session read paths with shared immutable indexes)

### Daemon Startup & Lock File

To prevent race conditions when multiple shells start simultaneously:

1. The widget checks for a PID file at `$XDG_RUNTIME_DIR/synapse.pid` (or `/tmp/synapse-$UID.pid`)
2. If the PID file exists and the process is alive (`kill -0`), connect to the existing daemon
3. If the PID file is stale or missing, acquire an exclusive `flock` on a lockfile (`synapse.lock`) before starting the daemon
4. The daemon writes its PID to the PID file after binding the socket
5. Any shell that lost the `flock` race re-checks for the socket and connects

### Graceful Degradation

If the daemon crashes or is unreachable:
- The widget silently stops showing suggestions — no error messages, no spam
- A `precmd` hook periodically attempts to reconnect (every 5 seconds, max 3 attempts per minute)
- If the user manually runs `synapse daemon start`, normal operation resumes immediately

---

## Components

### 1. Zsh Widget (`plugin/synapse.zsh`)

**Responsibilities:**
- Hook into `zle` to capture the input buffer on every keypress
- Send a request to the daemon with the current context (including a session ID)
- Render the returned suggestion as dimmed ghost text after the cursor (inline mode)
- Render a dropdown menu of multiple suggestions on demand (dropdown mode)
- Accept suggestion on right arrow / end-of-line; partial accept on `Ctrl+Right` (word-by-word)
- Start the daemon automatically if it's not running (with lock file coordination)
- Report user interactions back to the daemon (accept, dismiss, ignore)

**Async Update Mechanism (via `zle -F`):**

Zsh's `zle` is synchronous by default — widgets run to completion before the editor redraws. To receive async AI upgrades, the widget uses `zle -F`, which registers a callback on a file descriptor:

1. The widget opens a persistent connection (Unix socket fd) to the daemon
2. It registers the fd with `zle -F $fd _synapse_async_handler`
3. When the daemon pushes an update, Zsh invokes `_synapse_async_handler` during the next editor idle cycle
4. The handler reads the update, replaces the ghost text via `POSTDISPLAY`, and triggers a redraw with `zle -R`

Key considerations:
- `zle -F` only fires when `zle` is active (user is at the prompt). This is fine — we only show suggestions while the user is typing.
- The fd must be non-blocking (`zmodload zsh/system; sysopen -o nonblock`) to avoid hanging the shell.
- Async updates are suppressed while the dropdown is open to avoid visual disruption.

**Dropdown Menu:**

The dropdown is triggered by pressing `Down Arrow` and rendered below the command line using `POSTDISPLAY` with `region_highlight` for styling:

- Items are displayed as a scrollable list (max 8 visible at a time)
- The selected item is highlighted with `standout`, unselected items are dimmed
- A status line at the bottom shows the current position (e.g., `[3/12]`) and source of the selected suggestion
- Navigation uses a dedicated `synapse-dropdown` keymap entered via `recursive-edit`, providing modal key handling without interfering with normal Zsh editing
- When an item is accepted, its text replaces the current buffer
- Typing any alphanumeric character while the dropdown is open inserts the character and dismisses the dropdown

**Context payload sent to daemon (JSON):**

```json
{
  "session_id": "a1b2c3",
  "buffer": "git chec",
  "cursor_pos": 8,
  "cwd": "/home/user/myproject",
  "last_exit_code": 0,
  "recent_commands": ["git status", "npm test", "git add ."],
  "env_hints": {
    "VIRTUAL_ENV": "/home/user/.venv",
    "NODE_ENV": "development"
  }
}
```

**Interaction feedback sent to daemon:**

```json
{"type": "interaction", "session_id": "a1b2c3", "action": "accept", "suggestion": "git checkout main", "source": "history", "buffer_at_action": "git chec"}
{"type": "interaction", "session_id": "a1b2c3", "action": "dismiss", "suggestion": "git checkout main", "source": "history", "buffer_at_action": "git chec"}
{"type": "interaction", "session_id": "a1b2c3", "action": "ignore", "suggestion": "git checkout main", "source": "history", "buffer_at_action": "git checkout -b feat"}
```

Actions:
- **accept**: user pressed right arrow / tab to accept the suggestion
- **dismiss**: user pressed Esc to explicitly dismiss
- **ignore**: user continued typing something different than the suggestion

**Keybindings:**

| Key | Action |
|---|---|
| `→` (Right arrow) | Accept full suggestion |
| `Ctrl+→` | Accept next word |
| `Tab` | Accept full suggestion (configurable) |
| `Esc` | Dismiss suggestion |
| `↓` (Down arrow) | Open dropdown menu |

**Dropdown keybindings (while dropdown is open):**

| Key | Action |
|---|---|
| `↓` (Down arrow) | Move selection down (wraps) |
| `↑` (Up arrow) | Move selection up (wraps) |
| `Enter` / `Tab` / `→` | Accept selected suggestion |
| `Esc` | Dismiss dropdown |
| Any letter/digit | Insert character, dismiss dropdown |

### 2. Daemon (`src/`)

The daemon is a single Rust binary that runs in the background. It manages the suggestion pipeline, maintains in-memory caches, and tracks per-session state.

#### 2a. History Provider

- Parses `~/.zsh_history` (or `$HISTFILE`) on startup and watches for changes
- Builds a BTreeMap prefix index for fast lookup
- Uses fuzzy matching (substring + Levenshtein) when prefix match fails
- Ranks by recency and frequency (weighted combination)
- Supports `suggest_multi()` for returning top-N matches to the dropdown
- **Target latency:** <5ms

#### 2b. Context Provider

Gathers project-level signals to suggest commands relevant to the current environment:

| Signal | What it provides |
|---|---|
| `Makefile` | `make` targets |
| `package.json` | `npm run` / `yarn` / `pnpm` / `bun` scripts |
| `Cargo.toml` | `cargo` subcommands |
| `pyproject.toml` / `setup.py` | Python tooling commands |
| `docker-compose.yml` | `docker compose` services |
| `Justfile` | `just` recipes |
| `.git/` | Branch names, recent refs, common git workflows |

**Directory scanning:** The context provider walks up from the cwd toward the filesystem root, stopping at the git root (if inside a repo) or after `scan_depth` levels (configurable, default 3). This handles monorepos where the user may be deeply nested.

**Caching:** Context is cached per-cwd with a 5-minute TTL via `moka::future::Cache`.

**Target latency:** <20ms

#### 2c. Spec Provider

The spec provider offers structured CLI completions inspired by [Fig's autocomplete specs](https://fig.io/docs/getting-started). Each CLI tool can have a **spec** that defines its subcommand tree, options, arguments, and dynamic generators.

**Spec format (TOML):**

```toml
name = "git"
description = "The fast distributed version control system"

[[subcommands]]
name = "commit"
description = "Record changes to the repository"

  [[subcommands.options]]
  short = "-m"
  long = "--message"
  takes_arg = true
  description = "Commit message"

  [[subcommands.options]]
  long = "--amend"
  description = "Amend previous commit"

[[subcommands]]
name = "checkout"
aliases = ["co"]
description = "Switch branches or restore working tree files"

  [[subcommands.args]]
  name = "branch"
  [subcommands.args.generator]
  command = "git branch --no-color 2>/dev/null"
  strip_prefix = "* "
  cache_ttl_secs = 10
```

**Spec resolution (3-tier priority):**

1. **Project user specs** (`.synapse/specs/*.toml` in project root) — highest priority, user-defined overrides
2. **Project auto-generated specs** — generated at runtime by scanning project files (Makefile targets, package.json scripts, Cargo.toml, docker-compose services, Justfile recipes)
3. **Built-in specs** — embedded in the binary via `include_str!` for common tools (git, cargo, npm, docker)

**Completion algorithm:**

1. Tokenize the input buffer (respecting quotes and escaping)
2. Look up the root command spec by name or alias
3. Walk the spec tree, consuming complete tokens to resolve the current subcommand context
4. Generate completions for the partial token: matching subcommands, options (when prefix starts with `-`), and arguments (static suggestions, generators, templates)
5. Score completions by prefix-match similarity

**Generators:** Shell commands executed via `tokio::process::Command` with configurable timeout. Results are cached in `moka::future::Cache` with per-generator TTL (default 30s). Generators run in the project root directory.

**Argument templates:** Common argument types (`file_paths`, `directories`, `env_vars`, `history`) that delegate to the shell or other providers.

**Built-in specs:** git (20+ subcommands with options and branch/remote/tag generators), cargo, npm, docker (including `docker compose` with service name extraction).

**Target latency:** <10ms (cached spec lookup + tree walk)

#### 2d. AI Provider

- Calls an LLM for intelligent suggestions when the other providers lack confidence
- Supports multiple backends via a provider trait:
  - **Local:** Ollama (llama3, codellama, etc.)
  - **API:** Anthropic Claude, OpenAI
- Sends a compact prompt including the buffer, cwd, recent commands, git branch, and project type
- Uses **debouncing**: only triggers after the user pauses typing for 150ms (configurable)
- Caches recent AI responses keyed by `(buffer_prefix, cwd, project_type, git_branch)` to avoid redundant calls
- Returns asynchronously — if a history/context suggestion is already shown, AI can silently upgrade it
- Applies input scrubbing before sending to external APIs (see Security section)

**Rate limiting (configurable):**
- Token bucket: max `rate_limit_rpm` requests per minute (default: 30)
- Max concurrent requests: `max_concurrent_requests` (default: 2)
- If the bucket is empty, the AI provider is silently skipped for that suggestion cycle

**Target latency:** <500ms (but non-blocking; user sees history/context/spec suggestion first)

**Example prompt sent to LLM:**

```
You are a terminal command autocomplete engine. Given the context below,
suggest the single most likely command the user is trying to type.
Respond with ONLY the completed command on a single line, nothing else.

Working directory: /home/user/webapp
Project type: Node.js (package.json detected)
Git branch: feature/auth
Recent commands: git status, npm test, npm run dev
Current input: "git push ori"
```

Note: multi-line commands are never suggested. If the LLM returns multiple lines, only the first line is used.

#### 2e. Ranking & Merge

**Single suggestion (inline ghost text):**

When multiple providers return suggestions, they are ranked by a weighted score:

```
score = (w_history × history_score)
      + (w_context × context_score)
      + (w_ai × ai_score)
      + (w_spec × spec_score)
      + (w_recency × recency_bonus)
```

Default weights (configurable, normalized to sum to 1.0):
- `w_history`: 0.30
- `w_context`: 0.15
- `w_ai`: 0.25
- `w_spec`: 0.15
- `w_recency`: 0.15

If the AI suggestion arrives after the initial response has been sent, and it scores higher, the daemon pushes an **update** over the socket to replace the displayed suggestion.

**Multiple suggestions (dropdown list):**

The `rank_multi()` function collects suggestions from all providers via `suggest_multi()`, applies per-source weights, deduplicates by text (keeping the highest-scoring entry), sorts by final score, and truncates to `max_list_results` (default 10). Each suggestion carries metadata: `text`, `source`, `confidence`, `description`, and `kind` (command, subcommand, option, argument, file, history).

---

## Interaction Logging

The daemon logs all user interactions with suggestions to a local append-only file at `~/.local/share/synapse/interactions.jsonl`. Each line is a JSON object:

```json
{"ts": "2025-01-15T10:32:01Z", "session": "a1b2c3", "action": "accept", "buffer": "git chec", "suggestion": "git checkout main", "source": "history", "confidence": 0.92, "cwd": "/home/user/myproject"}
{"ts": "2025-01-15T10:32:15Z", "session": "a1b2c3", "action": "ignore", "buffer": "git chec", "suggestion": "git checkout main", "source": "history", "confidence": 0.92, "cwd": "/home/user/myproject"}
```

This data is local-only and never sent externally. It captures:
- Which provider's suggestions get accepted vs. ignored vs. dismissed
- Confidence scores at the time of interaction
- Temporal patterns (when suggestions are useful vs. not)

The log file is rotated when it exceeds 50MB (configurable). This data will be used in the future to auto-tune ranking weights and improve suggestion quality.

---

## Security

When the AI provider sends context to an external API (Anthropic, OpenAI), a scrubbing layer is applied:

1. **Path redaction:** Home directory paths are replaced with `~`. Usernames in paths are stripped (e.g., `/home/jsmith/project` becomes `~/project`).
2. **Env var scrubbing:** Only env var keys are sent in `env_hints`, never values. Keys matching sensitive patterns (`*_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`, `*_CREDENTIALS`) are excluded entirely.
3. **Command scrubbing:** Recent commands containing patterns matching a configurable blocklist are excluded. Default blocklist: commands containing `export *=`, `curl -u`, `curl -H "Authorization"`, `echo $*_KEY`, and similar patterns.
4. **No scrubbing for local models:** When using Ollama or other local providers, scrubbing is skipped (traffic stays on localhost).

The scrubbing blocklist is configurable in `config.toml` under `[security]`.

---

## Communication Protocol

Messages over the Unix socket use newline-delimited JSON.

**Suggest request (Zsh → Daemon):**

```json
{"type": "suggest", "session_id": "a1b2c3", "buffer": "docker com", "cursor_pos": 10, "cwd": "/app", "last_exit_code": 0, "recent_commands": ["git status", "docker ps"], "env_hints": {"NODE_ENV": "development"}}
```

**Suggestion response (Daemon → Zsh):**

```json
{"type": "suggestion", "text": "docker compose up -d", "source": "history", "confidence": 0.92}
```

**Async update (Daemon → Zsh, pushed):**

```json
{"type": "update", "text": "docker compose up --build -d", "source": "ai", "confidence": 0.95}
```

**List suggestions request (Zsh → Daemon):**

```json
{"type": "list_suggestions", "session_id": "a1b2c3", "buffer": "git co", "cursor_pos": 6, "cwd": "/app", "max_results": 10}
```

**Suggestion list response (Daemon → Zsh):**

```json
{"type": "suggestion_list", "suggestions": [
  {"text": "git commit", "source": "spec", "confidence": 0.92, "description": "Record changes to the repository", "kind": "subcommand"},
  {"text": "git checkout", "source": "spec", "confidence": 0.88, "description": "Switch branches or restore working tree files", "kind": "subcommand"},
  {"text": "git commit --amend", "source": "history", "confidence": 0.75, "kind": "history"}
]}
```

**Interaction feedback (Zsh → Daemon):**

```json
{"type": "interaction", "session_id": "a1b2c3", "action": "accept", "suggestion": "docker compose up -d", "source": "history", "buffer_at_action": "docker com"}
```

**Lifecycle commands:**

```json
{"type": "ping"}
{"type": "shutdown"}
{"type": "reload_config"}
{"type": "clear_cache"}
```

**Suggestion sources:** `history`, `context`, `ai`, `spec`

**Suggestion kinds:** `command`, `subcommand`, `option`, `argument`, `file`, `history`

---

## Configuration

Config lives at `~/.config/synapse/config.toml`:

```toml
[general]
socket_path = "/tmp/synapse.sock"       # auto-detected if omitted
debounce_ms = 150                       # AI trigger delay
max_suggestion_length = 200             # truncate long suggestions
accept_key = "right-arrow"              # or "tab"
log_level = "warn"                      # "error" | "warn" | "info" | "debug" | "trace"

[history]
enabled = true
max_entries = 50000
fuzzy = true

[context]
enabled = true
scan_depth = 3                          # max levels to walk up (ignored if inside a git repo — walks to git root)

[ai]
enabled = true
provider = "ollama"                     # "ollama" | "anthropic" | "openai"
model = "llama3"                        # model name
endpoint = "http://localhost:11434"      # for ollama
api_key_env = "ANTHROPIC_API_KEY"       # env var name for API providers
max_tokens = 50
temperature = 0.0
timeout_ms = 2000                       # give up after this
fallback_to_local = true                # if API fails, skip AI layer
rate_limit_rpm = 30                     # max API requests per minute
max_concurrent_requests = 2             # max in-flight API requests

[spec]
enabled = true
auto_generate = true                    # auto-generate specs from project files
generator_timeout_ms = 500              # max time for generator commands
max_list_results = 10                   # max items in dropdown list

[weights]
# Weights are normalized to sum to 1.0
history = 0.30
context = 0.15
ai = 0.25
spec = 0.15
recency = 0.15

[security]
scrub_paths = true                      # redact home directory in API payloads
scrub_env_keys = ["*_KEY", "*_SECRET", "*_TOKEN", "*_PASSWORD", "*_CREDENTIALS"]
command_blocklist = ["export *=", "curl -u", "curl -H \"Authorization\""]

[logging]
interaction_log = "~/.local/share/synapse/interactions.jsonl"
max_log_size_mb = 50                    # rotate after this size
```

---

## Logging & Debugging

The daemon supports a `--verbose` flag (or `-v`, stackable: `-vv` for debug, `-vvv` for trace) that increases log output:

```bash
synapse daemon start --verbose          # info level
synapse daemon start -vv                # debug level — logs every suggestion cycle
synapse daemon start -vvv               # trace level — logs socket I/O, cache hits, provider timings
```

Logs are written to stderr by default, or to a file if configured:

```bash
synapse daemon start --verbose --log-file ~/.local/share/synapse/daemon.log
```

The `log_level` config option sets the default level; the `--verbose` flag overrides it.

---

## Installation

### Quick install

```bash
# Install the daemon binary
cargo install synapse

# Permanent setup: appends init to ~/.zshrc (idempotent)
synapse setup

# Or add it manually to any RC file
synapse setup --rc-file ~/.zshrc
```

This appends `eval "$(synapse init)"` to the RC file if not already present.

### Instant activation (any terminal)

```bash
eval "$(synapse init)"
```

This exports `SYNAPSE_BIN`, sources the Zsh plugin, and auto-starts the daemon. When run from a `target/{debug,release}` build directory, `synapse init` automatically detects **dev mode** and sets up a unique per-workspace socket at `/tmp/synapse-dev-{hash}.sock` with reload support and cleanup traps, so multiple worktrees can run simultaneously without conflicts.

### Dev workflow

```bash
# Build and activate from a worktree
cargo build && eval "$(./target/debug/synapse init)"

# Or use the convenience script (builds, then delegates to synapse init)
source dev/test.sh
source dev/test.sh --release
```

### With Oh My Zsh

```bash
git clone https://github.com/user/synapse \
  ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/synapse
```

Then add `synapse` to the `plugins` array in `.zshrc`.

### Daemon lifecycle

The Zsh plugin auto-starts the daemon on first prompt if not already running. The daemon can also be managed manually:

```bash
synapse daemon start
synapse daemon stop
synapse daemon status
```

---

## Project Structure

```
synapse/
├── dev/
│   └── test.sh                      # Dev convenience script (build + synapse init)
├── plugin/
│   └── synapse.zsh                  # Zsh widget, keybindings, dropdown UI
├── specs/
│   └── builtin/                     # Embedded CLI specs (compiled into binary)
│       ├── git.toml
│       ├── cargo.toml
│       ├── npm.toml
│       └── docker.toml
├── src/
│   ├── main.rs                      # Daemon entrypoint, socket server, CLI
│   ├── lib.rs                       # Library root (module exports)
│   ├── config.rs                    # Config parsing
│   ├── protocol.rs                  # JSON message types
│   ├── session.rs                   # Per-session state management
│   ├── security.rs                  # Input scrubbing for external APIs
│   ├── logging.rs                   # Interaction logger (append-only JSONL)
│   ├── ranking.rs                   # Score merging, dedup, and ranking
│   ├── cache.rs                     # LRU caches for context and AI
│   ├── spec.rs                      # Spec data model (CommandSpec, SubcommandSpec, etc.)
│   ├── spec_store.rs                # Spec loading, caching, resolution, generators
│   ├── spec_autogen.rs              # Auto-generate specs from project files
│   └── providers/
│       ├── mod.rs                   # SuggestionProvider trait (suggest + suggest_multi)
│       ├── history.rs               # History-based suggestions
│       ├── context.rs               # Project/environment context
│       ├── ai.rs                    # LLM-backed suggestions
│       └── spec.rs                  # Spec-based CLI completions
├── tests/
│   ├── history_tests.rs
│   ├── context_tests.rs
│   ├── security_tests.rs
│   ├── integration_tests.rs
│   └── spec_tests.rs
├── docs/
│   └── design-doc.md
├── Cargo.toml
├── config.example.toml
└── CLAUDE.md
```

---

## Performance Targets

| Metric | Target |
|---|---|
| Time to first suggestion (history) | <5ms |
| Time to first suggestion (context) | <20ms |
| Time to first suggestion (spec) | <10ms |
| AI suggestion latency (local LLM) | <500ms |
| AI suggestion latency (API) | <1000ms |
| Dropdown list generation | <30ms |
| Daemon memory usage (idle) | <30MB |
| Daemon memory usage (50k history) | <80MB |
| Daemon startup time | <200ms |

---

## Design Decisions

1. **Dual interaction modes.** Inline ghost text for the common case (fast, unobtrusive), with an on-demand dropdown for exploring multiple options. The dropdown uses `recursive-edit` with a custom keymap for modal navigation.
2. **Spec-based completions.** Structured CLI specs (inspired by Fig autocomplete) provide accurate subcommand/option/argument completions without relying on AI. Built-in specs are embedded in the binary; project-specific specs are auto-generated from project files or user-defined in `.synapse/specs/`.
3. **Local interaction logging.** All suggestion interactions (accept/dismiss/ignore) are logged locally. Data never leaves the machine. Will be used to auto-tune ranking weights in a future iteration.
4. **Single-line suggestions only.** Multi-line ghost text is unreliable across terminal emulators. If the AI returns multiple lines, only the first is used.
5. **Security scrubbing is required.** Path redaction, env var filtering, and command blocklisting are applied before any data is sent to external APIs. See the Security section.
6. **Trait-based provider architecture.** All providers implement `SuggestionProvider` with `suggest()` (single best) and `suggest_multi()` (top N). This makes adding new providers straightforward.
