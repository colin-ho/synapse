# Agent Daemon Test Playbook (Human-Like, Full-Feature)

This playbook is for an agent testing Synapse as close to a real human user as possible.

## Non-Negotiable Rules

1. Do not disable product functionality.
2. Do not force `llm.enabled = false`, `workflow.enabled = false`, or other feature-off test configs.
3. Prefer testing through the real zsh plugin flow first.
4. Use protocol-level `synapse probe` as a secondary observability/check tool, not the primary UX path.
5. If a capability depends on missing environment prerequisites (for example API keys), report that as a blocker; do not hide it by reconfiguring features off.
6. Treat repeated daemon parse warnings as real findings, even if high-level scenarios appear to pass.

## Prompt Template For Another Agent

```text
Goal: Test Synapse in a human-like way with full functionality enabled, covering many realistic user scenarios.

Required approach:
- Primary path: real zsh plugin interaction (typing, ghost text, dropdown, accept/dismiss, NL/explain when available).
- Secondary path: `synapse probe` for validation and diagnostics.
- Do not disable functionality in config for convenience.

Required outcomes:
1) Validate normal user workflows end-to-end.
2) Cover broad scenario variety (not just load volume).
3) Identify regressions with exact repro and environment assumptions.
4) Produce a pass/fail matrix with scenario evidence.
```

## Preflight (Run Before Scenarios)

1. Record baseline environment:
   - `which synapse`
   - `synapse --help`
   - `printenv | rg 'ANTHROPIC|OPENAI|SYNAPSE'` (or equivalent)
2. Check LLM prerequisites:
   - If using hosted providers, verify required API env keys are present.
   - If using a local OpenAI-compatible endpoint (LM Studio, Ollama OpenAI shim, etc.), verify endpoint health with `curl http://127.0.0.1:1234/v1/models` (or configured base URL).
   - Record the exact model ID used for the run.
3. Confirm daemon log capture location before tests:
   - all runs must preserve daemon logs for post-run parse-error inspection.

## Environment And Setup

1. Build current code:
   - `cargo build`
2. Start Synapse in a user-like shell flow:
   - Use a zsh session that sources `plugin/synapse.zsh` and points `SYNAPSE_BIN` to the built binary.
3. Keep functionality enabled:
   - Use current config/environment as-is.
   - If keys are missing for LLM features, record blocked scenarios explicitly.
   - On macOS, isolated config runs should set `HOME` and write config to `$HOME/Library/Application Support/synapse/config.toml` (not only `XDG_CONFIG_HOME`).
4. Prepare a realistic test workspace:
   - git repo
   - files/directories including hidden files and spaces in names
   - optional `package.json`, `Cargo.toml`, `docker-compose.yml`, `Justfile` to exercise providers

## Human-Like Test Lanes

## Lane 1: Interactive UX (Primary)

Test as a human user would at the prompt:

1. Typing and ghost text appearance.
2. Full accept (`Right` or `Tab`, depending config).
3. Partial accept (`Ctrl+Right`).
4. Dismiss (`Esc`).
5. Dropdown open/navigation/accept/dismiss via arrow keys.
6. History navigation interactions with Synapse state.
7. Natural-language mode (`? <query>`) if enabled (`Enter`/`Tab` triggers translation and dropdown/error rendering).
8. Explain flow for LLM-sourced command (`Ctrl+E`) when available.

Expected evidence:
- terminal snippets showing visible behavior and accepted command results.

## Lane 1A: True ZLE Keypress (Preferred)

Use real keypress-driven behavior in an interactive zsh session:

1. Type buffers manually.
2. Use bound keys (`Right`, `Tab`, `Ctrl+Right`, `Esc`, arrows).
3. Validate visible ghost text + dropdown behavior.

If environment limitations prevent reliable key injection, downgrade to Lane 1B and mark Lane 1A `BLOCKED` with reason.

> **Automated agents:** Lane 1A is always BLOCKED in non-interactive environments (CI, headless SSH, agent sandboxes). Proceed directly to Lane 1B and Lane 2.

