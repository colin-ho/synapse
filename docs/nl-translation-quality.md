# NL Translation Quality Improvements

## Problem

The NL translation (`? query`) works but produces generic results because the prompt lacks available context. The daemon has a rich spec engine, project awareness, and interaction history — none of which feed into the NL prompt today. Specific issues:

1. **No project context beyond a type label.** The prompt says `Project type: rust` but doesn't mention available Makefile targets, npm scripts, cargo binaries, or justfile recipes. `? run tests` produces a generic `cargo test` instead of knowing the project uses `just test`.
2. **No spec awareness.** The daemon has structured specs for hundreds of commands but the NL prompt doesn't mention any flags or subcommands. The model hallucinates flags.
3. **No git context.** Queries like `? rebase onto main` don't know the current branch.
4. **No directory awareness.** `? compress the logs directory` doesn't know whether `logs/` exists.
5. **No temperature control.** The API default (1.0) is too random for command generation.
6. **No system prompt.** Everything goes in a single `user` message — no role separation, no few-shot structure.
7. **Cache key ignores project type.** `? run tests` in a Rust project and a Node project returns the same cached result.
8. **Hardcoded 26-tool list.** `extract_available_tools` misses terraform, helm, gh, poetry, uv, bun, and many others.
9. **Interaction history unused.** `interactions.jsonl` records accepted NL→command pairs but they're never fed back as few-shot examples.

## Design

### Phase 1: Low-hanging fruit (prompt + model params)

**1a. Add temperature control**

Add a `temperature` field to `OpenAIRequest`:
- NL single suggestion: `0.3`
- NL multiple suggestions: `0.7`
- Spec discovery: `0.2`

Make configurable via `[llm]` config:
```toml
[llm]
temperature = 0.3          # for NL (single)
temperature_multi = 0.7    # for NL (multiple suggestions)
```

**Key files:** `src/llm.rs` — add `temperature` to `OpenAIRequest`, wire into `call_openai`; `src/config.rs` — add config fields

**1b. Add system prompt separation**

Split the prompt into:
- `system` message: behavioral rules ("You are a shell command generator..."), output format instructions, safety rules
- `user` message: environment context + the actual query

This improves instruction-following on OpenAI-compatible APIs.

**Key files:** `src/llm.rs` — modify `build_nl_prompt` to return `(system, user)` tuple, modify `translate_command` to send two messages

**1c. Add git branch to context**

`project.rs` already has `read_git_branch_for_path`. Call it in `handle_natural_language` and include in the prompt:
```
- Git branch: feature/auth-flow
```

**Key files:** `src/daemon/handlers.rs` — call `read_git_branch_for_path`, pass to `NlTranslationContext`; `src/llm.rs` — add `git_branch: Option<String>` to `NlTranslationContext`, include in prompt

**1d. Include project_type in cache key**

Change `NlCacheKey` to include `project_type`:
```rust
struct NlCacheKey {
    normalized_query: String,
    cwd: String,
    os: String,
    project_type: String,  // NEW
}
```

**Key files:** `src/nl_cache.rs` — add field to key, update `get`/`insert` signatures; `src/daemon/handlers.rs` — pass project_type to cache calls

### Phase 2: Project context injection

**2a. Feed project scripts/targets into the prompt**

After resolving project specs (already done for `project_type`), extract command names and inject them:

```
- Project commands:
  make: build, test, clean, deploy
  npm run: dev, lint, test, build
  just: setup, migrate, seed
```

This uses `spec_store.get_project_specs(cwd)` which is already called in the completion path. Add it to the NL path too.

**Key files:**
- `src/daemon/handlers.rs` — call `spec_store.get_project_specs(cwd)`, extract command/subcommand names, pass to context
- `src/llm.rs` — add `project_commands: HashMap<String, Vec<String>>` to `NlTranslationContext`, include in prompt

**2b. Add shallow directory listing**

Run a quick `readdir` of the cwd (top-level only, max 50 entries) and include filenames:

```
- Files in cwd: src/, tests/, Cargo.toml, Makefile, README.md, logs/
```

This helps with path-relative queries without leaking file contents.

**Key files:**
- `src/daemon/handlers.rs` — add async readdir, pass to context
- `src/llm.rs` — add `cwd_entries: Vec<String>` to `NlTranslationContext`, include in prompt

### Phase 3: Spec-aware command generation

**3a. Inject relevant spec flags for mentioned tools**

When the query mentions a specific tool (e.g., `? show git branches sorted by date`), look up its spec and inject known flags:

```
- Known flags for `git branch`: --sort=<key>, --merged, --no-merged, --remote (-r), --all (-a), --list, --contains, --verbose (-v)
```

Heuristic for tool extraction: scan query tokens against `spec_store.all_command_names()`. For each match, look up the spec and include options (limit to ~20 to avoid prompt bloat).

**Key files:**
- `src/daemon/handlers.rs` — extract tool names from query, look up specs, collect options
- `src/llm.rs` — add `relevant_specs: HashMap<String, Vec<String>>` to `NlTranslationContext`, include in prompt

**3b. Post-generation flag validation**

After extracting commands from the LLM response, validate flags against known specs:

1. Parse the first token as the command name.
2. Look up its spec in the store.
3. For each `--flag` in the generated command, check if it exists in the spec's options.
4. If unknown flags are found, add a warning (don't remove — the model may know about flags the spec missed).

**Key files:**
- `src/daemon/handlers.rs` — add validation step after `extract_commands`, before returning
- `src/llm.rs` — no changes (validation happens in handler)

### Phase 4: Few-shot learning from interaction history

**4a. Read recent accepted translations from interactions.jsonl**

On daemon startup (and periodically), read the last N entries from `interactions.jsonl` where `action == "accept"` and `nl_query` is present. Store as a `Vec<(query, command)>` in `RuntimeState`.

On each NL request, select the 3-5 most relevant examples (by query similarity — simple token overlap) and inject them as few-shot examples:

```
Examples of commands you've previously generated:
Q: "find all rust files"
A: fd -e rs

Q: "show disk usage by directory"
A: du -sh */ | sort -rh

Q: "list docker containers"
A: docker ps -a
```

With system/user prompt separation (Phase 1b), these can be structured as alternating user/assistant messages for better few-shot formatting.

**Key files:**
- `src/daemon/state.rs` — add `interaction_examples: RwLock<Vec<(String, String)>>` to `RuntimeState`
- `src/logging.rs` — add `read_recent_accepted(path, limit) -> Vec<(String, String)>`
- `src/daemon/server.rs` — load examples on startup
- `src/llm.rs` — add `few_shot_examples: Vec<(String, String)>` to `NlTranslationContext`, format as prompt examples

### Phase 5: Expand available tools list

**5a. Broader PATH scan**

Replace the hardcoded 26-tool `NOTABLE` list with a broader scan:

1. Keep the notable list for prioritization.
2. Add more tools: `terraform`, `ansible`, `helm`, `gh`, `poetry`, `uv`, `pdm`, `bun`, `deno`, `act`, `mise`, `direnv`, `aws`, `gcloud`, `az`, `fly`, `railway`, `vercel`, `netlify`, `heroku`, `zig`, `gleam`, `elixir`, `mix`, `ruby`, `bundle`, `rails`, `php`, `composer`, `swift`, `xcodebuild`.
3. Alternatively, scan PATH for all executables in user-facing directories (`/usr/local/bin`, `~/.local/bin`, `~/.cargo/bin`, `~/go/bin`, etc.) and include the top N most recently accessed.

**Key files:**
- `src/daemon/handlers.rs` — expand `NOTABLE` list, or add a broader PATH scan path

### Phase 6: Fix post-processing issues

**6a. Fix destructive command detection false positives**

`"> "` matches all redirections. Tighten to only match at the start of a command or after `&&`/`||`/`;`:
```rust
// Instead of: command.contains("> ")
// Use: regex matching truncation-style redirects only
```

**6b. Fix `extract_commands` numbered-list parsing**

The `split_once(". ")` approach is fragile. Use a regex instead:
```rust
static NUM_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+\.\s+").unwrap());
```

**Key files:**
- `src/llm.rs` — fix `detect_destructive_command` patterns, fix `extract_commands` parsing

## Implementation Plan

| Step | Description | Files | Est. size |
|------|------------|-------|-----------|
| 1 | Add `temperature` to `OpenAIRequest`, wire config | `src/llm.rs`, `src/config.rs` | S |
| 2 | Split prompt into system + user messages | `src/llm.rs` | M |
| 3 | Add git branch to `NlTranslationContext` and prompt | `src/daemon/handlers.rs`, `src/llm.rs` | S |
| 4 | Add `project_type` to `NlCacheKey` | `src/nl_cache.rs`, `src/daemon/handlers.rs` | S |
| 5 | Feed project scripts/targets into NL prompt | `src/daemon/handlers.rs`, `src/llm.rs` | M |
| 6 | Add shallow cwd directory listing to prompt | `src/daemon/handlers.rs`, `src/llm.rs` | S |
| 7 | Inject relevant spec flags for mentioned tools | `src/daemon/handlers.rs`, `src/llm.rs` | M |
| 8 | Post-generation flag validation against specs | `src/daemon/handlers.rs` | M |
| 9 | Read interaction history for few-shot examples | `src/logging.rs`, `src/daemon/state.rs`, `src/daemon/server.rs` | M |
| 10 | Inject few-shot examples into prompt | `src/llm.rs` | S |
| 11 | Expand `NOTABLE` tools list | `src/daemon/handlers.rs` | S |
| 12 | Fix `detect_destructive_command` false positives | `src/llm.rs` | S |
| 13 | Fix `extract_commands` numbered-list parsing with regex | `src/llm.rs` | S |
| 14 | Tests: prompt content verification, cache key with project_type, flag validation, few-shot formatting | `tests/`, `src/llm.rs` (unit tests) | M |

## Risks and Mitigations

- **Prompt token bloat:** Adding project commands, directory listing, spec flags, and few-shot examples could exceed token limits. Mitigate by: limiting each section (max 10 project commands, max 50 dir entries, max 20 flags per tool, max 5 few-shot examples), and increasing the token budget from 512 to 1024.
- **Latency from additional context gathering:** Gathering specs, readdir, git branch adds time before the LLM call. Mitigate by running all context gathering concurrently (already done for project_root and tools via `tokio::join!`).
- **Few-shot example quality:** Old or irrelevant examples could confuse the model. Mitigate by: limiting to recent examples (last 7 days), filtering by relevance (token overlap with current query), and capping at 5 examples.
- **Over-scrubbing vs. under-scrubbing:** Adding directory listings could expose sensitive filenames (`.env`, `credentials.json`). The existing `scrub_env_keys` mechanism doesn't cover filenames. Add a filename scrub list for known sensitive patterns.
- **Flag validation false positives:** Specs may be incomplete (especially regex-parsed ones). Validation should only warn, never remove commands. Use wording like "note: --flag not found in known spec" rather than blocking.
