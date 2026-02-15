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

local script_dir="${0:A:h}"
local workspace_root="${script_dir:h}"
local build_profile="debug"
local cargo_build_args=()
if [[ "$1" == "--release" ]]; then
    build_profile="release"
    cargo_build_args=(--release)
fi

echo "synapse dev: building ($build_profile)..."
if ! (cd "$workspace_root" && cargo build "${cargo_build_args[@]}"); then
    echo "Error: Build failed." >&2
    return 1
fi

eval "$("$workspace_root/target/$build_profile/synapse")"
