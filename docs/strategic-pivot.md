# Synapse Strategic Pivot: Spec Engine + NL Translation Layer

## Context

Synapse currently reimplements functionality that mature, specialized tools already handle well. This document lays out a pivot from "full replacement" to "thin layer on top of existing tools," focusing Synapse on what nothing else provides.

## 1. Competitive Landscape

### Where Synapse overlaps and will likely lose

| Capability | Synapse Today | Incumbent | Why incumbent wins |
|---|---|---|---|
| Ghost text (inline) | History + spec + fs + env providers via `POSTDISPLAY` | **zsh-autosuggestions** (30k stars) | Simpler, battle-tested, pure zsh, zero dependencies |
| Tab completion | 17 built-in TOML specs | **compsys** + hundreds of built-in `_arguments` functions | Decades of coverage, fzf-tab adds fuzzy matching + previews |
| History search | BTreeMap prefix + Levenshtein | **Atuin** (SQLite, cross-machine sync, rich metadata) | Full-text/fuzzy search, filters by cwd/exit-code/host |
| Filesystem completion | Custom readdir + quoting | **compsys** `_files` | Native, handles edge cases, no daemon overhead |
| Fuzzy dropdown | Custom dropdown via `recursive-edit` | **fzf-tab** (replaces compsys menu with fzf) | Best-in-class fuzzy matching, preview windows, multi-select |

### What Synapse uniquely offers

1. **Auto-discovered specs from `--help`** — No other tool auto-generates structured CLI completions for arbitrary commands. Carapace requires manual YAML/Go, compsys requires arcane zsh scripting, Fig required TypeScript contributions. Synapse runs `--help` and auto-generates specs.

2. **Project-aware completions** — Auto-generates completions from Makefile targets, package.json scripts, Cargo.toml targets, docker-compose services, Justfile recipes. No existing tool does this automatically.

3. **Natural language to command translation** — The `? query` prefix integrated into the shell flow. Copilot CLI is a separate conversational agent; Synapse integrates NL translation into the normal shell experience.

4. **TOML spec format** — More accessible than compsys's zsh DSL. User-authorable specs in `.synapse/specs/` for project-specific commands.

## 2. Target Architecture

Synapse pivots to a **spec engine + NL translation layer** that integrates with the existing zsh ecosystem.

```
┌─────────────────────────────────────────────────────┐
│                    User's Shell                      │
│                                                      │
│  zsh-autosuggestions  →  ghost text (history-based)  │
│  compsys + fzf-tab   →  Tab completion (spec-based) │
│  Atuin               →  Ctrl-R history search        │
│  Synapse plugin      →  ? NL translation             │
│                                                      │
│  fpath includes:                                     │
│    ~/.local/share/synapse/completions/               │
│      _rg, _fd, _just, _make*, _npm*  ...             │
│      (* = project-aware, calls daemon)               │
│                                                      │
└──────────────┬───────────────────────────────────────┘
               │ Unix socket (NL queries, dynamic completions)
               │
┌──────────────▼───────────────────────────────────────┐
│              Synapse Daemon                            │
│                                                       │
│  Spec Engine:                                         │
│    - User project specs (.synapse/specs/*.toml)       │
│    - Auto-generated from project files                │
│    - Discovered from --help → compsys files directly  │
│    - Exported as compsys _arguments functions          │
│                                                       │
│  NL Translation:                                      │
│    - LLM client (OpenAI-compatible)                   │
│    - Query caching                                    │
│    - Command blocklist                                │
│                                                       │
│  Background Tasks:                                    │
│    - Spec discovery on unknown commands               │
│    - Compsys regeneration on discovery                │
│    - Dynamic completion serving                       │
└───────────────────────────────────────────────────────┘
```

### What Synapse becomes

