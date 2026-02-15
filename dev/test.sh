#!/usr/bin/env zsh
# dev/test.sh — Source this to test local Synapse changes in the current shell.
#
# Usage:
#   source dev/test.sh              # Build debug, start daemon, source plugin
#   source dev/test.sh --release    # Same but with release build
#
# Re-sourcing is safe: kills the old daemon first, reconnects.
# Daemon is cleaned up automatically when the shell exits.

# Must be sourced, not executed
if [[ "$ZSH_EVAL_CONTEXT" != *:file ]]; then
    echo "Error: This script must be sourced, not executed." >&2
    echo "Usage: source dev/test.sh" >&2
    return 1 2>/dev/null || exit 1
fi

# Determine workspace root
local script_dir="${0:A:h}"
local workspace_root="${script_dir:h}"

if [[ ! -f "$workspace_root/Cargo.toml" ]]; then
    echo "Error: Cannot find Cargo.toml at $workspace_root" >&2
    return 1
fi

# Parse arguments
local build_profile="debug"
local cargo_build_args=()
if [[ "$1" == "--release" ]]; then
    build_profile="release"
    cargo_build_args=(--release)
fi

# Derive unique socket path from workspace directory
local workspace_hash
if command -v md5 &>/dev/null; then
    workspace_hash=$(echo -n "$workspace_root" | md5 | head -c 8)
elif command -v md5sum &>/dev/null; then
    workspace_hash=$(echo -n "$workspace_root" | md5sum | head -c 8)
else
    workspace_hash=$(echo -n "$workspace_root" | shasum | head -c 8)
fi

local socket_path="/tmp/synapse-dev-${workspace_hash}.sock"
local pid_path="/tmp/synapse-dev-${workspace_hash}.pid"
local binary_path="$workspace_root/target/$build_profile/synapse"
local log_path="/tmp/synapse-dev-${workspace_hash}.log"

echo "synapse dev"
echo "  workspace: $workspace_root"
echo "  socket:    $socket_path"
echo "  profile:   $build_profile"

# Stop existing dev daemon on this socket if running
if [[ -f "$pid_path" ]]; then
    local old_pid=$(< "$pid_path")
    if [[ -n "$old_pid" ]] && kill -0 "$old_pid" 2>/dev/null; then
        echo "  stopping existing daemon (PID $old_pid)..."
        kill "$old_pid" 2>/dev/null
        local i
        for i in 1 2 3 4 5; do
            kill -0 "$old_pid" 2>/dev/null || break
            sleep 0.1
        done
    fi
    rm -f "$pid_path"
fi
rm -f "$socket_path"

# Build
echo "  building ($build_profile)..."
local build_output
build_output=$(cd "$workspace_root" && cargo build "${cargo_build_args[@]}" 2>&1)
if (( $? != 0 )); then
    echo "$build_output" >&2
    echo "Error: Build failed." >&2
    return 1
fi

if [[ ! -x "$binary_path" ]]; then
    echo "Error: Binary not found at $binary_path" >&2
    return 1
fi

# Export env vars for both daemon and plugin
export SYNAPSE_SOCKET="$socket_path"
export SYNAPSE_BIN="$binary_path"
export _SYNAPSE_DEV_RELOAD=1
export _SYNAPSE_DEV_WORKSPACE="$workspace_root"

# Start daemon in background
echo "  starting daemon..."
"$binary_path" daemon start --foreground --socket-path "$socket_path" \
    --log-file "$log_path" -vv &>/dev/null &
disown

# Wait for socket to appear (daemon writes its own PID file before binding)
local attempts=0
while [[ ! -S "$socket_path" ]] && (( attempts < 50 )); do
    sleep 0.1
    (( attempts++ ))
done

if [[ ! -S "$socket_path" ]]; then
    echo "Error: Daemon failed to start. Check: tail -f $log_path" >&2
    return 1
fi

echo "  daemon running (PID $(< "$pid_path"))"

# Source the local plugin (will re-initialize due to _SYNAPSE_DEV_RELOAD)
source "$workspace_root/plugin/synapse.zsh"

# Unset reload flag so normal operation doesn't keep reloading
unset _SYNAPSE_DEV_RELOAD

# Set up cleanup trap
_synapse_dev_cleanup() {
    if [[ -n "$SYNAPSE_SOCKET" ]]; then
        local pid_file="/tmp/synapse-dev-${SYNAPSE_SOCKET##*-dev-}"
        pid_file="${pid_file%.sock}.pid"
        if [[ -f "$pid_file" ]]; then
            local pid=$(< "$pid_file")
            if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
                kill "$pid" 2>/dev/null
            fi
            rm -f "$pid_file"
        fi
        rm -f "$SYNAPSE_SOCKET"
    fi
    unset SYNAPSE_SOCKET SYNAPSE_BIN _SYNAPSE_DEV_WORKSPACE
}

# Set up cleanup on shell exit
if [[ -z "$_SYNAPSE_DEV_TRAP_SET" ]]; then
    _SYNAPSE_DEV_TRAP_SET=1
    trap '_synapse_dev_cleanup' EXIT
fi

echo ""
echo "ready! this shell uses the local synapse build."
echo "  logs: tail -f $log_path"
echo "  stop: exit (or run: _synapse_dev_cleanup)"
