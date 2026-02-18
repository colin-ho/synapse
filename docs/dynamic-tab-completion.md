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

### Phase 1: Add timeout wrappers to inline generators

Modify `compsys_export.rs` to wrap generator commands in a timeout:

**Before:**
```zsh
{local -a vals; vals=(${(f)"$(git branch --no-color 2>/dev/null)"}); compadd -a vals}
```

**After:**
```zsh
{local -a vals; vals=(${(f)"$(timeout 2 git branch --no-color 2>/dev/null)"}); compadd -a vals}
```

Use `timeout` (coreutils) or `gtimeout` (macOS via Homebrew) if available, or fall back to a zsh-native approach using `zsh/system` with `sysread -t`.

Alternatively, emit `synapse complete` calls instead of inline generators — this routes through the daemon's existing timeout and caching infrastructure.

**Key files:**
- `src/compsys_export.rs` — modify `format_generator_action` to wrap in timeout or emit `synapse complete` calls

### Phase 2: Route all generators through the daemon

Instead of embedding shell commands inline in compsys files, emit `synapse complete` calls for all generator-backed args. This provides:

- Consistent timeout enforcement (daemon-side 500ms/5s cap)
- Generator output caching (moka, 10s default TTL)
- Centralized error handling and logging
- Offline fallback to static suggestions

**Generated compsys output would change from:**
```zsh
':branch:{local -a vals; vals=(${(f)"$(git branch --no-color 2>/dev/null)"}); compadd -a vals}'
```

**To:**
```zsh
':branch:{local -a vals; vals=(${(f)"$(synapse complete git checkout --cwd $PWD 2>/dev/null)"}); compadd -a vals}'
```

This requires the daemon to be aware of discovered specs (currently it only looks up project specs). See Phase 3.

**Key files:**
- `src/compsys_export.rs` — change `format_generator_action` to emit `synapse complete` calls
- `src/spec.rs` — no changes

### Phase 3: Make discovered specs available to the daemon

Currently `spec_store.lookup()` only searches `project_cache`. Discovered specs exist only as compsys files on disk — the daemon never reads them back. To route generators through the daemon, it needs access to discovered specs.

Two options:

**Option A: Read compsys files on demand.** On `lookup()` miss for project specs, check if `completions_dir/_command` exists, read it, parse via `parse_zsh_completion`, cache the result. Downside: the regex parser only extracts options, not generators or subcommand structure.

**Option B: Cache discovered specs in memory before writing.** When `save_discovered_spec` writes the compsys file, also store the `CommandSpec` in a new `discovered_cache: Cache<String, CommandSpec>`. On `lookup()`, check project cache then discovered cache. This preserves the full spec structure including generators.

**Recommendation: Option B.** It's simpler, preserves full spec fidelity, and the cache can be loaded from disk on startup by re-reading the compsys files (with the limitation that parsed-back specs lose some structure).

**Key files:**
- `src/spec_store.rs` — add `discovered_cache: Cache<String, CommandSpec>`, populate in `save_discovered_spec`, check in `lookup`
- `src/daemon/handlers.rs` — no changes needed (uses `spec_store.lookup`)

### Phase 4: Warm generator cache on `chpwd`

When the `CwdChanged` event fires (from the proactive-project-completions design), pre-run generators for the new cwd's project specs. This eliminates cold-cache latency on the first Tab press after a directory change.

1. On `CwdChanged`, load project specs for the new cwd.
2. For each spec with generators, run `run_generator` in the background (fire-and-forget).
3. By the time the user presses Tab, the generator cache is warm.

**Key files:**
- `src/daemon/handlers.rs` — in `handle_cwd_changed`, spawn generator pre-warming tasks
- `src/spec_store.rs` — add `prewarm_generators(&self, specs: &[CommandSpec], cwd: &Path)` method

### Phase 5: Improve offline fallback

When the daemon is unreachable, `run_complete_query` falls back to a fresh `SpecStore` with no generators. Improve this:

1. Cache the last successful generator result per `(command, cwd)` to a small file in `completions_dir/cache/`.
2. On offline fallback, read the cached file and return those values.
3. This provides stale-but-useful completions when the daemon is down.

**Key files:**
- `src/daemon/lifecycle.rs` — modify offline fallback in `run_complete_query`
- `src/spec_store.rs` — add file-based generator result cache (write-through from `run_generator`)

## Implementation Plan

| Step | Description | Files | Est. size |
|------|------------|-------|-----------|
| 1 | Add `discovered_cache` to `SpecStore`, populate in `save_discovered_spec` | `src/spec_store.rs` | S |
| 2 | Extend `lookup()` to check discovered cache after project cache | `src/spec_store.rs` | S |
| 3 | Change `format_generator_action` to emit `synapse complete` calls instead of inline shell | `src/compsys_export.rs` | M |
| 4 | Update `handle_complete` to resolve generators from discovered specs | `src/daemon/handlers.rs` | S |
| 5 | Add `prewarm_generators` method to `SpecStore` | `src/spec_store.rs` | S |
| 6 | Call `prewarm_generators` from `handle_cwd_changed` handler | `src/daemon/handlers.rs` | S |
| 7 | Add file-based generator result cache for offline fallback | `src/spec_store.rs`, `src/daemon/lifecycle.rs` | M |
| 8 | Tests: discovered cache lookup, synapse-complete-based compsys output, offline fallback | `tests/` | M |

## Risks and Mitigations

- **Daemon dependency for all completions:** Routing generators through the daemon means Tab completion for generator-backed args fails silently when the daemon is down (returns empty). Phase 5 mitigates this with a file-based fallback cache. Static completions (options, subcommand names) are still embedded in the compsys file and work without the daemon.
- **Latency regression from socket round-trip:** Adding a socket call where there was previously an inline shell command adds ~1-5ms overhead. This is negligible compared to generator command execution time (typically 50-500ms). The caching benefit (10s TTL vs. running the command every time) likely makes this faster in practice.
- **Discovered cache memory usage:** Specs for hundreds of commands could be large. Use a moka cache with a max capacity (e.g., 500 entries) and TTL matching `DISCOVER_MAX_AGE_SECS` (7 days). Entries are small (~1-10KB per spec).
- **Breaking change in compsys file format:** Existing compsys files with inline generators will continue to work. New/regenerated files will use `synapse complete`. Users need to regenerate files to get the new behavior (`synapse generate-completions --force`).