1. **Spec engine** — Discovers CLI specs and exports them directly as compsys `_arguments` completion functions. These work automatically with fzf-tab, zsh-autocomplete, and any other compsys-based tool.
2. **NL translator** — Keeps the `? query` natural language mode with its dedicated dropdown.
3. **Background daemon** — Handles spec discovery, project file parsing, LLM calls, and dynamic completion serving.
4. **Thin plugin** — Adds completions dir to fpath, provides NL mode, reports command execution for spec discovery.

### TOML spec simplification

The 17 built-in TOML specs (`specs/builtin/*.toml`) are removed. Every command they cover (git, cargo, docker, npm, etc.) already ships with compsys completions — with gap-only mode they would never be exported anyway.

The TOML disk cache (`~/.cache/synapse/specs/*.toml` via `spec_cache.rs`) is also removed. The useful output is the compsys function file itself — caching the intermediate TOML representation is unnecessary indirection. Discovery now writes compsys files directly to the completions directory.

**What remains of the spec system:**
- `CommandSpec` as an **in-memory IR** — the `--help` parser, project file parsers, and zsh completion parser all produce `CommandSpec` structs that get translated to compsys format. The Rust struct is a clean intermediate representation.
- **User-authored `.synapse/specs/*.toml`** — TOML remains as the user-facing authoring format for custom project commands. Writing a TOML spec is dramatically easier than authoring a compsys `_arguments` function.
- `spec_autogen.rs` — Project file parsing (Makefile, package.json, etc.) produces `CommandSpec` in-memory, exported to compsys.
- `spec_store.rs` — Simplified to 2 tiers: user project specs + project auto-generated specs. No builtin tier, no discovered-spec tier.

### What this enables

- Users keep their existing zsh-autosuggestions + fzf-tab + Atuin setup
- Synapse's auto-discovered specs make Tab completion work for tools that lack compsys functions
- Project-aware completions (make targets, npm scripts) appear in the user's normal Tab flow
- NL translation is a unique capability layered on top

## 3. Compsys Export Design

The core new capability: converting Synapse's `CommandSpec` into zsh `_arguments`-style completion functions.

### Spec → compsys mapping

#### Options

```
OptionSpec { long: "--verbose", short: "-v", description: "Verbose", takes_arg: false }
```
generates:
```zsh
'(-v --verbose)'{-v,--verbose}'[Verbose]'
```

```
OptionSpec { long: "--output", short: "-o", description: "Output file", takes_arg: true }
```
generates:
```zsh
'(-o --output)'{-o,--output}'=[Output file]:file:_files'
```

```
OptionSpec { long: "--port", short: None, description: "Port", takes_arg: true }
```
generates:
```zsh
'--port=[Port]:port:'
```

```
OptionSpec { short: "-j", long: None, description: "Jobs", takes_arg: true }
```
generates:
```zsh
'-j[Jobs]:jobs:'
```

#### Arguments

| ArgSpec | Compsys output |
|---|---|
| `{ template: FilePaths }` | `'*:file:_files'` |
| `{ template: Directories }` | `':directory:_files -/'` |
| `{ template: EnvVars }` | `':variable:_parameters -g "*(export)"'` |
| `{ suggestions: ["a", "b", "c"] }` | `':arg:(a b c)'` |
| `{ generator: { command: "..." } }` | See generators below |
| `{ variadic: true, template: FilePaths }` | `'*:file:_files'` |

#### Subcommands

Commands with subcommands use `_arguments -C` with state dispatch:

```zsh
#compdef cargo
# Auto-generated by synapse — do not edit manually
# Regenerate with: synapse generate-completions

_cargo() {
    local curcontext="$curcontext" state line
    typeset -A opt_args

    _arguments -C \
        '(-V --version)'{-V,--version}'[Print version]' \
        '(-h --help)'{-h,--help}'[Print help]' \
        '(-v --verbose)'{-v,--verbose}'[Use verbose output]' \
        '--color=[Coloring: auto, always, never]:when:(auto always never)' \
        '1:command:->cmd' \
        '*::args:->args'

    case $state in
        (cmd)
            local -a commands=(
                'build:Compile the current package'
                'test:Run tests'
                'run:Run a binary or example'
                'check:Check the current package'
                'clippy:Run clippy lints'
                'fmt:Format code'
            )
            _describe 'cargo command' commands
            ;;
        (args)
            case ${line[1]} in
                (build|b)  _cargo_build ;;
                (test|t)   _cargo_test ;;
                (run|r)    _cargo_run ;;
                # ...
            esac
            ;;
    esac
}

_cargo_build() {
    _arguments \
        '--release[Build in release mode]' \
        '--all-targets[Build all targets]' \
        '(-p --package)'{-p,--package}'=[Build only the specified package]:package:' \
        '--workspace[Build all workspace members]' \
        '--target=[Build for the target triple]:target:' \
        '(-j --jobs)'{-j,--jobs}'=[Number of parallel jobs]:jobs:' \
        '(-F --features)'{-F,--features}'=[Features to activate]:features:' \
        '--all-features[Activate all available features]' \
        '--no-default-features[Do not activate the default feature]' \
        '*:file:_files'
}

# ... more subcommand functions ...

_cargo "$@"
```

#### Generators

Two strategies depending on whether the generator needs daemon state:

**Inline generators** — For simple shell commands (git branches, ssh hosts):

```zsh
# Generator: git branch --no-color 2>/dev/null
':branch:{local -a branches; branches=(${(f)"$(git branch --no-color 2>/dev/null | sed "s/^[* ] //")"}); compadd -a branches}'
```

The `strip_prefix` field maps to a `sed` command in the generated code. The `split_on` field maps to the `(f)` (newline) or `(s:X:)` (custom delimiter) parameter expansion flag.

**Daemon-backed generators** — For project-aware completions that benefit from the daemon's caching:

```zsh
# Helper function for daemon-backed completions
_synapse_complete() {
    local -a results
    results=(${(f)"$(synapse complete "$1" "${@:2}" 2>/dev/null)"})
    [[ ${#results} -gt 0 ]] && compadd -a results
}
```

Used like:

```zsh
# In generated _make function:
':target:_synapse_complete make target'
```

The daemon handles `synapse complete make target` by looking up the project-auto spec for `make` in the current directory and returning Makefile targets.

#### Recursive commands

Commands with `recursive: true` (like `sudo`, `env`) dispatch to `_normal` after consuming their own options:

```zsh
_sudo() {
    _arguments \
        '-u[Run as user]:user:_users' \
        '-i[Login shell]' \
        '(-):command:_command_names -e' \
        '*::args:_normal'
}
```

#### Aliases

Subcommand aliases are handled by matching multiple patterns in the case dispatch:

```zsh
case ${line[1]} in
    (build|b)  _cargo_build ;;
    (test|t)   _cargo_test ;;
esac
```

Command-level aliases generate `compdef` aliases:

```zsh
compdef _cargo cargo
# If aliases: ["cg"]
compdef _cargo cg
```

### Gap detection

Before generating, check which commands already have compsys completions:

```rust
// Already exists in src/zsh_completion.rs
let existing = zsh_completion::scan_available_commands();

for spec in all_specs {
    if !existing.contains(&spec.name) {
        // Generate completion — this command has no existing compsys function
        compsys_export::write_completion_file(&spec, &output_dir)?;
    }
}
```

This uses the existing `scan_available_commands()` function which does a sub-millisecond `readdir` across all fpath directories.

**Exception**: Project-auto specs (Makefile, package.json, etc.) are always generated regardless of gap detection, since they provide project-specific completions that system-level compsys functions don't have.

### Output location

Generated completions go to `~/.local/share/synapse/completions/` (XDG data dir). This directory is added to `fpath` by the shell init code.

Each file is named `_<command>` (e.g., `_rg`, `_fd`, `_just`). Files include a header comment:

```zsh
#compdef rg
# Auto-generated by synapse — do not edit manually
# Source: discovered (parsed from --help)
# Generated: 2026-02-18T10:30:00Z
# Regenerate with: synapse generate-completions --force
```

## 4. Protocol Changes

### New request: `Complete`

For daemon-backed dynamic completions (called from generated compsys functions via `synapse complete`):

```json
{
    "type": "complete",
    "command": "make",
    "context": ["target"],
    "cwd": "/Users/colin/myproject"
}
```

### New response: `CompleteResult`

```
complete_result\t3\tclean\tRemove build artifacts\tbuild\tCompile the project\ttest\tRun test suite
```

(TSV format matching existing response conventions: type, count, then value/description pairs.)

### Removed request types (in Phase 2)

- `Suggest` — Ghost text suggestions (replaced by zsh-autosuggestions)
- `ListSuggestions` — Dropdown candidates (replaced by compsys + fzf-tab)
- `Interaction` — Accept/dismiss/ignore tracking (no longer relevant without ghost text)

### Kept request types

- `NaturalLanguage` — NL → command translation (unique value)
- `CommandExecuted` — Triggers spec discovery for unknown commands
- `Complete` — Dynamic completion values for generated compsys functions
- `Ping`, `Shutdown`, `ReloadConfig`, `ClearCache` — Control messages

## 5. What Gets Removed

### Plugin (`plugin/synapse.zsh`)

| Remove | Lines (approx) | Reason |
|---|---|---|
| Ghost text rendering (`_synapse_show_suggestion`, `POSTDISPLAY` logic, `region_highlight`) | ~50 | zsh-autosuggestions handles this |
| General dropdown (`_synapse_dropdown_open` trigger, `_synapse_render_dropdown` for suggestions) | ~140 | compsys + fzf-tab handles this |
| Suggestion widgets (`_synapse_self_insert`, `_synapse_backward_delete_char`, debounce) | ~80 | No longer intercepting keystrokes for suggestions |
| Suggestion request building (`_synapse_build_suggest_request`, `_synapse_build_list_request`) | ~60 | No more Suggest/ListSuggestions protocol |
| Async suggestion handler (`_synapse_handle_update`) | ~30 | No more Phase 2 async updates |
| Interaction reporting (`_synapse_report_interaction`) | ~20 | No accept/dismiss tracking |
| Ghost text state variables (`_SYNAPSE_CURRENT_SUGGESTION`, debounce vars) | ~15 | |
| Widget overrides (`self-insert`, `backward-delete-char`, arrow key bindings for suggestions) | ~30 | |

**Plugin goes from ~1200 lines to ~770 lines.** Retained: NL mode, command reporting, connection management, daemon lifecycle, fpath setup.

### Daemon (Rust)

| Remove | File | Reason |
|---|---|---|
| History provider | `src/providers/history.rs` | Atuin / zsh-autosuggestions |
| Filesystem provider | `src/providers/filesystem.rs` | compsys `_files` |
| Environment provider | `src/providers/environment.rs` | compsys `_command_names` |
| Workflow provider (bigram) | `src/providers/workflow.rs` | Doesn't fit compsys model |
| Workflow LLM provider | `src/providers/workflow_llm.rs` | Doesn't fit compsys model |
| LLM argument provider | `src/providers/llm_argument.rs` | Over-engineered for marginal value |
| Spec provider | `src/providers/spec.rs` | Replaced by compsys export + `Complete` handler |
| Provider trait + dispatch | `src/providers/mod.rs` | No more provider system |
| Ranking system | `src/ranking.rs` | No multi-provider ranking needed |
| Workflow predictor | `src/workflow.rs` | Removed with workflow provider |
| Completion context builder | `src/completion_context/builder.rs` | Only needed for provider pipeline |
| Built-in TOML specs | `specs/builtin/*.toml` (17 files) | All covered by existing compsys functions; gap-only mode means they'd never be exported |
| Spec disk cache | `src/spec_cache.rs` | Compsys files are the cache; TOML intermediate is unnecessary |