## Lane 1B: Scripted Plugin Function Checks (Fallback)

Use plugin functions directly (`_synapse_suggest`, `_synapse_request`, dropdown parse helpers) for deterministic validation.

Important:
- This is not equivalent to full keypress UX.
- Report lane as `PASS (fallback)` and keep Lane 1A status explicit (`PASS`/`BLOCKED`).

Automation hygiene for Lane 1B:
1. Clear/normalize `_SYNAPSE_RECENT_COMMANDS` before request assertions.
2. Avoid feeding control characters into shell command history.
3. If parse errors appear, run triage (see below) before classifying product behavior.

## Lane 2: Protocol Verification (Secondary)

Use `synapse probe` to confirm daemon-side behavior behind UX results:

1. `ping`, `suggest`, `list_suggestions`, `interaction`, `command_executed`.
2. malformed request handling and parser resilience.
3. NL async follow-up capture via `--wait-for-update` or `--stdio --wait-ms` (while treating immediate non-`ack` responses as terminal).

Expected evidence:
- output lines and frame types matching requests.

## Failure Triage Rule (Mandatory)

When a plugin-path scenario fails:

1. Re-run equivalent request via `synapse probe`.
2. If probe succeeds but plugin path fails:
   - classify as plugin/path/harness issue
3. If both fail:
   - classify as daemon/provider/protocol issue
4. Record both repro commands in findings.
5. For NL, if `probe --request` times out waiting for follow-up frames, retry with `probe --stdio --wait-ms 20000` before marking FAIL.

## Scenario Breadth Matrix (Required)

Run a broad set of scenarios. Target at least 30 distinct scenarios total.

### A) Core Prompt UX Scenarios

1. Empty buffer (no noisy suggestion).
2. Command prefix typing (`gi` -> `git ...`).
3. Subcommand typing (`git ch`).
4. Option typing (`git commit --am`).
5. Argument typing (`cd ` / `cat `).
6. Pipe target typing (`echo hi | gr`).
7. Redirect target typing (`echo hi > out`).

### B) Interaction Semantics

1. Accept full suggestion.
2. Accept by word.
3. Dismiss suggestion.
4. Ignore by typing alternate content.
5. Dropdown select first item.
6. Dropdown scroll and select non-first item.
7. Dropdown dismiss and continue typing.

### C) History Learning

1. Execute new command, verify future suggestion improvement.
2. Recency effect (recent command outranks older one).
3. Frequency effect (repeated command becomes stronger).
4. Typo/fuzzy-ish recovery behavior.
5. Multi-line command handling.

### D) Provider Diversity

1. Spec: builtin completion trees (`git`, `cargo`, `docker`).
2. Filesystem: hidden files, spaced paths, dir/file distinction.
3. Environment: PATH command name completion.
4. Workflow provider sequence behavior.
5. LLM contextual arg suggestion when available.

### E) LLM/NL Scenarios (When Environment Supports)

1. NL query returns either a command `update` or explicit `error` (never silent/ack-only terminal behavior).
2. Safety/policy behavior on risky NL translation (expect explicit `error` when blocked).
3. Explain command response quality and stability (`suggest` or `error`).
4. Timeout/degraded behavior when provider is slow.
5. Probe timeout handling:
   - validate LLM scenarios with `--stdio --wait-ms` or `--wait-for-update` to avoid false negatives from fixed short request timeouts.

> **NL response contract:** `natural_language` can return (a) immediate `update` (cache hit), (b) immediate `error` (validation/config/policy), or (c) `ack` then async `update`/`error`. Do not require `ack` as the first frame.
>
> **Explain response contract:** `explain` is synchronous (`suggest` or `error`), not `ack` + async update.

If blocked by missing API key/env:
- mark scenario as `BLOCKED`, include exact missing prerequisite.

### E2) Request Escaping Robustness

Verify shell-originated request payload safety:

1. Recent-command strings containing quotes, tabs, and control-like content.
2. Ensure request frames remain valid JSON (no daemon parse errors caused by client encoding).

Pass criteria:
- no parser errors attributable to request construction/escaping

### F) Session And Context Isolation

1. Two shells/sessions open simultaneously.
2. Session A activity should not corrupt Session B state.
3. Different cwd contexts produce context-appropriate suggestions.
4. Rapid switching between projects.

### G) Resilience And Recovery

1. Daemon restart while shell remains open.
2. Temporary socket unavailability and reconnect behavior.
3. Malformed requests do not break later valid requests.
4. Shutdown/start cycle preserves operability.

## Optional Scenario Helper Commands

### Probe quick checks

```bash
synapse probe --socket-path "$SOCK" --request '{"type":"ping"}'
synapse probe --socket-path "$SOCK" --request '{"type":"suggest","session_id":"s1","buffer":"git ch","cursor_pos":6,"cwd":"/tmp","last_exit_code":0,"recent_commands":[]}'
# for slower local LLMs:
synapse probe --socket-path "$SOCK" --request '{"type":"explain","session_id":"s1","command":"git rebase -i HEAD~3"}' --first-response-timeout-ms 30000
```

### NL / Explain / Interaction probe examples

```bash
# Natural language query (can be immediate update/error OR ack then async update/error).
# --wait-for-update waits for a follow-up only when first frame is ack:
synapse probe --socket-path "$SOCK" --request '{"type":"natural_language","session_id":"s1","query":"find all rust files modified today","cwd":"/tmp","recent_commands":[]}' --wait-for-update --first-response-timeout-ms 30000

# Explain a command (synchronous: suggest or error):
synapse probe --socket-path "$SOCK" --request '{"type":"explain","session_id":"s1","command":"git rebase -i HEAD~3"}' --first-response-timeout-ms 30000

# Interaction feedback (synchronous ack):
synapse probe --socket-path "$SOCK" --request '{"type":"interaction","session_id":"s1","action":"accept","suggestion":"git status","source":"history","buffer_at_action":"git sta"}'

# Command executed feedback (synchronous ack):
synapse probe --socket-path "$SOCK" --request '{"type":"command_executed","session_id":"s1","command":"git status"}'

# Alternative: use --stdio --wait-ms for NL/explain to capture all output including late updates:
echo '{"type":"natural_language","session_id":"s1","query":"list docker containers","cwd":"/tmp","recent_commands":[]}' | synapse probe --socket-path "$SOCK" --stdio --wait-ms 30000
```

> **Note:** The `? prefix` syntax is plugin-only (handled by the zsh widget). At the daemon protocol level, use `"type": "natural_language"` instead.

### Streamed sequence

```bash
cat <<'EOF' | synapse probe --socket-path "$SOCK" --stdio --wait-ms 300
{"type":"ping"}
{"type":"command_executed","session_id":"s1","command":"git status"}
{"type":"list_suggestions","session_id":"s1","buffer":"git ch","cursor_pos":6,"cwd":"/tmp","max_results":8,"last_exit_code":0,"recent_commands":[]}
EOF
```

## Reporting Contract (Mandatory)

1. Findings ordered by severity:
   - `severity | scenario | evidence | repro`
2. Scenario matrix:
   - `PASS` / `FAIL` / `BLOCKED`
3. Environment prerequisites detected:
   - present/missing keys and tools affecting coverage
4. Exact commands run
5. Key output snippets
6. Final verdict:
   - `PASS` only if all non-blocked critical scenarios pass

## Required Log Inspection

At end of run, inspect daemon logs for parser and transport anomalies:

1. Count lines matching `Parse error:`.
2. If count > 0, add finding with sample lines and likely source (daemon input vs plugin serialization).
3. Do not silently ignore parse warnings even when scenario assertions pass.

## Retry Policy For Non-Deterministic Scenarios

For workflow/async/update-sensitive scenarios:

1. Retry up to 5 times before marking `BLOCKED`.
2. Record each attempt count and observed response type.
