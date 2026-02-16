# Suggestion Algorithm Design

This document describes the redesigned suggestion algorithm for Synapse. It replaces the current provider-based pipeline with a context-aware completion system.

## Goals

Predict what the user is about to type, using every available signal: what they've typed so far, where they are, what tools they have, what they've done before, and what they're trying to accomplish.

**Design principles:**

- **Speed first.** Phase 1 must complete in <20ms. Design for the fast path — most suggestions should come from cached, pre-computed data.
- **Right type of completion.** The system must understand whether it needs a command name, a file path, a branch name, or an option flag. Suggesting `cat` when the user needs a filename is worse than suggesting nothing.
- **Degrade gracefully.** For well-known commands with specs, provide structured completions. For unknown commands, fall back to history and AI. Never show nothing when there's a reasonable guess.
- **Learn from behavior.** The system should get smarter over time by observing what users accept, dismiss, and ignore.

## Current System — Honest Evaluation

Four independent providers (History, Context, Spec, AI) each receive the raw buffer string, independently produce suggestions, and a static weighted ranker picks the best: history 0.30, ai 0.25, spec 0.15, context 0.15, recency 0.15.

**What works:**

- The two-phase model (fast sync + async AI) is fundamentally sound
- Spec tree-walking for known commands (git, cargo, npm, docker) produces accurate subcommand/option completions
- BTreeMap prefix search on history is very fast
- Context scanning of project files (Makefile, package.json, etc.) is useful
- The overall protocol and Zsh integration are solid

**What doesn't work:**

1. **Providers are blind to completion type.** Every provider gets the raw buffer and prefix-matches the whole string. The History provider doesn't know we need a branch name. The Spec provider knows it needs a file path but can't produce one. They can't coordinate because they don't share a common understanding of what's being completed.

2. **History dominates everything.** History scores are unbounded (a command run 150 times scores ~6.0) while other providers produce 0–1. After applying weights, a mediocre history match (0.30 × 4.0 = 1.2) crushes a perfect spec match (0.15 × 1.0 = 0.15).

3. **File/directory completion is absent.** The spec data model has `FilePaths`/`Directories` templates, but they produce zero actual completions. `cd `, `cat `, `vim ` get nothing from Synapse.

4. **No environment awareness.** Doesn't know what programs are on PATH, what virtualenv is active, or what tools are available.

5. **No understanding of workflows.** Each suggestion is independent. The system doesn't notice that after `git add .`, the user almost always types `git commit`.

6. **Ranking is a blunt instrument.** Static weights can't express "trust the spec at an option position, trust history at a command position." The same weights apply everywhere.

7. **Context provider is too limited.** Only does full-command prefix matching. Can't match at the word level.

8. **AI is underused.** Excluded from the dropdown entirely. Prompt includes no information about what type of completion is needed.

---

## The Redesigned Algorithm

### Core Idea: Parse, Then Complete

The fundamental change: **understand the buffer before trying to complete it.**

Before any completion source runs, a `CompletionContext` is computed that answers: *what kind of thing are we completing?* This drives everything downstream — which sources to query, how to score results, and what to show the user.

This is analogous to a language server: first parse the code to understand cursor position and expected type, then provide completions appropriate for that position.

### CompletionContext

Computed once per request (~1–2ms). Example for the buffer `git checkout f`:

```
CompletionContext:
  buffer: "git checkout f"
  tokens: ["git", "checkout", "f"]
  trailing_space: false
  partial: "f"                          // what's being typed right now
  prefix: "git checkout "               // prepend to completions
  command: "git"                        // first token
  position: Argument { index: 0 }       // what position in the command
  expected_type: Generator("git branch --no-color")
  resolved_spec: Some(git checkout spec)
  subcommand_path: ["checkout"]
  present_options: []                   // flags already typed
```

#### Position

An enum describing where we are in the command:

| Position | Example | What we're completing |
|---|---|---|
| `CommandName` | `gi▏` | The command itself → `git` |
| `Subcommand` | `git ch▏` | A subcommand → `checkout` |
| `OptionFlag` | `git commit --am▏` | A flag → `--amend` |
| `OptionValue { option }` | `git checkout -b ▏` | A flag's value → branch name |
| `Argument { index }` | `cd ▏` | A positional argument → directory |
| `PipeTarget` | `cat foo.txt \| ▏` | Command after a pipe → `grep`, `sort` |
| `Redirect` | `echo hello > ▏` | File after a redirect → file path |
| `Unknown` | (no spec, can't determine) | Best guess |

#### Expected Type

What kind of value is expected at the current position:

- `Any` — no constraint known
- `FilePath` — any file
- `Directory` — directories only
- `Executable` — executable files
- `Generator(spec)` — dynamic values from a shell command (branches, remotes, services)
- `OneOf(values)` — one of a static set
- `Hostname` — SSH hosts
- `EnvVar` — environment variable name
- `Command` — another command name (for `sudo`, `xargs`, etc.)

#### How It's Built

1. **Tokenize** the buffer. Extend the existing tokenizer to recognize `|`, `>`, `<`, `&&`, `||`, `;`.
2. **Segment** on pipe/redirect/chain operators. Only analyze the last command segment.
3. **Spec lookup.** Look up the first token in the SpecStore.
4. **If spec found:** walk the spec tree (reusing existing tree-walk logic) to determine position and expected type.
5. **If no spec:** use a **command argument table** — a hardcoded map of common commands to their argument types:

| Command | Argument type |
|---|---|
| `cd` | Directory |
| `cat`, `less`, `head`, `tail`, `vim`, `nvim`, `code`, `nano` | FilePath |
| `cp`, `mv`, `rm`, `chmod`, `chown` | FilePath |
| `mkdir`, `rmdir` | Directory |
| `python`, `python3`, `node`, `ruby`, `perl` | FilePath (first arg) |
| `sudo`, `env`, `nohup`, `time`, `watch` | Command (recurse) |
| `ssh`, `scp` | Hostname |
| `export` | EnvVar |
| After `\|` | Command |
| After `>`, `>>`, `<` | FilePath |

6. **Fallback:** single token, no trailing space → `CommandName`. Otherwise → `Unknown`.

The `Command` type for `sudo`/`env`/`time` etc. triggers recursive parsing: `sudo git ch▏` re-parses `git ch` as its own command, resolving to `Subcommand` with the git spec.

---

### Completion Sources

Instead of 4 independent providers with fixed identities, the system has **completion sources** — modules that produce candidates tagged with metadata. Only sources relevant to the current `CompletionContext` are activated.

#### Source 1: Spec

- **Activated for:** `Subcommand`, `OptionFlag`, `OptionValue`, `Argument` (when a spec exists)
- **Produces:** subcommand names, option flags, argument values from generators or static lists
- **Changes from current:**
  - Activate `arg_generator` on `OptionSpec` (currently dead code). When position is `OptionValue`, look up the option's generator and run it.
  - Implement file/directory listing for `FilePaths`/`Directories` templates. Use `tokio::fs::read_dir` with a 5s cache. Only when the spec explicitly declares the type — defer to Zsh for everything else.

#### Source 2: Filesystem

- **Activated for:** `FilePath`, `Directory`, `Redirect` expected types
- **Produces:** file and directory names from the relevant directory
- Parse the partial to extract a directory prefix (`src/ma` → list `src/`, filter for `ma`). For `Directory` type, show only directories. Add trailing `/` to directories.
- **Smart ranking:** recently modified files score higher. Files matching the expected extension for the command (`.py` for `python`, `.rs` for `cargo test --test`) get a bonus.
- **Cache:** per-directory, 5s TTL, max 200 entries

#### Source 3: History

- **Activated for:** all positions (history is always relevant)
- **Changes from current:**
  - **Position-aware matching.**
    - `CommandName`: match against first tokens of history entries.
    - `Subcommand`/`Argument`: extract the argument at the same position from history entries sharing the same command prefix. `git checkout ` + history `git checkout feature/auth` → suggest `feature/auth`, not the full command.
    - `Unknown`: fall back to full-buffer prefix match (current behavior).
  - **Normalize scores to [0, 1].** Replace unbounded `ln(freq) * recency` with: `0.6 * (ln(freq) / ln(max_freq)) + 0.4 * recency_decay`. Track `max_freq` across all entries.
  - **Include fuzzy matches in `suggest_multi`** (currently the dropdown only shows prefix matches).

#### Source 4: Environment

- **Activated for:** `CommandName`
- **Produces:** executable names from PATH
- Scan all PATH directories at daemon startup, collect names into a `HashSet<String>`. Cache with 60s TTL. Typical system: ~2000 commands across ~15 directories, <50ms to scan.
- Also used to **validate** suggestions from other sources — reject AI suggestions of nonexistent commands.
- Zsh plugin sends `VIRTUAL_ENV` in the existing `env_hints` protocol field (currently unpopulated). If set, also scan `$VIRTUAL_ENV/bin/`.

#### Source 5: Project Context

- **Activated for:** `CommandName`, `Subcommand`, `Argument` (when command matches a trigger)
- **Change:** word-level matching. Decompose stored commands into prefix + subcommand. When `ctx.command` matches a trigger, match `ctx.partial` against the subcommand part. `make t` matches target `test` because command=`make` and partial `t` matches `test`.

#### Source 6: AI

- **Activated for:** all positions (Phase 2 async for ghost text; with timeout for dropdown)
- **Changes:**
  - Include in dropdown with a 200ms timeout — proceed without it if it doesn't respond in time.
  - Pass CompletionContext to the prompt: position, expected type, command name.
  - Intent prediction: include recent_commands to predict workflows.

#### Source Activation Matrix

| Position | Spec | Filesystem | History | Environment | Context | AI |
|---|---|---|---|---|---|---|
| CommandName | names | — | first-tokens | executables | triggers | Phase 2 |
| Subcommand | subcommands | — | arg-extract | — | word-match | Phase 2 |
| OptionFlag | options | — | — | — | — | Phase 2 |
| OptionValue | generators | if file type | arg-extract | — | — | Phase 2 |
| Argument | generators/static | if file type | arg-extract | — | word-match | Phase 2 |
| PipeTarget | — | — | first-tokens | executables | — | Phase 2 |
| Redirect | — | files | — | — | — | Phase 2 |
| Unknown | — | — | full-prefix | — | full-prefix | Phase 2 |

Only activated sources run. At `OptionFlag` position, we skip Filesystem, History, Environment, and Context entirely.

---

### Scoring Model

#### All scores normalized to [0.0, 1.0]

No exceptions. Enforced with a clamp at the trait boundary. This prevents any single source from dominating by producing outsized scores.

#### Context-dependent weights

The weight of each source depends on the completion position:

| Position | Spec | Filesystem | History | Environment | Context | Recency |
|---|---|---|---|---|---|---|
| CommandName | 0.15 | — | 0.30 | 0.20 | 0.10 | 0.25 |
| Subcommand | 0.40 | — | 0.20 | — | 0.15 | 0.25 |
| OptionFlag | 0.60 | — | 0.10 | — | — | 0.30 |
| OptionValue | 0.40 | 0.20 | 0.20 | — | — | 0.20 |
| Argument (file) | 0.10 | 0.50 | 0.15 | — | — | 0.25 |
| Argument (generator) | 0.45 | — | 0.25 | — | — | 0.30 |
| Argument (other) | 0.20 | — | 0.30 | — | 0.15 | 0.35 |
| PipeTarget | — | — | 0.40 | 0.25 | — | 0.35 |
| Redirect | — | 0.60 | 0.10 | — | — | 0.30 |
| Unknown | 0.10 | — | 0.35 | — | 0.15 | 0.40 |

At positions where an authoritative source exists (spec for options, filesystem for files), that source dominates. At positions with no authoritative source (Unknown, PipeTarget), history and recency carry more weight. Recency is always significant because users tend to repeat patterns.

#### Specificity bonus

When the user has typed more of a match, that match should score higher. A suggestion matching 5 of 6 typed characters is more likely correct than one matching 1 of 6. Each source incorporates `partial.len() / match.len()` into its score.

#### Recency model

Exponential decay over recent commands: `exp(-0.3 * index)` (faster decay than the current `exp(-0.1 * index)`) so that only the last 3–5 commands have significant influence. Recency rewards recent *patterns*, not just exact repeats — if the user ran `cargo test` 2 commands ago, both `cargo test` and `cargo build` should get a boost.

---

### Workflow Prediction

A new concept: **predict the next command in a workflow.**

When the buffer is empty or very short, use the recent_commands sequence to predict what comes next. Common patterns:

- `git add .` → `git commit -m ""`
- `git commit ...` → `git push`
- `mkdir X` → `cd X`
- `cd X` → `ls` or project-specific build commands
- `cargo build` (failed) → `cargo build` (retry) or edit command
- `vim file.rs` → `cargo test` or `cargo build`

**Implementation:** A `WorkflowPredictor` that:

1. Maintains a map of `(previous_command_prefix) → Vec<(next_command, count)>` — mined from the interaction log.
2. Updated on each `preexec` event.
3. Returns top predictions when the buffer is empty.
4. Stored persistently in `~/.local/share/synapse/workflows.json`.

The statistical approach (bigram/trigram frequency table) is the primary method — fast, no API calls, improves with usage. The LLM approach is a fallback for novel sequences.

---

### LLM Strategy

Three uses, in priority order:

#### 1. Enriched prompting

The current AI prompt is generic ("suggest the single most likely command"). The new prompt includes CompletionContext:

```
You are a terminal command autocomplete engine.

Context:
- Working directory: ~/projects/myapp (Node.js project)
- Git branch: feature/auth
- Recent commands: npm test, vim src/auth.ts, npm run build
- Current input: "git commit -m "
- Completing: argument at position 0 (commit message, free text)

Suggest the most likely completion. Respond with ONLY the completed command.
```

This gives the LLM much more signal. It can produce contextually relevant commit messages, know that `npm` commands are appropriate for this project, etc.

#### 2. Intent prediction

When the buffer is empty or the user just typed a command prefix, predict intent from the recent command sequence:

```
The user just ran these commands:
1. git status
2. git add src/auth.ts
3. git add src/auth.test.ts

They are now typing: "git "

What subcommand are they most likely to type? Respond with just the complete command.
```

More valuable than simple bigram prediction because the LLM understands the semantic flow (staging files → commit).

#### 3. Spec auto-generation (future work)

When encountering an unknown command on PATH, run `command --help` in the background and use the LLM to generate a TOML spec. Cache permanently. This bootstraps spec coverage for the user's entire toolset without manual curation.

Deferred because it requires running arbitrary commands, TOML validation, and a separate cache tier.

---

### Performance

#### Phase 1 budget (<20ms)

| Step | Budget | Notes |
|---|---|---|
| CompletionContext::build() | 1–2ms | Tokenize + cached spec lookup + tree walk |
| Active sources (parallel) | 3–8ms | Only activated sources; all via `tokio::join!` |
| Ranking | <1ms | Score + dedup + sort ~20 items |
| **Total** | **5–11ms** | Well under budget |

Individual source budgets when activated:

- Spec: 2–5ms (filter from cached spec; generator cache hit <1ms)
- Filesystem: 1–3ms (readdir cache hit <1ms, miss 2–5ms on SSD)
- History: 1–3ms (BTreeMap range query)
- Environment: <1ms (HashSet lookup)
- Context: 1–2ms (cached, word-level filter)

#### Phase 2 (async AI)

Same model as today but improved:

- Debounce: 150ms (configurable)
- Buffer staleness check after debounce
- CompletionContext included in prompt
- Push `Update` if AI scores higher than Phase 1 winner

#### Caching

| Cache | Key | TTL | Rationale |
|---|---|---|---|
| Directory listing | `PathBuf` | 5s | Filesystem changes often |
| PATH executables | singleton | 60s | PATH rarely changes in a session |
| Project context | cwd `PathBuf` | 5min | Project files change infrequently |
| Spec (project) | cwd `PathBuf` | 5min | Same |
| Generator results | `(command, cwd)` | 30s | Git branches etc. change moderately |
| AI results | `(buffer_prefix, cwd, context)` | 10min | API results are expensive |
| Workflow predictions | singleton | persistent | Mined from interaction log |

#### Early exit

If CompletionContext determines only certain sources are relevant, skip the rest. At `OptionFlag` position, only Spec runs. At `Redirect`, only Filesystem. Reduces unnecessary work and lock contention.

---

### End-to-End Examples

#### `git checkout f`

1. **Parse:** tokens=`["git","checkout","f"]`, partial=`"f"`, prefix=`"git checkout "`
2. **Spec:** git → checkout subcommand → args[0] has generator `git branch --no-color`
3. **Context:** position=Argument{0}, expected=Generator
4. **Sources:** Spec runs generator (cached), filters branches for "f" → `feature/auth`, `fix/bug-123`. History extracts `feature/auth` from `git checkout feature/auth`.
5. **Ranking:** Argument+generator weights (spec=0.45, history=0.25, recency=0.30)
6. **Result:** `git checkout feature/auth`

#### `cd s`

1. **Parse:** tokens=`["cd","s"]`, partial=`"s"`, prefix=`"cd "`
2. **No spec.** Command argument table: cd → Directory
3. **Context:** position=Argument{0}, expected=Directory
4. **Sources:** Filesystem lists directories starting with "s" → `src/`, `scripts/`, `static/`. History has `cd src` (50 times), `cd scripts` (3 times).
5. **Ranking:** Argument+file weights (filesystem=0.50, history=0.15, recency=0.25)
6. **Result:** `cd src/`

#### `cat src/`

1. **Parse:** tokens=`["cat","src/"]`, partial=`"src/"`, prefix=`"cat "`
2. **Command argument table:** cat → FilePath
3. **Sources:** Filesystem lists `src/` contents. History finds `cat src/main.rs` used recently.
4. **Result:** `cat src/main.rs`

#### `make ` (with Makefile)

1. **Parse:** tokens=`["make"]`, trailing_space, partial=`""`, prefix=`"make "`
2. **Spec:** auto-generated make spec has subcommands from Makefile targets
3. **Context:** position=Subcommand
4. **Sources:** Spec provides `build`, `test`, `clean`, `install`. History: `make test` (20×), `make build` (10×). Context: also has targets from Makefile scan.
5. **Ranking:** Subcommand weights (spec=0.40, history=0.20, recency=0.25)
6. **Result:** `make test`

#### Empty buffer after `git add .`

1. **Parse:** buffer=`""`, tokens=`[]`
2. **Context:** position=CommandName
3. **Workflow predictor:** previous `git add .` → `git commit` (85% probability)
4. **Sources:** Workflow predictor, History, Environment, Context
5. **Result:** `git commit -m ""`

#### `cat foo.txt | g`

1. **Parse:** pipe detected, last segment tokens=`["g"]`, partial=`"g"`
2. **Context:** position=PipeTarget, expected=Command
3. **Sources:** History (`grep` used after pipes 200 times). Environment: `grep`, `gzip`, `git`, `go`...
4. **Ranking:** PipeTarget weights (history=0.40, environment=0.25, recency=0.35)
5. **Result:** `grep`

#### `docker compose up -d --build p`

1. **Parse:** tokens=`["docker","compose","up","-d","--build","p"]`, partial=`"p"`
2. **Spec:** docker → compose → up → args have generator `docker compose config --services`
3. **Context:** position=Argument{0}, present_options=`["--detach","--build"]`
4. **Sources:** Generator returns `postgres`, `redis` → filter for "p" → `postgres`
5. **Result:** `docker compose up -d --build postgres`

---

### Comparison with Current Architecture

| Aspect | Current | Proposed |
|---|---|---|
| Buffer understanding | None — raw string to all providers | CompletionContext parsed first |
| File completion | Dead code | Actual filesystem listing when type is known |
| Command completion | History prefix only | History + PATH scan + aliases |
| History scores | Unbounded (0–6+) | Normalized to [0, 1] |
| Ranking weights | Static | Dynamic, position-dependent |
| Argument completion | Full-buffer prefix | Position-aware argument extraction from history |
| Context matching | Full-command prefix | Word-level matching |
| AI in dropdown | Excluded | Included with 200ms timeout |
| AI prompt | Generic | Includes position, expected type, workflow |
| Workflow prediction | None | Bigram/trigram mining from interaction log |
| Pipe/redirect | Not parsed | Recognized; pipe-target and redirect completions |
| Option value completion | Dead code | Active generators |
| Environment awareness | None | Cached PATH scan, VIRTUAL_ENV detection |
| Source activation | All 4 always run | Only relevant sources per position |

---

### Implementation Order

Each step is independently valuable and shippable:

1. **Normalize history scores to [0, 1]** — biggest single improvement to ranking quality
2. **Introduce CompletionContext** — parse buffer to determine position and expected type; include command argument table
3. **Pass CompletionContext to sources** — update trait signature; sources can use it incrementally
4. **Argument-aware history** — extract argument values from history entries at the correct position
5. **Implement filesystem listing** — actual file/directory completions when expected type is known
6. **Dynamic ranking weights** — position-dependent weight tables
7. **Word-level context matching** — context source matches partial against subcommands
8. **PATH scanning** — environment source for command name completion and validation
9. **Activate option argument generators** — un-dead-code `OptionSpec.arg_generator`
10. **Pipe/redirect parsing** — extend tokenizer to recognize operators and complete appropriately
11. **Add AI to dropdown** — include AI in ListSuggestions with timeout
12. **Enriched AI prompt** — pass CompletionContext to LLM
13. **Workflow prediction** — mine interaction log for command sequences
14. **Add more built-in specs** — ls, grep, find, curl, ssh, python, pip
