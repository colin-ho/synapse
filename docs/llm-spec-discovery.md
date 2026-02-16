# LLM-Powered Spec Discovery

## Overview

Replace the regex-based `--help` parser (`src/help_parser.rs`) with an LLM-powered parser that can understand arbitrary help text formats and produce richer `CommandSpec` output. The LLM runs only during background spec discovery — zero latency impact on interactive suggestions.

## Problem

The current `parse_help_output()` function uses regex patterns to extract subcommands, options, and descriptions from `--help` output. It handles common formats (clap, cobra, argparse, click) but fails on:

- Non-standard indentation or formatting
- Tools that use custom help layouts (e.g., `ffmpeg`, `imagemagick`, `aws`)
- Nested option groups or conditional flags
- Argument type inference (the regex parser detects `takes_arg` from value hints like `<FILE>` but never sets `template` or `generator` fields)
- Mutually exclusive options (`exclusive_with` is defined in `OptionSpec` but never populated)
- Man-page style help that uses different conventions

The regex parser produces specs with ~60-70% coverage for well-structured help and near 0% for non-standard formats. Options always have `template: None` and `arg_generator: None`, meaning discovered specs provide subcommand/option name completion but no argument value completion.

## Design

### Architecture

The LLM parser slots into the existing discovery pipeline as an alternative backend. No changes to the spec data model, storage, or resolution logic.

```
trigger_discovery()
  → discover_command_impl()
    → run_help_command()           [unchanged]
    → parse_help_output()          [current regex parser]
    → llm_parse_help_output()      [new — replaces regex when configured]
    → spec_cache::save_discovered() [unchanged]
```

### LLM Provider Abstraction

A new `src/llm.rs` module provides a thin client abstraction:

```rust
pub struct LlmClient {
    provider: LlmProvider,
    api_key: String,
    model: String,
    timeout: Duration,
}

pub enum LlmProvider {
    Anthropic,
    OpenAI,
}

impl LlmClient {
    /// Send a structured prompt and parse the response as TOML.
    pub async fn generate_spec(
        &self,
        command_name: &str,
        help_text: &str,
    ) -> Result<CommandSpec, LlmError>;
}
```

The client is constructed once at daemon startup (if an API key is configured) and shared via `Arc<LlmClient>`. If no API key is set, the system falls back to the regex parser — no degradation.

### Prompt Design

The prompt asks the LLM to return a TOML spec directly, matching the existing `CommandSpec` schema:

```
Parse this CLI help text into a TOML command spec.

Command name: {command_name}

Help text:
```
{help_text}
```

Return ONLY valid TOML matching this schema:

name = "command_name"
description = "..."

[[subcommands]]
name = "subcommand_name"
description = "..."

[[options]]
long = "--flag-name"
short = "-f"            # omit if none
description = "..."
takes_arg = true/false

  [options.arg_generator]          # only if the value is dynamic
  command = "shell command"        # e.g., "git branch --no-color"

[[args]]
name = "arg_name"
description = "..."
template = "file_paths"   # or "directories" if the arg expects dirs

Rules:
- Set takes_arg = true when the option requires a value (indicated by <VALUE>, =VALUE, or uppercase placeholder)
- Set template = "file_paths" when an argument clearly expects file paths (FILE, PATH, FILENAME)
- Set template = "directories" when an argument clearly expects directories (DIR, DIRECTORY)
- Omit --help and --version options
- For subcommand aliases (e.g., "checkout, co"), use: aliases = ["co"]
- Include arg_generator only when you can infer a reliable shell command for dynamic values
```

The prompt is deliberately constrained: the LLM outputs TOML that we can parse with the same `toml::from_str::<CommandSpec>()` used everywhere else. If parsing fails, fall back to the regex parser.

### Integration into SpecStore

