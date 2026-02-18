# Agent Daemon Test Playbook

This playbook is for an automated agent testing Synapse via `synapse probe` (protocol-level). Interactive zsh testing (Lane 1) is always BLOCKED in agent environments — all scenarios use the probe path.

## Non-Negotiable Rules

1. Do not disable product functionality.
2. Do not force `llm.enabled = false` or other feature-off test configs.
3. If a capability depends on missing environment prerequisites (for example API keys), report that as a blocker; do not hide it by reconfiguring features off.
4. Treat repeated daemon parse warnings as real findings, even if high-level scenarios appear to pass — but exclude parse errors caused by your own intentional malformed-request tests (see G3).

## Environment And Setup

### Preflight

1. Build current code: `cargo build`
2. Record baseline environment:
   - `which synapse`, `synapse --help`
   - `printenv | grep -E 'OPENAI|SYNAPSE'`
3. Check LLM prerequisites:
   - If using a local endpoint, verify health: `curl -s http://127.0.0.1:1234/v1/models`
   - If using hosted OpenAI, verify API key is set.
   - Record the model ID. Mark NL scenarios `BLOCKED` if no LLM is available.

### Isolated Test Environment

Create an isolated workspace so tests don't interfere with the user's real config or daemon.

```bash
BIN=./target/debug/synapse
TESTDIR=$(mktemp -d /tmp/synapse-playbook.XXXXXX)
SOCK="$TESTDIR/synapse-test.sock"

# Git repo + project files for spec auto-generation
cd "$TESTDIR" && git init -q
cat > Cargo.toml <<'EOF'
[package]
name = "testproj"
version = "0.1.0"
edition = "2021"
[[bin]]
name = "testproj"
path = "src/main.rs"
EOF
mkdir -p src && echo 'fn main() {}' > src/main.rs

cat > Makefile <<'EOF'
build:
	echo build
test:
	echo test
clean:
	echo clean
EOF

cat > package.json <<'EOF'
{"name":"testproj","scripts":{"dev":"echo dev","build":"echo build","test":"echo test"}}
EOF

cat > docker-compose.yml <<'EOF'
services:
  web:
    image: nginx
  db:
    image: postgres
EOF

cat > Justfile <<'EOF'
default:
    echo default
build:
    echo build
test:
    echo test
EOF

# Isolated config
CONFDIR="$TESTDIR/config/synapse"
mkdir -p "$CONFDIR"
cat > "$CONFDIR/config.toml" <<'EOF'
[llm]
enabled = true
natural_language = true
provider = "openai"
base_url = "http://127.0.0.1:1234/v1"

[spec]
discover_from_help = true

[completions]
auto_regenerate = true
gap_only = false
EOF

# Start daemon
XDG_CONFIG_HOME="$TESTDIR/config" $BIN start --foreground \
    --socket-path "$SOCK" -vv > "$TESTDIR/daemon.log" 2>&1 &
DAEMON_PID=$!

# Wait for socket
for i in $(seq 1 50); do [ -S "$SOCK" ] && break; sleep 0.05; done
[ -S "$SOCK" ] || { echo "FAIL: socket not created"; cat "$TESTDIR/daemon.log"; exit 1; }
```

All subsequent probe commands use `$BIN`, `$SOCK`, and `$TESTDIR` from this setup.

## How To Test

Use `synapse probe` for all testing. Supported request types: `ping`, `command_executed`, `complete`, `natural_language`, `shutdown`, `reload_config`, `clear_cache`.

Expected evidence: output lines and frame types matching requests.

## Failure Triage Rule (Mandatory)

When a scenario fails:

1. Retry the exact same probe command to rule out transient issues.
2. For NL, if `probe --request` times out, retry with `probe --stdio --wait-ms 20000` before marking FAIL.
3. For discovery, wait at least 10 seconds and retry — discovery runs asynchronously.
4. Record exact probe commands and responses in findings.

## Scenario Breadth Matrix (Required)

Target at least 20 distinct scenarios.

### A) Compsys Completion Scenarios

| # | Scenario | How to test |
|---|---|---|
| A1 | `generate-completions` produces files | Run `$BIN generate-completions --output-dir "$TESTDIR/completions"` from project cwd, count output files |
| A2 | Project auto-spec (`npm run`, `make`, `just`) | `complete` with project `cwd` — expect targets from project files |
| A3 | Gap-only mode | `generate-completions` output reports skipped commands with existing compsys functions |

### B) Complete Protocol

| # | Scenario | How to test |
|---|---|---|
| B1 | Project command returns items | `complete` for `make` with project cwd — expect `complete_result` with count > 0 |
| B2 | Unknown command returns empty | `complete` for `nonexistent_xyz` — expect `complete_result\t0` |
| B3 | `cwd` affects results | `complete` for `make` with project cwd vs `/tmp` — expect different counts (project targets vs none) |

### C) Spec System

| # | Scenario | How to test |
|---|---|---|
| C1 | Project auto-specs | `complete` for `make`/`npm`/`just` with project cwd — expect targets from project files |
| C2 | Discovery triggers on `command_executed` | See Discovery Testing below |
| C3 | Discovered specs persist as compsys files | After discovery, check completions dir for generated `_<cmd>` file |

#### Discovery Testing