### Config

Remove sections: `[history]`, `[workflow]`, `[weights]`, `ghost_text_color`.

## 6. What Gets Kept

| Component | File(s) | Why |
|---|---|---|
| Spec data model | `src/spec.rs` | In-memory IR — CommandSpec, OptionSpec, ArgSpec, GeneratorSpec |
| Spec store (simplified) | `src/spec_store.rs` | 2-tier resolution (user project specs + project auto-gen), generator execution |
| Spec auto-generation | `src/spec_autogen.rs` | Makefile, package.json, Cargo.toml, docker-compose, Justfile parsing |
| Zsh completion scanner | `src/zsh_completion.rs` | Gap detection + inbound spec parsing from `--help` |
| LLM client | `src/llm.rs` | NL translation + spec discovery enrichment |
| NL cache | `src/nl_cache.rs` | Caching NL translation results |
| Session manager | `src/session.rs` | Per-session state for NL context |
| Interaction logger | `src/logging.rs` | Analytics (simplified) |
| Project detection | `src/project.rs` | Project root/type detection |
| Config (simplified) | `src/config.rs` | Spec, LLM, completions, security, logging config |
| Daemon server | `src/daemon/server.rs` | Unix socket server |
| Daemon lifecycle | `src/daemon/lifecycle.rs` | Start/stop/daemonize |
| Shell init | `src/daemon/shell.rs` | Plugin sourcing + fpath setup |
| Probe tool | `src/daemon/probe.rs` | Debugging |
| Tokenizer | `src/completion_context/tokenizer.rs` | Needed by `Complete` handler |

## 7. What Gets Added

### `src/compsys_export.rs` — Spec → compsys conversion

Core module that converts `CommandSpec` into zsh `_arguments` completion function text.

```rust
/// Generate a complete zsh completion function from a CommandSpec.
pub fn export_command_spec(spec: &CommandSpec) -> String

/// Write the completion function to a file in the given directory.
/// Returns the path to the written file.
pub fn write_completion_file(spec: &CommandSpec, dir: &Path) -> io::Result<PathBuf>

/// Return the default completions directory (~/.local/share/synapse/completions/).
pub fn completions_dir() -> PathBuf

/// Generate completions for a list of specs, skipping commands with existing compsys functions.
pub fn generate_all(
    specs: &[CommandSpec],
    existing_commands: &HashSet<String>,
    output_dir: &Path,
    gap_only: bool,
) -> io::Result<GenerationReport>
```

### CLI commands

```
synapse generate-completions [--output-dir PATH] [--force] [--no-gap-check]
```

Generates compsys completion files for all known specs. With `--force`, regenerates even if files exist. With `--no-gap-check`, generates even for commands with existing compsys functions.

```
synapse complete <command> [context...] [--cwd PATH]
```

Queries the running daemon for dynamic completion values. Used by generated compsys functions for project-aware completions. Outputs one value per line (with optional `\t` description).

### `Complete` protocol message and handler

New handler in `src/daemon/handlers.rs` that resolves specs via the spec store, walks the subcommand path, and returns values from generators or static suggestions.

### fpath integration in shell init

Shell init code (`src/daemon/shell.rs`) adds the completions directory to fpath:

```zsh
# Synapse completions
_synapse_completions_dir="${XDG_DATA_HOME:-$HOME/.local/share}/synapse/completions"
[[ -d "$_synapse_completions_dir" ]] && fpath=("$_synapse_completions_dir" $fpath)
```

### Config additions

```toml
[completions]
output_dir = "~/.local/share/synapse/completions"  # override default
gap_only = true        # only generate for commands without existing compsys functions
auto_regenerate = true # regenerate when new specs are discovered
```

## 8. Implementation Plan

### Phase 0: Add compsys export (additive, no removals)

Everything still works as before. This phase only adds new capability.

**New files:**
- `src/compsys_export.rs` — Core export logic