```rust
// In spec_store.rs — discover_command_impl()

let mut spec = if let Some(ref llm) = self.llm_client {
    match llm.generate_spec(command, &help_text).await {
        Ok(spec) => spec,
        Err(e) => {
            tracing::debug!("LLM parse failed for {command}, falling back to regex: {e}");
            help_parser::parse_help_output(command, &help_text)
        }
    }
} else {
    help_parser::parse_help_output(command, &help_text)
};
```

### Subcommand Recursion

For subcommand discovery, the current system runs `command subcommand --help` for each subcommand and parses individually. With LLM parsing, each subcommand's help text gets its own LLM call. To control costs:

- Rate-limit LLM calls to 1/second during recursive discovery
- Cap total LLM calls per discovery run at 20 (configurable)
- Fall back to regex for any subcommand beyond the cap

### Config

New fields in `config.toml` under `[llm]`:

```toml
[llm]
enabled = false                    # master switch
provider = "anthropic"             # "anthropic" or "openai"
api_key_env = "ANTHROPIC_API_KEY"  # env var name containing the key
base_url = ""                      # optional base URL (e.g. "http://127.0.0.1:1234" for LM Studio)
model = "claude-haiku-4-5-20251001"  # fast + cheap for structured extraction
timeout_ms = 10000                 # per-request timeout
max_calls_per_discovery = 20       # cap LLM calls during recursive discovery
```

The API key is read from an environment variable (never stored in the config file). If the env var is unset, LLM features are disabled silently.

For local OpenAI-compatible endpoints (like LM Studio on `http://127.0.0.1:1234`), Synapse accepts missing keys and uses a placeholder bearer token.

### Cost Analysis

Spec discovery runs infrequently — once per unknown command, then cached for 7 days (configurable). Typical `--help` output is 500-3000 tokens. Using Claude Haiku:

| Scenario | Input tokens | Output tokens | Cost |
|---|---|---|---|
| Single command | ~1500 | ~500 | ~$0.001 |
| Command + 10 subcommands | ~15000 | ~5000 | ~$0.01 |
| First week (50 new commands) | ~75000 | ~25000 | ~$0.05 |

After the first week, nearly all commands are cached and the ongoing cost is effectively zero.

### Error Handling

1. **LLM returns invalid TOML** → fall back to regex parser, log warning
2. **LLM timeout** → fall back to regex parser
3. **API error (rate limit, auth failure)** → fall back to regex parser, disable LLM for 5 minutes (backoff)
4. **Malformed spec (e.g., wrong command name)** → correct the name field, use spec if otherwise valid

### Security

- The LLM sees `--help` output only — no user data, no commands, no file contents
- Help text is already public information (it's printed by the tool itself)
- API keys are read from env vars, never persisted to disk by Synapse
- The `security.scrub_paths` setting is applied to help text before sending to the LLM

### Testing

- Add integration tests that compare LLM-generated specs against known-good specs for clap, cobra, argparse, and click tools
- Add a `--dry-run-llm-parse` CLI flag for development that reads help text from stdin and prints the generated spec
- Existing `help_parser.rs` tests remain as the regression suite for the regex fallback

### Quality Improvements Over Regex

| Aspect | Regex parser | LLM parser |
|---|---|---|
| Subcommand detection | Pattern-matched indented words | Understands any format |
| Option `takes_arg` | Value hint patterns (`<>`, `=`) | Semantic understanding from descriptions |
| Argument `template` | Never set | Infers `file_paths`/`directories` from context |
| `exclusive_with` | Never set | Can infer from "cannot be used with" descriptions |
| Aliases | Comma-separated on same line | Understood from any format |
| Non-standard help | Fails silently | Handles custom layouts |
| Description quality | Raw text extraction | Clean, normalized descriptions |

### Implementation Order

1. Add `LlmClient` struct and `LlmConfig` to config
2. Add HTTP client dependency (`reqwest`) to Cargo.toml
3. Implement `generate_spec()` with prompt + TOML parsing + fallback
4. Wire into `discover_command_impl()` behind config flag
5. Add rate limiting and cost controls
6. Add integration tests with real help text fixtures