Discovery only triggers for commands that have **no existing zsh compsys function AND no previously generated compsys file**. Commands like `git`, `cargo`, `rg`, `ls` typically already have zsh completions and will be skipped. To test discovery:

1. Find a candidate: pick an installed command that lacks a zsh completion file (e.g. `fzf`, `delta`, `hyperfine`).
2. Trigger: send `command_executed` with that command name.
3. Wait: discovery runs `--help` parsing asynchronously. **Wait at least 10 seconds** before checking.
4. Verify: check the completions dir (`~/.local/share/synapse/completions/`) for a generated `_<cmd>` file. Also check daemon logs for `Wrote compsys completion for <cmd>`.

### D) NL Scenarios (When Environment Supports)

| # | Scenario | How to test |
|---|---|---|
| D1 | NL returns suggestions | `natural_language` query — expect `list` frame with count > 0 |
| D2 | Short query rejected | `natural_language` with query `"hi"` — expect `error` with "too short" |
| D3 | Cache hit | Send same NL query twice — second should return identical `list` immediately |
| D4 | Risky command handling | `natural_language` with destructive query — expect `list` with warning descriptions, or `error` if blocked by `security.command_blocklist` |

> **NL response contract:** `natural_language` can return (a) immediate `list` (cache hit), (b) immediate `error` (validation/config/policy), or (c) `ack` then async `list`/`error`. Do not require `ack` as the first frame. The `list` frame is TSV: `list\t<count>\t<cmd1>\t<source1>\t<desc1>\t<kind1>\t...`

Use `--wait-for-update --first-response-timeout-ms 30000` to handle the async case.

If blocked by missing API key/env, mark all D scenarios as `BLOCKED`.

### E) Request Escaping Robustness

| # | Scenario | How to test |
|---|---|---|
| E1 | Quotes in command | `command_executed` with `echo "hello" \| grep 'world'` — expect `ack` |
| E2 | Special chars in context | `complete` with `context:["git","log","--format=%H"]` — expect `complete_result` |

Pass criteria: no parser errors in daemon logs attributable to these requests.

### F) Session And Context Isolation

| # | Scenario | How to test |
|---|---|---|
| F1 | Multi-session | Send `command_executed` with different `session_id` values — both return `ack` |
| F2 | cwd isolation | `complete` for `make` with project cwd vs `/tmp` — different results |
| F3 | Concurrent connections | Fire 5 `ping` requests in parallel — all return `pong` |

### G) Resilience And Recovery

| # | Scenario | How to test |
|---|---|---|
| G1 | Malformed request recovery | Send `{"type":"bogus"}` then `{"type":"ping"}` — expect `error` then `pong` |
| G2 | Invalid JSON recovery | Send `{broken` then `{"type":"ping"}` — expect `error` then `pong` |
| G3 | Shutdown/restart cycle | Send `shutdown`, wait 1s, restart daemon, `ping` — expect `pong` |
| G4 | Sequential reconnect | 5 sequential `ping` requests — all return `pong` |

## Probe Reference

```bash
# Ping
$BIN probe --socket-path "$SOCK" --request '{"type":"ping"}'

# Complete (project-aware — use a cwd with project files)
$BIN probe --socket-path "$SOCK" --request \
  '{"type":"complete","command":"make","context":[],"cwd":"'"$TESTDIR"'"}'

# Command executed (triggers discovery for unknown commands)
$BIN probe --socket-path "$SOCK" --request \
  '{"type":"command_executed","session_id":"s1","command":"make build","cwd":"'"$TESTDIR"'"}'

# NL (with async wait)
$BIN probe --socket-path "$SOCK" --request \
  '{"type":"natural_language","session_id":"s1","query":"find rust files","cwd":"/tmp"}' \
  --wait-for-update --first-response-timeout-ms 30000

# Streamed sequence
cat <<EOF | $BIN probe --socket-path "$SOCK" --stdio --wait-ms 500
{"type":"ping"}
{"type":"command_executed","session_id":"s1","command":"make build","cwd":"$TESTDIR"}
{"type":"complete","command":"make","context":[],"cwd":"$TESTDIR"}
EOF
```

> **Note:** The `? prefix` NL syntax is plugin-only. At the protocol level, use `"type": "natural_language"`.

## Reporting Contract (Mandatory)

1. Findings ordered by severity: `severity | scenario | evidence | repro`
2. Scenario matrix: `PASS` / `FAIL` / `BLOCKED` for each scenario ID (A1–G4)
3. Environment prerequisites: present/missing keys and tools affecting coverage
4. Exact probe commands run and key output snippets
5. Final verdict: `PASS` only if all non-blocked scenarios pass

## Required Log Inspection

At end of run, inspect daemon logs for anomalies:

1. Count lines matching `Parse error:`.
2. **Subtract** parse errors caused by your own intentional malformed-request tests (G1, G2). These are expected.
3. If remaining count > 0, add finding with sample lines and likely source.
4. Check for `ERROR`-level lines. Any unexpected errors are findings.
5. Do not silently ignore warnings even when scenario assertions pass.

## Cleanup

```bash
$BIN probe --socket-path "$SOCK" --request '{"type":"shutdown"}'
rm -rf "$TESTDIR"
```

## Retry Policy

For NL/async/discovery-sensitive scenarios:

1. Retry up to 5 times before marking `BLOCKED`.
2. For discovery, use increasing waits (5s, 10s, 15s).
3. Record each attempt count and observed response type.
