# Dynamic Tab Completion Improvements

## Current State

There are two paths for Tab completion:

**Path A — Inline generators (no daemon):** Generated compsys files embed shell commands directly in `_arguments` actions:
```zsh
':branch:{local -a vals; vals=(${(f)"$(git branch --no-color 2>/dev/null)"}); compadd -a vals}'
```
These run synchronously in zsh with **no timeout wrapper**. If the command hangs, Tab completion blocks the shell indefinitely.

**Path B — `synapse complete` via daemon:** The compsys file calls `synapse complete <cmd> [ctx...] --cwd $PWD`. The daemon walks the spec tree, runs generators with a 500ms timeout (5s cap), and returns results over the socket (5s connection timeout).

**Key problems:**
1. Inline generators have no timeout protection.
2. `spec_store.lookup()` only returns project specs — discovered specs (written as compsys files) are never re-read by the daemon. The daemon can't serve dynamic completions for discovered commands.
3. The offline fallback (`run_complete_query` when daemon is down) returns only static suggestions — no generators run.
4. Generator cache is keyed on `(command_string, cwd)`. A `cd` invalidates the cache, so the first completion in a new directory always pays full generator latency.

## Design

### Phase 1: ~~Add timeout wrappers to inline generators~~ (Skipped)

Superseded by Phase 2. Inline timeout wrappers (`timeout` or `gtimeout`) would only be a stop-gap — routing through the daemon provides timeout, caching, and centralized error handling in one step.

### Phase 2: Route all generators through the daemon

Instead of embedding shell commands inline in compsys files, emit `synapse run-generator` calls. This provides:

- Consistent timeout enforcement (daemon-side 500ms/5s cap)
- Generator output caching (moka, 10s default TTL)
- Centralized error handling and logging
- Offline fallback via direct execution with timeout

**Why `synapse run-generator` instead of `synapse complete`:** The original plan proposed routing through `synapse complete`, but this has two problems:

1. **Arg disambiguation:** `handle_complete` returns ALL generators for the current command level. If a subcommand has multiple positional args with generators (e.g., branch + path), results get mixed. The `_arguments` spec dispatches to different actions per positional arg, so each action needs only its specific generator's output.

2. **Daemon restart bootstrapping:** The discovered_cache is populated in-memory during discovery. After daemon restart, it's empty. Compsys files that call `synapse complete` for discovered commands would get nothing until re-discovery. `synapse run-generator` doesn't depend on spec lookup — it takes the generator command directly.

**Generated compsys output changes from:**
```zsh
':branch:{local -a vals; vals=(${(f)"$(git branch --no-color 2>/dev/null)"}); compadd -a vals}'
```

**To:**
```zsh
':branch:{local -a vals; vals=(${(f)"$(synapse run-generator "git branch --no-color" --cwd "$PWD" --strip-prefix "* " 2>/dev/null)"}); compadd -a vals}'
```

The `synapse run-generator` CLI:
- Connects to the daemon and sends a `RunGenerator` protocol request
- Daemon runs the command via `SpecStore::run_generator` (timeout + caching)
- If daemon is down, falls back to direct execution with timeout (`GENERATOR_TIMEOUT_MS`)
- Outputs one value per line (compsys action always splits on newlines)
- `strip_prefix` and `split_on` are handled daemon-side, not in the compsys file

**Key files:**
- `src/protocol.rs` — add `RunGenerator` request type
- `src/daemon/handlers.rs` — add `handle_run_generator`
- `src/daemon/mod.rs` — add `RunGenerator` CLI subcommand
- `src/daemon/lifecycle.rs` — add `run_generator_query` with offline fallback
- `src/compsys_export.rs` — change `format_generator_action` to emit `synapse run-generator` calls

### Phase 3: Make discovered specs available to the daemon (implemented)

**Option B (as recommended).** When `save_discovered_spec` writes the compsys file, also store the `CommandSpec` in `discovered_cache: Cache<String, CommandSpec>`. `lookup()` checks project cache then discovered cache.

**Key files:**
- `src/spec_store.rs` — add `discovered_cache`, populate in `save_discovered_spec`, check in `lookup`, clear in `clear_caches`

### Phase 4: Warm generator cache on `chpwd` (deferred)

Requires a `CwdChanged` protocol event that doesn't exist yet. Deferred until the proactive-project-completions work adds this infrastructure.

### Phase 5: ~~Improve offline fallback~~ (partially addressed)

The `synapse run-generator` CLI already provides offline fallback by running the generator command directly with a timeout when the daemon is unreachable. This is simpler than a file-based cache and covers the main use case.

A file-based stale cache could still be added later for faster offline completions (avoiding generator execution entirely), but the direct-execution fallback is sufficient for now.

## Implementation Plan

| Step | Description | Files | Status |
|------|------------|-------|--------|
| 1 | Add `discovered_cache` to `SpecStore`, populate in `save_discovered_spec` | `src/spec_store.rs` | Done |
| 2 | Extend `lookup()` to check discovered cache after project cache | `src/spec_store.rs` | Done |
| 3 | Add `RunGenerator` protocol request + daemon handler | `src/protocol.rs`, `src/daemon/handlers.rs` | Done |
| 4 | Add `synapse run-generator` CLI with daemon + offline fallback | `src/daemon/mod.rs`, `src/daemon/lifecycle.rs` | Done |
| 5 | Change `format_generator_action` to emit `synapse run-generator` calls | `src/compsys_export.rs` | Done |
| 6 | Tests: discovered cache, protocol deserialization, compsys output format | unit tests | Done |
| — | Add `CwdChanged` event + `prewarm_generators` | `src/protocol.rs`, `src/daemon/handlers.rs`, `src/spec_store.rs` | Deferred |
| — | File-based generator result cache for offline fallback | `src/spec_store.rs`, `src/daemon/lifecycle.rs` | Deferred |

## Risks and Mitigations

- **Daemon dependency for generator completions:** When the daemon is down, `synapse run-generator` falls back to direct execution with a 5s timeout. Static completions (options, subcommand names) are still embedded in the compsys file and work without the daemon or synapse binary.
- **Latency regression from socket round-trip:** Adding a socket call where there was previously an inline shell command adds ~1-5ms overhead. This is negligible compared to generator command execution time (typically 50-500ms). The caching benefit (10s TTL vs. running the command every time) likely makes this faster in practice.
- **Discovered cache memory usage:** Specs for hundreds of commands could be large. Uses a moka cache with max capacity 500 entries and TTL matching `DISCOVER_MAX_AGE_SECS` (7 days). Entries are small (~1-10KB per spec). Cache is empty after daemon restart (specs are re-discovered on use).
- **Breaking change in compsys file format:** Existing compsys files with inline generators continue to work. New/regenerated files use `synapse run-generator`. Users need to regenerate files to get the new behavior (`synapse generate-completions --force`).
