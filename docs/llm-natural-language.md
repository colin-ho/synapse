# Natural Language → Command Translation

## Overview

Add a new interaction mode where the user types intent in natural language and Synapse translates it into a complete shell command. Triggered by a prefix character (e.g., `?` or `#`) at the start of the buffer, the LLM generates a command that appears as ghost text for the user to accept or reject.

## Problem

Synapse currently only completes commands the user already knows how to start typing. If a user doesn't know the right command (or the right flags), Synapse can't help. Common scenarios:

- "How do I find files larger than 100MB?" → `find . -type f -size +100M`
- "Kill whatever's using port 3000" → `lsof -ti:3000 | xargs kill`
- "Show me the last 5 git commits as one-liners" → `git log --oneline -5`
- "Compress this directory into a tar.gz" → `tar -czf archive.tar.gz directory/`
- "Find all TODO comments in Python files" → `grep -rn 'TODO' --include='*.py'`

These are queries where the user knows the *goal* but not the *command*. This is the single biggest UX gap between a smart autocompleter and an AI-powered terminal.

## Design

### Trigger Mechanism

The natural language mode activates when the buffer starts with `?` followed by a space:

```
? find large files in this directory
```

The `?` prefix is chosen because:
- It's not a valid command prefix in any shell
- It's easy to type (single keystroke, no modifier)
- It visually signals "I have a question"
- It doesn't conflict with existing Zsh features (glob qualifiers use `?` mid-word, not at the start)

The `?` and space are stripped from the buffer before sending to the LLM. The prefix is configurable in `config.toml`.

### Request Flow

```
User types: "? find files bigger than 100mb"
                  │
                  ▼
Zsh widget detects "? " prefix
                  │
                  ▼
Sends request with type: "natural_language"
  {
    "type": "natural_language",
    "session_id": "abc",
    "query": "find files bigger than 100mb",
    "cwd": "/home/user/project",
    "recent_commands": [...],
    "env_hints": {...}
  }
                  │
                  ▼
Daemon routes to LLM provider
                  │
                  ▼
LLM returns command
                  │
                  ▼
Response: { "type": "suggestion", "text": "find . -type f -size +100M", "source": "llm" }
                  │
                  ▼
Zsh widget replaces buffer:
  - Clears the "? find files bigger than 100mb" text
  - Sets buffer to the generated command
  - Does NOT execute — user reviews first
```

### Debouncing

Natural language queries are debounced more aggressively than regular suggestions:

- **Debounce delay:** 500ms (vs 150ms for regular suggest)
- **Minimum query length:** 5 characters after the `?` prefix
- **No suggestions while typing** — only fire when the user pauses

This prevents sending half-formed queries to the LLM and reduces API costs.

### LLM Prompt

```
You are a shell command generator. Convert the user's natural language request into a single shell command.

Environment:
- Shell: zsh
- OS: {os}  (e.g., "macOS 14.5" or "Ubuntu 22.04")
- Working directory: {cwd}
- Project type: {project_type}
- Available tools: {relevant_tools}  (e.g., "git, cargo, docker, npm")
- Recent commands: {recent_commands}

User request: {query}

Rules:
- Return ONLY the shell command, nothing else
- Use tools available on the system (prefer common POSIX utilities)
- Use the working directory context (don't use absolute paths unless necessary)
- If the request is ambiguous, prefer the most common interpretation
- If the request requires multiple commands, chain them with && or |
- Never generate destructive commands (rm -rf /, dd, mkfs) without explicit safeguards
- For file operations, prefer relative paths from the working directory
```

The "Available tools" field is populated from the environment provider's PATH scan — this prevents the LLM from suggesting tools that aren't installed.

### Zsh Widget Changes

New widget logic in `plugin/synapse.zsh`:

```zsh
_synapse_check_natural_language() {
    if [[ "$BUFFER" == "? "* ]] && (( ${#BUFFER} > 3 )); then
        # Extract query (strip "? " prefix)
        local query="${BUFFER#\? }"

        # Send natural language request
        _synapse_send_nl_request "$query"
    fi
}

_synapse_handle_nl_response() {
    local command="$1"
    if [[ -n "$command" ]]; then
        # Replace the buffer with the generated command
        BUFFER="$command"
        CURSOR=${#BUFFER}
        zle redisplay
    fi
}
```

When the user accepts the suggestion (right arrow or tab), the generated command replaces the buffer. The user can then review and execute it with Enter. The original query is never executed.

### Protocol Changes

Add a new request type:

```rust
#[derive(Debug, Deserialize)]
pub struct NaturalLanguageRequest {
    pub session_id: String,
    pub query: String,
    pub cwd: String,
    #[serde(default)]
    pub recent_commands: Vec<String>,
    #[serde(default)]
    pub env_hints: HashMap<String, String>,
}
```

Add `NaturalLanguage(NaturalLanguageRequest)` to the `Request` enum.

The response uses the existing `Suggestion` response type — the generated command is just a suggestion with `source: "llm"`.

Add a new source variant:

```rust
pub enum SuggestionSource {
    History,
    Spec,
    Filesystem,
    Environment,
    Llm,  // new
}
```

### Safety

Natural language → command translation has unique safety concerns because the user may not fully understand the generated command:

1. **Never auto-execute.** The generated command is always presented for review — the user must press Enter to run it.

2. **Destructive command warnings.** If the generated command contains potentially destructive operations, append a description in the suggestion's `description` field:
   - `rm` → "deletes files"
   - `chmod 777` → "makes files world-writable"
   - `dd` → "raw disk write"
   - `> file` → "overwrites file"

3. **Command validation.** Before returning the LLM response:
   - Check that the first token is a valid executable (exists in PATH or is a shell builtin)
   - Reject if the command starts with a blocklisted prefix from `security.command_blocklist`

4. **Explanation mode.** When the user presses a configurable key (default: `Ctrl+E`) while viewing a generated command, show a brief explanation of what it does in the dropdown area:
   ```
   find . -type f -size +100M
   ─────────────────────────────
   Finds files (not dirs) larger than 100MB
   in the current directory and subdirectories
   ```
   This explanation is a second LLM call, only triggered on demand.

### Caching

Natural language queries are cached to avoid redundant LLM calls:

- **Cache key:** `(normalized_query, cwd, os)`
- **TTL:** 10 minutes
- **Max entries:** 100

Query normalization: lowercase, collapse whitespace, strip trailing punctuation. This means "Find large files" and "find large files" hit the same cache entry.

### Config

```toml
[llm]
# ... (shared)
base_url = ""                      # optional OpenAI-compatible base URL (LM Studio: "http://127.0.0.1:1234")
natural_language = true            # enable ? prefix mode
nl_prefix = "?"                    # trigger character
nl_debounce_ms = 500               # debounce delay
nl_min_query_length = 5            # minimum characters after prefix
nl_explain_key = "ctrl-e"          # key to show command explanation
```

### Cost Analysis

Natural language queries are user-initiated and intentional — they won't fire on every keystroke. Typical usage:

| Scenario | Calls/day | Input tokens | Output tokens | Daily cost |
|---|---|---|---|---|
| Light usage | ~5 | ~2000 | ~200 | ~$0.001 |
| Heavy usage | ~30 | ~12000 | ~1200 | ~$0.008 |
| With explanations | ~10 extra | ~3000 | ~500 | ~$0.002 |

### UX Details

**Visual differentiation:** When in natural language mode (buffer starts with `?`), the ghost text should render in a different color (e.g., cyan instead of the default dim) to signal that this is a generated command, not a history/spec completion.

**Dropdown integration:** If the user presses Down Arrow while a natural language suggestion is shown, the dropdown shows:
1. The top LLM suggestion
2. 1-2 alternative commands (generated by asking the LLM for variants)
3. A "explain" option that shows what each command does

**History recording:** When the user accepts and executes a generated command, it enters the history like any other command. The natural language query itself is NOT recorded in history — only the resulting command.

### Interaction Logging

Natural language interactions are logged to `interactions.jsonl` with additional fields:

```json
{
    "ts": "...",
    "session": "abc",
    "action": "accept",
    "buffer": "? find large files",
    "suggestion": "find . -type f -size +100M",
    "source": "llm",
    "confidence": 0.95,
    "cwd": "/home/user/project",
    "nl_query": "find large files"
}
```

This data enables future fine-tuning and quality analysis.

### Implementation Order

1. **Add `Llm` source variant** to `SuggestionSource`
2. **Add `NaturalLanguage` request type** to protocol
3. **Add handler in daemon** — route to LLM client, return suggestion
4. **Add Zsh widget logic** — detect `?` prefix, send NL request, handle response
5. **Add debouncing** — 500ms delay, minimum query length
6. **Add caching** — moka cache with normalized query keys
7. **Add safety checks** — executable validation, destructive command warnings
8. **Add explanation mode** — Ctrl+E to explain the generated command
9. **Add dropdown variants** — multiple LLM suggestions on Down Arrow
