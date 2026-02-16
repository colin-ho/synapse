# LLM-Powered Contextual Argument Suggestion

## Overview

Use an LLM to suggest contextually appropriate argument values when the spec system and history have nothing useful to offer. This covers cases where the expected argument value depends on project state, recent commands, or domain knowledge that static specs can't capture.

## Problem

The current system suggests argument values from three sources:

1. **Generators** (`GeneratorSpec`) — shell commands like `git branch --no-color` that produce dynamic values. These are powerful but require someone to write the generator command in a spec.
2. **Templates** (`ArgTemplate`) — `file_paths`, `directories`, `env_vars`. These handle filesystem arguments but nothing else.
3. **Static suggestions** — hardcoded values in spec files (e.g., `suggestions = ["development", "production"]`).

When none of these apply, the system returns nothing. Common gaps:

| Command position | What the user needs | What Synapse offers |
|---|---|---|
| `git commit -m "` | A commit message based on staged changes | Nothing |
| `curl -H "` | Common HTTP headers | Nothing |
| `docker run --name ` | A name based on the image | Nothing |
| `ssh ` (no spec) | Hostnames from `~/.ssh/config` | Nothing (unless spec exists) |
| `grep -r "` | Patterns relevant to the project | Nothing |
| `git tag ` | A version number based on the project | Nothing |
| `aws s3 cp ` | S3 bucket paths | Nothing |

The spec system can't cover these because the right answer depends on runtime context that no static spec can predict.

## Design

### Architecture

The LLM argument provider runs as an async Phase 2 provider — it doesn't block the fast synchronous path. It only activates when:

1. The `CompletionContext` position is `Argument` or `OptionValue`
2. The expected type is `Any` (no generator, template, or static suggestions available)
3. An LLM client is configured

```
Phase 1 (sync, <20ms):
  Spec provider → generator/template/static suggestions
  History provider → argument extraction from history
  Filesystem provider → file paths (if expected type is FilePath/Directory)

Phase 2 (async, 200-800ms):
  LLM argument provider → contextual value suggestion
  → Push Update response if it beats Phase 1
```

### LLM Argument Provider

A new provider that only activates at argument/option-value positions when other providers lack useful data:

```rust
pub struct LlmArgumentProvider {
    client: Arc<LlmClient>,
}

impl SuggestionProvider for LlmArgumentProvider {
    async fn suggest(&self, request: &ProviderRequest, max: NonZeroUsize) -> Vec<ProviderSuggestion> {
        let ctx = request.completion();

        // Only activate for argument/option-value positions with no other source
        if !matches!(ctx.position, Position::Argument { .. } | Position::OptionValue { .. }) {
            return Vec::new();
        }
        if !matches!(ctx.expected_type, ExpectedType::Any) {
            return Vec::new();  // Other providers can handle this
        }

        self.client.suggest_argument(ctx, request).await
    }
}
```

### Prompt Design

The prompt is position-aware — it tells the LLM exactly what kind of value is needed:

```
You are a terminal argument value predictor. Suggest the most likely value for the current argument position.

Command: {command} {subcommand_path}
Current option: {option_name}  (if OptionValue position)
Argument position: {index}     (if Argument position)
Partial input: "{partial}"
Working directory: {cwd}
Recent commands: {recent_commands}

Context:
{additional_context}

Respond with ONLY the argument value (no quotes, no explanation). If multiple values are likely, separate them with newlines (max 5).
```

### Context Enrichment

The key to useful argument suggestions is providing the right context. Different command positions need different context:

#### Git Commit Messages

When position is `OptionValue` for `git commit -m`:

```rust
// Gather staged diff
let diff = Command::new("git")
    .args(["diff", "--staged", "--stat"])
    .current_dir(cwd)
    .output().await?;
let diff_detail = Command::new("git")
    .args(["diff", "--staged", "--no-color"])
    .current_dir(cwd)
    .output().await?;
// Truncate to ~2000 tokens
```

Additional context:
```
Git staged changes:
{diff --stat output}

Diff preview (truncated):
{first 2000 chars of diff}
```

#### Docker Operations

When command is `docker` and argument involves image/container names:

```rust
let containers = Command::new("docker")
    .args(["ps", "--format", "{{.Names}}"])
    .output().await?;
let images = Command::new("docker")
    .args(["images", "--format", "{{.Repository}}:{{.Tag}}"])
    .output().await?;
```

#### HTTP Headers/URLs

When command is `curl` or `wget` and argument is after `-H`:

No additional context needed — the LLM knows common HTTP headers. Additional context from the command buffer (e.g., the URL being requested) is already available in `recent_commands`.

#### SSH Hosts

When command is `ssh`, `scp`, or `sftp`:

