#!/usr/bin/env zsh
# Synapse — Intelligent Zsh command suggestions via ghost text
# Source this file in your .zshrc or use: source $(synapse --shell-init)

# Guard against double-sourcing
[[ -n "$_SYNAPSE_LOADED" ]] && return
_SYNAPSE_LOADED=1

# --- Configuration ---
typeset -g _SYNAPSE_SESSION_ID=""
typeset -g _SYNAPSE_SOCKET_FD=""
typeset -g _SYNAPSE_CONNECTED=0
typeset -g _SYNAPSE_CURRENT_SUGGESTION=""
typeset -g _SYNAPSE_CURRENT_SOURCE=""
typeset -g _SYNAPSE_RECONNECT_ATTEMPTS=0
typeset -g _SYNAPSE_MAX_RECONNECT=3
typeset -g _SYNAPSE_LAST_RECONNECT_MINUTE=0
typeset -gi _SYNAPSE_RECENT_CMD_MAX=10
typeset -ga _SYNAPSE_RECENT_COMMANDS=()

# --- Modules ---
zmodload zsh/net/socket 2>/dev/null || { return; }
zmodload zsh/zle 2>/dev/null || { return; }
zmodload zsh/system 2>/dev/null  # for sysread/syswrite

# --- Helpers ---

# Generate a short session ID
_synapse_generate_session_id() {
    _SYNAPSE_SESSION_ID="$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
}

# Find the daemon binary
_synapse_find_binary() {
    local bin
    # Check common locations
    for bin in \
        "$(command -v synapse 2>/dev/null)" \
        "${0:A:h:h}/target/release/synapse" \
        "${0:A:h:h}/target/debug/synapse"; do
        [[ -x "$bin" ]] && { echo "$bin"; return 0; }
    done
    return 1
}

# Get socket path (mirrors Rust logic)
_synapse_socket_path() {
    if [[ -n "$XDG_RUNTIME_DIR" ]]; then
        echo "${XDG_RUNTIME_DIR}/synapse.sock"
    else
        echo "/tmp/synapse-$(id -u).sock"
    fi
}

# Get PID file path
_synapse_pid_path() {
    local sock="$(_synapse_socket_path)"
    echo "${sock%.sock}.pid"
}

# Get lock file path
_synapse_lock_path() {
    local sock="$(_synapse_socket_path)"
    echo "${sock%.sock}.lock"
}

