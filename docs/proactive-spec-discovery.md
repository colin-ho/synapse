# Proactive Spec Discovery

## Problem

Spec discovery is purely reactive today — it only runs after the user executes a command (`preexec` → `command_executed` → `trigger_discovery`). This means:

1. The first time you run `kubectl`, there are no completions. Discovery fires in the background, but you don't benefit until the next shell session (when zsh reloads `fpath`).
2. The `zsh_index` (built once at startup via readdir of fpath dirs) is frozen for the daemon's lifetime. If `brew install helm` happens after startup, the index is stale.
3. Hundreds of commands on PATH that have completion generators (`command completion zsh`) or parseable `--help` output sit undiscovered indefinitely until manually executed.
4. `find_and_parse` in `zsh_completion.rs` — a function that reads and parses existing installed zsh completion files — is dead code. Never called.

## Design

### Phase 1: Startup PATH scan

On daemon startup, after `run_server` begins accepting connections, spawn a background task that:

1. Reads `$PATH` directories (from the first connected session's env, or from the daemon's own `$PATH`).
2. For each executable, checks `has_completion()`. If false, adds to a discovery queue.
3. Processes the queue with bounded concurrency (e.g., 4 concurrent discoveries) and a configurable delay between batches to avoid CPU spikes.
4. Uses the existing `trigger_discovery` path, which already handles dedup, blocklists, and file-existence checks.

**New config:**
```toml
[spec]
# Scan PATH for undiscovered commands on daemon startup
startup_scan = true
# Max concurrent discovery tasks during background scan
startup_scan_concurrency = 4
# Delay between discovery batches (ms)
startup_scan_delay_ms = 500
```

**Priority ordering:** Try the completion generator strategy first (cheapest, most accurate). Skip `--help` parsing during background scan unless a `startup_scan_depth` flag opts in, since LLM calls are expensive for bulk discovery.

**Key files:**
- `src/spec_store.rs` — add `run_startup_scan(&self, path_dirs: &[PathBuf])` method
- `src/daemon/server.rs` — spawn the scan task after server starts
- `src/config.rs` — add config fields

### Phase 2: Activate `find_and_parse` for installed completions

`zsh_completion.rs` already has `find_and_parse(command)` that reads existing `_command` files from fpath and parses them into `CommandSpec` via regex. This is currently dead code.

Wire it into the spec lookup path:

1. When `handle_complete` gets a `Complete` request and `spec_store.lookup()` returns `None`, try `zsh_completion::find_and_parse(command)` as a fallback.
2. Cache the parsed result in the project cache (or a new dedicated cache) to avoid re-parsing on every request.
3. This gives the daemon structured specs for commands that already have system completions (git, docker, etc.) — enabling the NL translator to know about their flags.

**Key files:**
- `src/zsh_completion.rs` — ensure `find_and_parse` is public and tested
- `src/spec_store.rs` — add a `parsed_system_specs` cache, call `find_and_parse` on lookup miss
- `src/daemon/handlers.rs` — no changes needed (uses `spec_store.lookup`)

### Phase 3: Refresh `zsh_index` periodically

The `zsh_index` `HashSet<String>` is built once at startup and never updated. Add a periodic refresh:

1. Every 5 minutes (or on `ReloadConfig`), re-run `scan_available_commands()`.
2. Replace `zsh_index` (change from `HashSet` to `RwLock<HashSet>` or use an `ArcSwap`).
3. This catches newly-installed tools (e.g., `brew install` while daemon is running).

**Key files:**
- `src/spec_store.rs` — change `zsh_index: HashSet<String>` to `zsh_index: ArcSwap<HashSet<String>>` (or `RwLock`)
- `src/daemon/server.rs` — spawn periodic refresh task

### Phase 4: Pre-discover notable tools

The `NOTABLE` list in `handlers.rs` (git, cargo, npm, docker, kubectl, etc.) represents the most commonly-needed commands. On first daemon startup (no existing completions dir), proactively discover these.

1. On startup, check if `completions_dir` is empty or has fewer than N files.
2. If so, run discovery for each `NOTABLE` tool that exists on PATH and isn't in `zsh_index`.
3. This provides a good first-run experience.

**Key files:**
- `src/daemon/server.rs` — check completions dir size, trigger notable-tools discovery
- `src/spec_store.rs` — reuse `trigger_discovery`

## Implementation Plan

| Step | Description | Files | Est. size |
|------|------------|-------|-----------|
| 1 | Add `startup_scan*` config fields with defaults | `src/config.rs` | S |
| 2 | Implement `run_startup_scan` on `SpecStore` — reads PATH dirs, filters against `has_completion`, runs `trigger_discovery` with bounded concurrency via `tokio::sync::Semaphore` | `src/spec_store.rs` | M |
| 3 | Spawn startup scan task in `run_server` after accepting first connection (to get PATH from session env), or from daemon's own PATH | `src/daemon/server.rs` | S |
| 4 | Make `zsh_index` mutable (`ArcSwap` or `RwLock`), add `refresh_zsh_index(&self)` method | `src/spec_store.rs` | S |
| 5 | Spawn periodic `refresh_zsh_index` task (every 5 min) alongside session pruner | `src/daemon/server.rs` | S |
| 6 | Wire `find_and_parse` into spec lookup as fallback, add `parsed_system_specs` cache | `src/spec_store.rs`, `src/zsh_completion.rs` | M |
| 7 | Add first-run notable-tools discovery | `src/daemon/server.rs`, `src/spec_store.rs` | S |
| 8 | Tests: startup scan with temp PATH dirs, zsh_index refresh, find_and_parse integration | `tests/` | M |

## Risks and Mitigations

- **CPU spikes during startup scan:** Mitigated by bounded concurrency (semaphore) and inter-batch delay. Generator-only mode avoids `--help` parsing for bulk.
- **LLM cost explosion from bulk discovery:** Phase 1 skips LLM by default. Only completion-generator and basic regex parsing run during startup scan.
- **Stale zsh_index causing redundant discovery:** Phase 3 fixes this with periodic refresh.
- **`find_and_parse` quality:** The regex parser in `zsh_completion.rs` only extracts options (not subcommands or args). Specs from this path will be partial. Good enough for NL prompt injection and basic completion, but not a replacement for full `--help` discovery.
