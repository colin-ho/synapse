# Proactive Project Completions

## Problem

Project-aware completions (make targets, npm scripts, docker-compose services, etc.) have three issues:

1. **Memory-only by default.** Project specs live only in the daemon's `project_cache` (moka, 5-min TTL). They are never written to disk as compsys files unless the user manually runs `synapse generate-completions`. This means zsh's native Tab completion doesn't see project-specific targets — it falls back to system completions.

2. **Lazy evaluation.** Project specs are only built on the first `Complete` request for a given cwd. There's no `chpwd` hook, no file-watching, no preemptive generation.

3. **Static encoding.** Make targets, npm scripts, docker services, and justfile recipes are encoded as static `SubcommandSpec` or `suggestions` values, not as `GeneratorSpec` commands. Once written to a compsys file, they go stale the moment the project file changes.

4. **Dead config.** `CompletionsConfig.auto_regenerate` exists (defaults to `true`) but is never read by any code path.

## Design

### Phase 1: Auto-write compsys files on project spec build

When `get_project_specs` builds specs on a cache miss (the `spawn_blocking` path), also write them as compsys files to disk. This eliminates the need to run `synapse generate-completions` manually.

1. After `spec_autogen::generate_specs` returns, call `compsys_export::write_completion_file` for each spec.
2. Gate this behind the existing `auto_regenerate` config flag (already defined, just unwired).
3. Only write if the spec has changed from what's on disk (compare a hash or mtime) to avoid unnecessary writes.

**Key files:**
- `src/spec_store.rs` — in `get_project_specs`, after building specs, call compsys export
- `src/compsys_export.rs` — ensure `write_completion_file` handles `ProjectAuto` source
- `src/config.rs` — no changes needed, `auto_regenerate` already exists

### Phase 2: Use generators instead of static values

Convert project-specific completions from static subcommands/suggestions to `GeneratorSpec` commands. This ensures completions are always live — the daemon re-reads project files on each completion request (within the generator cache TTL).

| Tool | Current encoding | Proposed generator |
|------|-----------------|-------------------|
| make | Static `SubcommandSpec` per target | `make -qp 2>/dev/null \| awk -F: '/^[a-zA-Z][^$#\/\t=]*:([^=]|$)/{print $1}'` |
| npm run | Static `SubcommandSpec` per script | `node -e "Object.keys(require('./package.json').scripts\|\|{}).forEach(s=>console.log(s))"` |
| docker compose | Static `suggestions` per service | `docker compose config --services 2>/dev/null` |
| just | Static `SubcommandSpec` per recipe | `just --summary 2>/dev/null \| tr ' ' '\n'` |
| cargo | Hardcoded subcommands | Keep static — cargo subcommands don't change per project |

Generator specs get embedded in compsys files as inline shell code. The 10-second default cache TTL means after adding a new Makefile target, the next Tab press within 10 seconds shows the old list, then refreshes.

**Key files:**
- `src/spec_autogen.rs` — change `make_spec`, `package_json_spec`, `justfile_spec`, `docker_spec` to use `GeneratorSpec` for the dynamic parts (target/script/recipe names), keep static structure for subcommands
- `src/spec.rs` — no changes needed
- `src/compsys_export.rs` — already handles `GeneratorSpec` in `format_arg`

### Phase 3: `chpwd` hook for cache warming

Add a zsh `chpwd` hook that notifies the daemon when the user changes directories:

1. Plugin: register `_synapse_chpwd` via `add-zsh-hook chpwd`.
2. On directory change, send a new `CwdChanged` protocol message with the new cwd.
3. Daemon: on `CwdChanged`, pre-warm `get_project_specs(new_cwd)` and write compsys files if `auto_regenerate` is enabled.
4. This ensures completions are ready before the first Tab press in a new directory.

**New protocol message:**
```json
{"type": "cwd_changed", "session_id": "abc123", "cwd": "/new/path"}
```