# Check if daemon is running
_synapse_daemon_running() {
    local pid_file="$(_synapse_pid_path)"
    [[ -f "$pid_file" ]] || return 1
    local pid="$(< "$pid_file")"
    [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

# Start daemon if needed (with lock to prevent races)
_synapse_ensure_daemon() {
    _synapse_daemon_running && return 0

    local bin
    bin="$(_synapse_find_binary)" || return 1
    local lock_file="$(_synapse_lock_path)"

    # Use flock to prevent race conditions
    (
        flock -n 9 || return 0  # Another shell is starting it
        # Double-check after acquiring lock
        _synapse_daemon_running && return 0
        "$bin" daemon start &>/dev/null &
        disown
        # Wait briefly for daemon to start
        local i
        for i in 1 2 3 4 5; do
            sleep 0.1
            _synapse_daemon_running && return 0
        done
        return 1
    ) 9>"$lock_file"
}

# --- Connection Management ---

_synapse_connect() {
    _synapse_disconnect

    local sock="$(_synapse_socket_path)"
    [[ -S "$sock" ]] || return 1

    # Connect via zsocket
    zsocket "$sock" 2>/dev/null || return 1
    _SYNAPSE_SOCKET_FD="$REPLY"
    _SYNAPSE_CONNECTED=1

    # Register async handler for pushed updates
    zle -F "$_SYNAPSE_SOCKET_FD" _synapse_async_handler

    return 0
}

_synapse_disconnect() {
    if [[ -n "$_SYNAPSE_SOCKET_FD" ]]; then
        zle -F "$_SYNAPSE_SOCKET_FD" 2>/dev/null  # Unregister handler
        exec {_SYNAPSE_SOCKET_FD}>&- 2>/dev/null   # Close fd
        _SYNAPSE_SOCKET_FD=""
    fi
    _SYNAPSE_CONNECTED=0
    _SYNAPSE_CURRENT_SUGGESTION=""
    _SYNAPSE_CURRENT_SOURCE=""
    POSTDISPLAY=""
}

# --- Protocol ---

# Send a JSON request and read the response
_synapse_request() {
    local json="$1"
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || return 1
    [[ -n "$_SYNAPSE_SOCKET_FD" ]] || return 1

    # Write request
    print -u "$_SYNAPSE_SOCKET_FD" "$json" 2>/dev/null || {
        _synapse_disconnect
        return 1
    }

    # Read response with timeout (50ms)
    local response=""
    if read -t 0.05 -u "$_SYNAPSE_SOCKET_FD" response 2>/dev/null; then
        echo "$response"
        return 0
    fi

    return 1
}

# Minimal JSON value extraction (no jq dependency)
# Usage: _synapse_json_get '{"key":"value"}' key
_synapse_json_get() {
    local json="$1" key="$2"
    # Match "key":"value" or "key":number or "key":0.number
    local pattern="\"${key}\"[[:space:]]*:[[:space:]]*"
    if [[ "$json" =~ ${pattern}\"([^\"]*)\" ]]; then
        echo "${match[1]}"
    elif [[ "$json" =~ ${pattern}([0-9.]+) ]]; then
        echo "${match[1]}"
    fi
}

# Build a suggest request JSON
_synapse_build_suggest_request() {
    local buffer="$1" cursor_pos="$2" cwd="$3"

    # Escape special chars in buffer for JSON
    local escaped_buffer="${buffer//\\/\\\\}"
    escaped_buffer="${escaped_buffer//\"/\\\"}"
    escaped_buffer="${escaped_buffer//$'\n'/\\n}"
    escaped_buffer="${escaped_buffer//$'\t'/\\t}"

    local escaped_cwd="${cwd//\\/\\\\}"
    escaped_cwd="${escaped_cwd//\"/\\\"}"

    # Build recent commands array
    local recent_json="["
    local first=1
    local cmd
    for cmd in "${_SYNAPSE_RECENT_COMMANDS[@]}"; do
        local escaped_cmd="${cmd//\\/\\\\}"
        escaped_cmd="${escaped_cmd//\"/\\\"}"
        [[ $first -eq 1 ]] && first=0 || recent_json+=","
        recent_json+="\"${escaped_cmd}\""
    done
    recent_json+="]"

    echo "{\"type\":\"suggest\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"buffer\":\"${escaped_buffer}\",\"cursor_pos\":${cursor_pos},\"cwd\":\"${escaped_cwd}\",\"last_exit_code\":${_SYNAPSE_LAST_EXIT:-0},\"recent_commands\":${recent_json}}"
}

# --- Ghost Text Rendering ---

_synapse_show_suggestion() {
    local full_suggestion="$1"
    local buffer="$BUFFER"

    if [[ -z "$full_suggestion" ]] || [[ -z "$buffer" ]]; then
        POSTDISPLAY=""
        _SYNAPSE_CURRENT_SUGGESTION=""
        return
    fi

    # Only show the completion part (after what the user typed)
    if [[ "$full_suggestion" == "$buffer"* ]]; then
        local completion="${full_suggestion#$buffer}"
        if [[ -n "$completion" ]]; then
            POSTDISPLAY="$completion"
            _SYNAPSE_CURRENT_SUGGESTION="$full_suggestion"
        else
            POSTDISPLAY=""
            _SYNAPSE_CURRENT_SUGGESTION=""
        fi
    else
        POSTDISPLAY=""
        _SYNAPSE_CURRENT_SUGGESTION=""
    fi
}

_synapse_clear_suggestion() {
    POSTDISPLAY=""
    _SYNAPSE_CURRENT_SUGGESTION=""
    _SYNAPSE_CURRENT_SOURCE=""
}

# --- Core Widget ---

# Request a suggestion for the current buffer
_synapse_suggest() {
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || return

    local buffer="$BUFFER"
    local cursor="$CURSOR"

    # Don't suggest for empty buffer
    if [[ -z "$buffer" ]]; then
        _synapse_clear_suggestion
        return
    fi

    local json
    json="$(_synapse_build_suggest_request "$buffer" "$cursor" "$PWD")"

    local response
    response="$(_synapse_request "$json")" || return

    local text
    text="$(_synapse_json_get "$response" "text")"
    _SYNAPSE_CURRENT_SOURCE="$(_synapse_json_get "$response" "source")"

    _synapse_show_suggestion "$text"
}

# --- Async Handler ---

# Called by zle -F when the daemon pushes data
_synapse_async_handler() {
    local fd="$1"

    # Check for error condition
    if [[ "$2" == *err* ]] || [[ "$2" == *hup* ]]; then
        _synapse_disconnect
        return
    fi

    local response=""
    if read -u "$fd" response 2>/dev/null; then
        local msg_type
        msg_type="$(_synapse_json_get "$response" "type")"

        if [[ "$msg_type" == "update" ]]; then
            local text
            text="$(_synapse_json_get "$response" "text")"
            _SYNAPSE_CURRENT_SOURCE="$(_synapse_json_get "$response" "source")"
            _synapse_show_suggestion "$text"
            zle -R  # Redraw
        fi
    fi
}

# --- Interaction Reporting ---

_synapse_report_interaction() {
    local action="$1"
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || return
    [[ -n "$_SYNAPSE_CURRENT_SUGGESTION" ]] || return

    local escaped_suggestion="${_SYNAPSE_CURRENT_SUGGESTION//\\/\\\\}"
    escaped_suggestion="${escaped_suggestion//\"/\\\"}"
    local escaped_buffer="${BUFFER//\\/\\\\}"
    escaped_buffer="${escaped_buffer//\"/\\\"}"
    local source="${_SYNAPSE_CURRENT_SOURCE:-history}"

    local json="{\"type\":\"interaction\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"action\":\"${action}\",\"suggestion\":\"${escaped_suggestion}\",\"source\":\"${source}\",\"buffer_at_action\":\"${escaped_buffer}\"}"

    # Fire and forget — don't wait for response
    print -u "$_SYNAPSE_SOCKET_FD" "$json" 2>/dev/null
}

# --- Key Widgets ---

# Override self-insert to trigger suggestions on every keypress
_synapse_self_insert() {
    # Check if we should report ignore (user typed something different)
    if [[ -n "$_SYNAPSE_CURRENT_SUGGESTION" ]]; then
        local next_char="${KEYS}"
        local expected=""
        if [[ "$_SYNAPSE_CURRENT_SUGGESTION" == "$BUFFER"* ]]; then
            expected="${_SYNAPSE_CURRENT_SUGGESTION:$#BUFFER:1}"
        fi
        if [[ -n "$expected" ]] && [[ "$next_char" != "$expected" ]]; then
            _synapse_report_interaction "ignore"
        fi
    fi

    zle .self-insert
    _synapse_suggest
}

# Override backward-delete-char to re-suggest
_synapse_backward_delete_char() {
    zle .backward-delete-char
    _synapse_suggest
}

# Accept the full suggestion
_synapse_accept() {
    if [[ -n "$_SYNAPSE_CURRENT_SUGGESTION" ]] && [[ -n "$POSTDISPLAY" ]]; then
        _synapse_report_interaction "accept"
        BUFFER="$_SYNAPSE_CURRENT_SUGGESTION"
        CURSOR=${#BUFFER}
        _synapse_clear_suggestion
    else
        # Fall through to default behavior (move cursor right)
        zle .forward-char
    fi
}

# Accept the next word from the suggestion
_synapse_accept_word() {
    if [[ -n "$_SYNAPSE_CURRENT_SUGGESTION" ]] && [[ -n "$POSTDISPLAY" ]]; then
        local remaining="${_SYNAPSE_CURRENT_SUGGESTION#$BUFFER}"
        # Extract next word (up to next space or end)
        local next_word="${remaining%% *}"
        if [[ "$remaining" == *" "* ]] && [[ "$next_word" != "$remaining" ]]; then
            next_word+=" "
        fi
        BUFFER+="$next_word"
        CURSOR=${#BUFFER}
        _synapse_show_suggestion "$_SYNAPSE_CURRENT_SUGGESTION"
    else
        zle .forward-word
    fi
}

# Dismiss the current suggestion
_synapse_dismiss() {
    if [[ -n "$_SYNAPSE_CURRENT_SUGGESTION" ]]; then
        _synapse_report_interaction "dismiss"
        _synapse_clear_suggestion
    else
        zle .send-break
    fi
}

# --- Lifecycle Hooks ---

# precmd: runs before each prompt
_synapse_precmd() {
    # Store last exit code
    _SYNAPSE_LAST_EXIT=$?

    # Try to connect/reconnect if needed
    if [[ $_SYNAPSE_CONNECTED -eq 0 ]]; then
        local current_minute=$(( EPOCHSECONDS / 60 ))
        if [[ "$current_minute" != "$_SYNAPSE_LAST_RECONNECT_MINUTE" ]]; then
            _SYNAPSE_RECONNECT_ATTEMPTS=0
            _SYNAPSE_LAST_RECONNECT_MINUTE="$current_minute"
        fi

        if [[ $_SYNAPSE_RECONNECT_ATTEMPTS -lt $_SYNAPSE_MAX_RECONNECT ]]; then
            (( _SYNAPSE_RECONNECT_ATTEMPTS++ ))
            _synapse_ensure_daemon
            _synapse_connect
        fi
    fi

    # Clear any leftover ghost text
    _synapse_clear_suggestion
}

# preexec: runs before each command execution
_synapse_preexec() {
    local cmd="$1"

    # Track recent commands
    _SYNAPSE_RECENT_COMMANDS=("$cmd" "${_SYNAPSE_RECENT_COMMANDS[@]:0:$(( _SYNAPSE_RECENT_CMD_MAX - 1 ))}")

    # Clear ghost text
    _synapse_clear_suggestion
}

# --- Setup ---

_synapse_init() {
    # Generate session ID
    _synapse_generate_session_id

    # Register widgets
    zle -N self-insert _synapse_self_insert
    zle -N backward-delete-char _synapse_backward_delete_char
    zle -N synapse-accept _synapse_accept
    zle -N synapse-accept-word _synapse_accept_word
    zle -N synapse-dismiss _synapse_dismiss

    # Keybindings
    bindkey '^[[C' synapse-accept         # Right arrow
    bindkey '^[[1;5C' synapse-accept-word # Ctrl+Right arrow
    bindkey '^[' synapse-dismiss          # Escape

    # Hooks
    autoload -Uz add-zsh-hook
    add-zsh-hook precmd _synapse_precmd
    add-zsh-hook preexec _synapse_preexec

    # Initial connection
    _synapse_ensure_daemon
    _synapse_connect
}

# Initialize
_synapse_init