**Modified files:**
- `src/lib.rs` — Add `pub mod compsys_export;`
- `src/daemon/mod.rs` — Add `GenerateCompletions` and `Complete` CLI subcommands
- `src/protocol.rs` — Add `Complete` request/response types
- `src/daemon/handlers.rs` — Add `handle_complete()` function
- `src/daemon/shell.rs` — Add fpath setup to init code
- `src/config.rs` — Add `CompletionsConfig`

**Tests:**
- Unit tests in `compsys_export.rs` for each spec construct (options, subcommands, args, generators, templates, aliases, recursive)
- Integration test: construct a `CommandSpec` in-memory → export → verify output contains expected `_arguments` patterns
- Integration test: `synapse complete` round-trip through daemon

**Deliverable:** `synapse generate-completions` works and produces valid compsys functions. Users can manually run it to try the new system while keeping the existing suggestion flow.

### Phase 1: Slim the plugin

Remove ghost text, general dropdown, and suggestion widgets. Keep NL mode and command reporting.

**Modified files:**
- `plugin/synapse.zsh` — Major reduction (~1200 → ~770 lines)

**Removals in plugin:**
- `_synapse_show_suggestion()`, `_synapse_clear_suggestion()`
- `_synapse_line_pre_redraw()` (pre-redraw hook)
- `_synapse_suggest()`, `_synapse_suggest_or_nl()` (simplify to NL-only check)
- `_synapse_self_insert()`, `_synapse_backward_delete_char()` widget overrides
- `_synapse_accept()` (right-arrow accept)
- `_synapse_handle_update()` (async suggestion updates)
- `_synapse_build_suggest_request()`, `_synapse_build_list_request()`
- `_synapse_report_interaction()`
- `_synapse_dropdown_open()` (Down arrow trigger for general dropdown)
- All `POSTDISPLAY`/`region_highlight` ghost text manipulation
- All debounce state variables
- Ghost text keybindings (Tab accept, right-arrow accept, Escape dismiss)
- Widget registrations: `self-insert`, `backward-delete-char`, `zle-line-pre-redraw`

**Kept in plugin:**
- Connection management (zsocket, reconnect)
- Session ID generation
- `_synapse_precmd` / `_synapse_preexec` hooks (simplified, for command reporting)
- `command_executed` reporting (triggers spec discovery)
- NL prefix detection (`? query`)
- NL request/response/dropdown flow (reuses dropdown rendering)
- `synapse-dropdown` keymap (for NL results only)
- Daemon lifecycle (ensure running)
- fpath setup (from Phase 0)

**Tests:** Manual verification that NL mode still works, Tab falls through to normal compsys, ghost text is gone.

### Phase 2: Slim the daemon

Remove the provider system, ranking, and suggestion pipeline.

**Deleted files:**
- `src/providers/history.rs`
- `src/providers/filesystem.rs`
- `src/providers/environment.rs`
- `src/providers/workflow.rs`
- `src/providers/workflow_llm.rs`
- `src/providers/llm_argument.rs`
- `src/providers/spec.rs`
- `src/providers/mod.rs`
- `src/ranking.rs`
- `src/workflow.rs`
- `src/completion_context/builder.rs`
- `src/spec_cache.rs`
- `specs/builtin/*.toml` (all 17 files)

