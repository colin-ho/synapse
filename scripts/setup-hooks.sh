#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOOK_SRC="$REPO_ROOT/scripts/pre-commit"
HOOK_DST="$REPO_ROOT/.git/hooks/pre-commit"

# Handle git worktrees: .git may be a file pointing to the real gitdir
if [ -f "$REPO_ROOT/.git" ]; then
    GITDIR="$(sed 's/^gitdir: //' "$REPO_ROOT/.git")"
    if [[ "$GITDIR" != /* ]]; then
        GITDIR="$REPO_ROOT/$GITDIR"
    fi
    HOOK_DST="$GITDIR/hooks/pre-commit"
fi

mkdir -p "$(dirname "$HOOK_DST")"
ln -sf "$HOOK_SRC" "$HOOK_DST"
echo "Pre-commit hook installed: $HOOK_DST -> $HOOK_SRC"
