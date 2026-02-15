# Synapse — Design Document

## Overview

**Synapse** is a Zsh plugin that provides intelligent, real-time command suggestions by combining shell history, contextual project awareness, and AI-powered completions. It displays a single ghost-text suggestion inline as the user types, similar to zsh-autosuggestions but significantly smarter.

The goal is to reduce the cognitive load of remembering exact commands, flags, and workflows — especially across different project types and tools.

---

## Goals & Non-Goals

### Goals
- Provide fast (<50ms p95) suggestions for the common case (history + context)
- Gracefully augment with AI suggestions when simpler strategies lack confidence
- Be project-aware: understand git state, package managers, Makefiles, Docker, etc.
- Feel seamless — no UI beyond ghost text; no popups, no menus
- Work offline (history + context layers function without network)
- Be easy to install and configure

### Non-Goals
- Full shell replacement (this is a plugin, not a new terminal)
- IDE-level code intelligence (we're completing commands, not source code)
- Windows support (Zsh is Unix-only; targeting macOS and Linux)
- Multiple suggestion dropdown/menu (single ghost-text suggestion only for v1)
- Plugin ecosystem for custom context providers (may revisit post-v1)

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
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐ │   │
│  │  │ History  │  │ Context  │  │    AI    │ │   │
│  │  │ (fast)   │  │ (medium) │  │ (slow)   │ │   │
│  │  │ <5ms     │  │ <20ms    │  │ <500ms   │ │   │
│  │  └────┬─────┘  └────┬─────┘  └────┬─────┘ │   │
│  │       │              │              │       │   │
│  │       └──────────────┼──────────────┘       │   │
│  │                      ▼                      │   │
│  │               Ranking / Merge               │   │
│  │                      │                      │   │
│  └──────────────────────┼──────────────────────┘   │
│                         ▼                          │
│  ┌──────────────────────────────────────────────┐  │
│  │          Interaction Logger                   │  │
│  │  (records suggest / accept / dismiss / ignore)│  │
│  └──────────────────────────────────────────────┘  │
│                                                    │
│                 Best suggestion                    │
└──────────────────────────────────────────────────┘
```

The system is split into two processes:

1. **Zsh widget** — a thin shell-script layer that captures input and renders suggestions
2. **Daemon** — a single long-running background process (Rust binary) that serves all terminal sessions

They communicate over a **Unix domain socket** at `$XDG_RUNTIME_DIR/synapse.sock` (or `/tmp/synapse-$UID.sock` as fallback).

### Single Daemon, Multiple Sessions

The daemon serves all terminal sessions from one process. Each connection is tracked with a session ID assigned on connect.

**Pros:**
- Shared history index and context caches across sessions — lower total memory
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
- Render the returned suggestion as dimmed ghost text after the cursor
- Accept suggestion on right arrow / end-of-line; partial accept on `Ctrl+Right` (word-by-word)
- Start the daemon automatically if it's not running (with lock file coordination)
- Report user interactions back to the daemon (accept, dismiss, ignore)

**Async Update Mechanism (via `zle -F`):**

Zsh's `zle` is synchronous by default — widgets run to completion before the editor redraws. To receive async AI upgrades, the widget uses `zle -F`, which registers a callback on a file descriptor:

1. The widget opens a persistent connection (Unix socket fd) to the daemon
2. It registers the fd with `zle -F $fd _synapse_async_handler`
3. When the daemon pushes an update, Zsh invokes `_synapse_async_handler` during the next editor idle cycle
4. The handler reads the update, replaces the ghost text via `POSTDISPLAY`, and triggers a redraw with `zle -R`

This is the same mechanism used by `zsh-async` and other production Zsh plugins. Key considerations:
- `zle -F` only fires when `zle` is active (user is at the prompt). This is fine — we only show suggestions while the user is typing.
- The fd must be non-blocking (`zmodload zsh/system; sysopen -o nonblock`) to avoid hanging the shell.
- If `zle -F` proves unreliable on a specific terminal emulator, the fallback is synchronous-only mode: the widget waits up to 50ms for a response and shows whatever it gets (history/context only, no AI upgrades).

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

### 2. Daemon (`src/`)

The daemon is a single Rust binary that runs in the background. It manages the suggestion pipeline, maintains in-memory caches, and tracks per-session state.

#### 2a. History Provider

- Parses `~/.zsh_history` (or `$HISTFILE`) on startup and watches for changes
- Builds a trie or prefix index for fast lookup
- Uses fuzzy matching (substring + Levenshtein) when prefix match fails
- Ranks by recency and frequency (weighted combination)
- **Target latency:** <5ms

#### 2b. Context Provider

Gathers project-level signals to suggest commands relevant to the current environment:

| Signal | What it provides |
|---|---|
| `Makefile` | `make` targets |
| `package.json` | `npm run` / `yarn` / `pnpm` scripts |
| `Cargo.toml` | `cargo` subcommands |
| `pyproject.toml` / `setup.py` | Python tooling commands |
| `docker-compose.yml` | `docker compose` services |
| `.git/` | Branch names, recent refs, common git workflows |
| `Procfile` / `Justfile` | Task runner targets |
| `.env` files | Awareness of environment variables (keys only — values are never read) |
| Executable scan | Commands available in `$PATH` and local `./node_modules/.bin`, `.venv/bin`, etc. |

**Directory scanning:** The context provider walks up from the cwd toward the filesystem root, stopping at the git root (if inside a repo) or after `scan_depth` levels (configurable, default 3). This handles monorepos where the user may be deeply nested.

**File change detection:** Uses OS-native file watching — `kqueue` on macOS, `inotify` on Linux — via the `notify` crate. The daemon registers watches on detected project files and their parent directories. When a file changes, the cache for that directory subtree is invalidated. No polling.

**Target latency:** <20ms

#### 2c. AI Provider

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

**Target latency:** <500ms (but non-blocking; user sees history/context suggestion first)

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

#### 2d. Ranking & Merge

When multiple providers return suggestions, they are ranked by a weighted score:

```
score = (w_history × history_score)
      + (w_context × context_score)
      + (w_ai × ai_score)
      + (w_recency × recency_bonus)
```

Default weights (configurable, normalized to sum to 1.0):
- `w_history`: 0.35
- `w_context`: 0.2
- `w_ai`: 0.3
- `w_recency`: 0.15

If the AI suggestion arrives after the initial response has been sent, and it scores higher, the daemon pushes an **update** over the socket to replace the displayed suggestion.

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

**Request (Zsh → Daemon):**

```json
{"type": "suggest", "session_id": "a1b2c3", "buffer": "docker com", "cursor_pos": 10, "cwd": "/app", "last_exit_code": 0, "recent_commands": ["git status", "docker ps"], "env_hints": {"NODE_ENV": "development"}}
```

**Response (Daemon → Zsh):**

```json
{"type": "suggestion", "text": "docker compose up -d", "source": "history", "confidence": 0.92}
```

**Async update (Daemon → Zsh, pushed):**

```json
{"type": "update", "text": "docker compose up --build -d", "source": "ai", "confidence": 0.95}
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

[weights]
history = 0.35
context = 0.2
ai = 0.3
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

# Add the plugin to .zshrc
echo 'source $(synapse --shell-init)' >> ~/.zshrc
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
├── plugin/
│   └── synapse.zsh                  # Zsh widget and keybindings
├── src/
│   ├── main.rs                      # Daemon entrypoint, socket server, CLI
│   ├── config.rs                    # Config parsing
│   ├── protocol.rs                  # JSON message types
│   ├── session.rs                   # Per-session state management
│   ├── security.rs                  # Input scrubbing for external APIs
│   ├── logging.rs                   # Interaction logger (append-only JSONL)
│   ├── providers/
│   │   ├── mod.rs                   # Provider trait definition
│   │   ├── history.rs               # History-based suggestions
│   │   ├── context.rs               # Project/environment context
│   │   └── ai.rs                    # LLM-backed suggestions
│   ├── ranking.rs                   # Score merging and ranking
│   └── cache.rs                     # LRU caches for context and AI
├── tests/
│   ├── history_tests.rs
│   ├── context_tests.rs
│   ├── security_tests.rs
│   └── integration_tests.rs
├── Cargo.toml
├── config.example.toml
└── README.md
```

---

## Development Phases

### Phase 1: Foundation (Week 1–2)
- Zsh widget with ghost text rendering and keybindings
- Daemon skeleton with Unix socket server and multi-session support
- PID file / lock file coordination for daemon startup
- History provider with prefix matching
- Basic request/response protocol with session IDs
- Interaction logging infrastructure
- `--verbose` flag and log-level support
- **Milestone:** Working plugin that matches zsh-autosuggestions behavior

### Phase 2: Context Awareness (Week 3–4)
- Context provider: project file scanning and parsing
- Event-driven cache invalidation via `kqueue`/`inotify` (using `notify` crate)
- Git integration (branch names, recent refs)
- Configurable scan depth with git-root auto-detection
- Ranking/merge logic for multiple providers
- **Milestone:** Suggests `npm run dev` when in a Node project, `make build` near a Makefile, etc.

### Phase 3: AI Integration (Week 5–6)
- AI provider with Ollama support
- Debounce logic and async suggestion updates via `zle -F`
- Prompt engineering and response parsing (single-line only)
- API provider support (Anthropic, OpenAI)
- Response caching keyed by `(buffer_prefix, cwd, project_type, git_branch)`
- Rate limiting (configurable RPM and max concurrent)
- Security scrubbing layer for external API calls
- **Milestone:** AI suggestions appear after a brief pause, upgrading simpler suggestions

### Phase 4: Polish (Week 7–8)
- Fuzzy history matching (Levenshtein / Smith-Waterman)
- Partial accept (word-by-word with Ctrl+Right)
- Config file support and `synapse` CLI
- Performance profiling and optimization
- Installation scripts and documentation
- **Milestone:** Ready for public beta

---

## Performance Targets

| Metric | Target |
|---|---|
| Time to first suggestion (history) | <5ms |
| Time to first suggestion (context) | <20ms |
| AI suggestion latency (local LLM) | <500ms |
| AI suggestion latency (API) | <1000ms |
| Daemon memory usage (idle) | <30MB |
| Daemon memory usage (50k history) | <80MB |
| Daemon startup time | <200ms |

---

## Resolved Design Decisions

These were originally open questions, now settled:

1. **Single ghost-text suggestion.** No dropdown or menu for v1. Ship the simplest UX and revisit if users request it.
2. **Local interaction logging.** All suggestion interactions (accept/dismiss/ignore) are logged locally. Data never leaves the machine. Will be used to auto-tune ranking weights in a future iteration.
3. **Single-line suggestions only.** Multi-line ghost text is unreliable across terminal emulators. If the AI returns multiple lines, only the first is used.
4. **Security scrubbing is required.** Path redaction, env var filtering, and command blocklisting are applied before any data is sent to external APIs. See the Security section.
5. **No plugin ecosystem for v1.** Context providers are hardcoded. The trait-based architecture makes this easy to add later without major refactoring.