**Modified files:**
- `src/lib.rs` — Remove `pub mod providers;`, `pub mod ranking;`, `pub mod workflow;`, `pub mod spec_cache;`
- `src/protocol.rs` — Remove `Suggest`, `ListSuggestions`, `Interaction` request types and related structs. Remove `Suggestion`, `Update`, `SuggestDone` response types. Remove unused `SuggestionSource` variants.
- `src/daemon/handlers.rs` — Remove `handle_suggest()`, `handle_list_suggestions()`, `handle_interaction()`, `Phase2UpdatePlan`, `SuggestHandling`, `collect_provider_suggestions()`, `spawn_phase2_update()`, phase deadline constants. Simplify `handle_command_executed()` to only trigger discovery.
- `src/daemon/state.rs` — Remove `providers`, `phase2_providers`, `ranker`, `workflow_predictor` fields.
- `src/daemon/lifecycle.rs` — Remove provider initialization, workflow predictor setup. Add: trigger `generate_all` on startup.
- `src/spec_store.rs` — Simplify from 4-tier to 2-tier resolution (user project specs + project auto-gen). Remove builtin spec loading (`include_str!` embeds), remove discovered-spec tier, remove `spec_cache` integration. Discovery now writes compsys files directly via `compsys_export` instead of caching TOML.
- `src/config.rs` — Remove `HistoryConfig`, `WorkflowConfig`, weight constants, `ghost_text_color`. Keep `SpecConfig`, `LlmConfig`, `SecurityConfig`, `LoggingConfig`. Add `CompletionsConfig`.
- `src/completion_context/mod.rs` — Simplify to just re-export tokenizer (needed by `Complete` handler).
- `config.example.toml` — Already updated: `[history]`, `[workflow]`, `[weights]`, `ghost_text_color` removed; `[completions]` added.

**Tests:** Update `tests/integration_tests.rs` — remove Suggest/ListSuggestions protocol tests, add Complete protocol tests. Ensure NL flow and daemon lifecycle tests still pass.

### Phase 3: Enhance auto-regeneration

Make the spec engine automatically export compsys functions when specs change.

**Modified files:**
- `src/daemon/handlers.rs` — In `handle_command_executed()`, after discovery completes, write compsys file directly via `compsys_export::write_completion_file()` if `auto_regenerate` is enabled.
- `src/daemon/lifecycle.rs` — On startup, run `generate_all` in a background task so it doesn't block the socket listener.

**Deliverable:** Users get new compsys completions automatically when they run an unknown command for the first time. The daemon discovers the spec from `--help` and generates a compsys function directly in the completions directory. The next time the user opens a new shell (or runs `compinit`), the new completions are available.

### Phase 4: Cleanup

**Cargo.toml** — Remove unused dependencies: `strsim` (Levenshtein), any others only used by removed providers.

**Tests** — Remove provider unit tests, ranking tests. Add comprehensive compsys export tests.

**Docs** — Update `CLAUDE.md` architecture section, `README.md`, `config.example.toml`.

**Migration guide** — Add section to README:
- `eval "$(synapse)"` now adds completions to fpath instead of rendering ghost text
- Recommended companion tools: zsh-autosuggestions (ghost text), fzf-tab (fuzzy Tab menu), Atuin (history search)
- Old config keys (`[history]`, `[workflow]`, `[weights]`) are silently ignored
- `~/.cache/synapse/specs/` can be deleted (no longer used; discovered specs now write directly to `~/.local/share/synapse/completions/`)
- `? query` NL mode works exactly as before

## 9. Estimated Impact

| Metric | Before | After |
|---|---|---|
| Rust LOC | ~14,400 | ~6,500 |
| Zsh plugin LOC | ~1,200 | ~770 |
| Rust source files | 35 | ~20 |
| Provider count | 8 | 0 (replaced by compsys export) |
| Protocol request types | 9 | 7 |
| Config sections | 8 | 5 |
| Built-in TOML specs | 17 | 0 (removed — gap-only means they'd never export) |
| Spec tiers | 4 | 2 (user project + project auto-gen) |

## 10. Open Questions

1. **Should `synapse generate-completions` run automatically on daemon startup?** Proposed: yes, in a background task. But this means the first shell session after install might not have completions until the daemon finishes generating. Alternative: generate synchronously during `synapse install`.

2. **Should the NL dropdown integrate with fzf?** The current NL dropdown is custom (rendered via `POSTDISPLAY` + `recursive-edit`). It could potentially pipe results to fzf for a more polished UX. This is orthogonal to the pivot and could be done later.