```rust
let ssh_config = std::fs::read_to_string(
    dirs::home_dir().unwrap().join(".ssh/config")
)?;
// Extract Host entries
```

### Context Gathering Registry

A registry of context-gathering functions keyed by command name:

```rust
type ContextGatherer = Box<dyn Fn(&Path) -> BoxFuture<String> + Send + Sync>;

struct ContextRegistry {
    gatherers: HashMap<String, ContextGatherer>,
}

impl ContextRegistry {
    fn new() -> Self {
        let mut r = Self { gatherers: HashMap::new() };
        r.register("git", gather_git_context);
        r.register("docker", gather_docker_context);
        r.register("ssh", gather_ssh_context);
        r.register("kubectl", gather_k8s_context);
        r
    }

    async fn gather(&self, command: &str, cwd: &Path) -> String {
        if let Some(gatherer) = self.gatherers.get(command) {
            gatherer(cwd).await
        } else {
            String::new()
        }
    }
}
```

This is extensible — adding context for a new command is just registering a new gatherer function.

### Caching

Argument suggestions are cached aggressively since the same position often recurs:

- **Cache key:** `(command, subcommand_path, option_or_arg_index, partial, context_hash)`
- **TTL:** 60 seconds (context changes frequently)
- **Max entries:** 200

The `context_hash` prevents stale suggestions when the project state changes (e.g., different staged files → different commit message).

### High-Value Argument Scenarios

Ordered by impact and frequency:

#### Tier 1: Git commit messages

- **Trigger:** `git commit -m "` or `git commit -m '`
- **Context:** `git diff --staged`
- **Expected output:** A concise commit message describing the staged changes
- **Special handling:** The LLM response replaces the opening quote; the closing quote is appended. If the partial already contains text, the LLM completes it rather than replacing it.

#### Tier 2: Branch/tag names

- **Trigger:** `git checkout -b `, `git tag `
- **Context:** Current branch name, recent commits, project conventions
- **Expected output:** A branch name following the project's naming convention (e.g., `feat/`, `fix/`, `release/`)

#### Tier 3: Docker arguments

- **Trigger:** `docker run --name `, `docker exec -it `
- **Context:** Running containers, available images
- **Expected output:** Container/image names

#### Tier 4: Generic option values

- **Trigger:** Any option with `takes_arg = true` and no generator/template
- **Context:** Option description from the spec, command context
- **Expected output:** Likely values based on the option's description

### Config

```toml
[llm]
# ... (shared)
contextual_args = true             # enable LLM argument suggestions
arg_context_timeout_ms = 2000      # timeout for context gathering commands
arg_max_context_tokens = 3000      # max context tokens sent to LLM
```

### Ranking

LLM argument suggestions enter the ranking system with `source: SuggestionSource::Llm`. Position weights for `Llm` at argument positions:

| Position | Llm weight |
|---|---|
| Argument (no other source) | 0.60 |
| Argument (with generator) | 0.15 |
| OptionValue (no other source) | 0.55 |
| OptionValue (with generator) | 0.10 |
| All other positions | 0.0 |

When a generator or template exists, the LLM weight drops sharply — deterministic sources are preferred when available.

### Security

- Context gathering commands (e.g., `git diff`) are hardcoded and non-configurable — users can't inject arbitrary commands through this mechanism
- `git diff` output is scrubbed through `security.scrub_paths` before sending to the LLM
- SSH config parsing only extracts `Host` entries, not keys or passwords
- Docker context only includes container/image names, not environment variables or volumes
- All LLM-suggested values are treated as untrusted — they're presented as suggestions, never auto-executed

### Cost Analysis

Contextual argument suggestions fire only at specific positions where no other provider has data. The commit message case is by far the most common:

| Scenario | Calls/day | Input tokens | Output tokens | Daily cost |
|---|---|---|---|---|
| Commit messages (5/day) | 5 | ~10000 | ~250 | ~$0.005 |
| Docker args (2/day) | 2 | ~1000 | ~50 | ~$0.001 |
| Other option values (5/day) | 5 | ~2500 | ~100 | ~$0.002 |

### Protocol Changes

Reuses the `Llm` source variant added by the natural language feature. No new request types — contextual arguments are triggered by regular `Suggest` and `ListSuggestions` requests when the position and expected type conditions are met.

### Implementation Order

1. **Add LlmArgumentProvider** — skeleton provider with position/type gating
2. **Add context registry** — git commit message context first (highest value)
3. **Wire into Phase 2** — async delivery via Update response
4. **Add git commit message generation** — end-to-end with `git diff --staged`
5. **Add SSH host suggestion** — parse `~/.ssh/config`
6. **Add Docker context** — container/image names
7. **Add generic option value suggestion** — use spec descriptions as context
8. **Add caching** — context-hash-keyed cache with 60s TTL