**Key files:**
- `plugin/synapse.zsh` — add `_synapse_chpwd` hook
- `src/protocol.rs` — add `CwdChanged` request variant
- `src/daemon/handlers.rs` — add handler that calls `spec_store.get_project_specs(cwd)` + optional compsys write

### Phase 4: Expand project type coverage

Fill gaps in `spec_autogen.rs`:

| Project type | What to add |
|-------------|-------------|
| Cargo | More subcommands: `add`, `remove`, `doc`, `bench`, `clean`, `install`, `update`, `tree`, `fix`. Enumerate `--bin` targets as suggestions. |
| Docker compose | More subcommands: `exec`, `run`, `stop`, `start`, `pull`, `config`. |
| Justfile | Extract recipe descriptions from `# comment` lines above recipes, or use `just --list` output. |
| Makefile | Extract `.PHONY` targets. Use `make -p` descriptions where available. |
| Go | Detect Go projects (`go.mod`), generate `go build/test/run/mod/vet/fmt` specs. |
| Python (uv) | Detect `uv.lock` or `[tool.uv]` in pyproject.toml, generate `uv run/sync/add/remove/lock` spec. |
| Taskfile.yml | Detect Taskfile.yml, generate `task` spec with task names. |

**Key files:**
- `src/spec_autogen.rs` — extend each parser, add new parsers
- `src/project.rs` — add detection for new project types

## Implementation Plan

| Step | Description | Files | Est. size |
|------|------------|-------|-----------|
| 1 | Wire `auto_regenerate` config flag — after `get_project_specs` builds specs on cache miss, write compsys files if `auto_regenerate = true` | `src/spec_store.rs` | S |
| 2 | Convert `make_spec` to use `GeneratorSpec` for target names (keep static subcommands for `make` itself) | `src/spec_autogen.rs` | S |
| 3 | Convert `package_json_spec` to use `GeneratorSpec` for script names | `src/spec_autogen.rs` | S |
| 4 | Convert `justfile_spec` to use `GeneratorSpec` | `src/spec_autogen.rs` | S |
| 5 | Convert `docker_spec` to use `GeneratorSpec` for service names | `src/spec_autogen.rs` | S |
| 6 | Add `CwdChanged` protocol message and handler | `src/protocol.rs`, `src/daemon/handlers.rs` | S |
| 7 | Add `_synapse_chpwd` hook to plugin, sends `cwd_changed` on directory change | `plugin/synapse.zsh` | S |
| 8 | Expand Cargo spec with more subcommands and `--bin` enumeration | `src/spec_autogen.rs` | S |
| 9 | Add Docker compose missing subcommands | `src/spec_autogen.rs` | S |
| 10 | Add Go project detection and spec generation | `src/spec_autogen.rs`, `src/project.rs` | M |
| 11 | Add `uv` project detection and spec generation | `src/spec_autogen.rs`, `src/project.rs` | S |
| 12 | Add Taskfile.yml support | `src/spec_autogen.rs`, `src/project.rs` | S |
| 13 | Extract Justfile recipe descriptions from comments | `src/spec_autogen.rs` | S |
| 14 | Tests for generator-based specs, chpwd hook, new project types | `tests/` | M |

## Risks and Mitigations

- **Generator commands failing silently:** The existing `run_generator` path returns empty `Vec` on failure and caches it for the full TTL. This is fine for resilience but means a broken generator silently produces no completions. Add DEBUG-level logging when a generator returns empty.
- **Auto-write race with manual `generate-completions`:** Both paths write to the same directory. The last writer wins. This is acceptable since both produce correct output.
- **compsys files written for cwd A visible in cwd B:** The generated `_make` file contains targets from the project where it was last written. If the user switches to a different project, the stale file is loaded by zsh until the daemon rewrites it. Generator-based specs solve this (Phase 2) because the generator command runs in the current cwd at completion time.
- **Generator overhead at completion time:** Running `make -qp` or `docker compose config --services` adds latency to Tab completion. The 10s cache TTL mitigates this for repeated completions. For slow generators, the 500ms default timeout prevents blocking.
