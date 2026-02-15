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

# --- Dropdown State ---
typeset -gi _SYNAPSE_DROPDOWN_OPEN=0
typeset -gi _SYNAPSE_DROPDOWN_INDEX=0
typeset -gi _SYNAPSE_DROPDOWN_COUNT=0
typeset -ga _SYNAPSE_DROPDOWN_ITEMS=()
typeset -ga _SYNAPSE_DROPDOWN_SOURCES=()
typeset -ga _SYNAPSE_DROPDOWN_DESCS=()
typeset -ga _SYNAPSE_DROPDOWN_KINDS=()
typeset -gi _SYNAPSE_DROPDOWN_MAX_VISIBLE=8
typeset -gi _SYNAPSE_DROPDOWN_SCROLL=0

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

# Build a list_suggestions request JSON
_synapse_build_list_request() {
    local buffer="$1" cursor_pos="$2" cwd="$3" max_results="${4:-10}"

    local escaped_buffer="${buffer//\\/\\\\}"
    escaped_buffer="${escaped_buffer//\"/\\\"}"
    escaped_buffer="${escaped_buffer//$'\n'/\\n}"
    escaped_buffer="${escaped_buffer//$'\t'/\\t}"

    local escaped_cwd="${cwd//\\/\\\\}"
    escaped_cwd="${escaped_cwd//\"/\\\"}"

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

    echo "{\"type\":\"list_suggestions\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"buffer\":\"${escaped_buffer}\",\"cursor_pos\":${cursor_pos},\"cwd\":\"${escaped_cwd}\",\"max_results\":${max_results},\"last_exit_code\":${_SYNAPSE_LAST_EXIT:-0},\"recent_commands\":${recent_json}}"
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

# --- Dropdown Rendering ---

_synapse_render_dropdown() {
    if [[ $_SYNAPSE_DROPDOWN_COUNT -eq 0 ]]; then
        POSTDISPLAY=""
        return
    fi

    # Cap visible items to terminal height
    local max_vis=$_SYNAPSE_DROPDOWN_MAX_VISIBLE
    if (( max_vis > LINES - 3 )); then
        max_vis=$(( LINES - 3 ))
    fi
    (( max_vis < 1 )) && max_vis=1

    # Adjust scroll offset to keep selected item visible
    if (( _SYNAPSE_DROPDOWN_INDEX < _SYNAPSE_DROPDOWN_SCROLL )); then
        _SYNAPSE_DROPDOWN_SCROLL=$_SYNAPSE_DROPDOWN_INDEX
    elif (( _SYNAPSE_DROPDOWN_INDEX >= _SYNAPSE_DROPDOWN_SCROLL + max_vis )); then
        _SYNAPSE_DROPDOWN_SCROLL=$(( _SYNAPSE_DROPDOWN_INDEX - max_vis + 1 ))
    fi

    local display=""
    local max_width=$(( COLUMNS - 6 ))
    local i start end

    start=$_SYNAPSE_DROPDOWN_SCROLL
    end=$(( start + max_vis ))
    (( end > _SYNAPSE_DROPDOWN_COUNT )) && end=$_SYNAPSE_DROPDOWN_COUNT

    # Show ghost text for selected item first
    local selected_text="${_SYNAPSE_DROPDOWN_ITEMS[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    if [[ "$selected_text" == "$BUFFER"* ]]; then
        local ghost="${selected_text#$BUFFER}"
        display="${ghost}"
    fi

    # Build dropdown lines
    for (( i = start; i < end; i++ )); do
        local text="${_SYNAPSE_DROPDOWN_ITEMS[$(( i + 1 ))]}"
        local source="${_SYNAPSE_DROPDOWN_SOURCES[$(( i + 1 ))]}"
        local desc="${_SYNAPSE_DROPDOWN_DESCS[$(( i + 1 ))]}"

        # Truncate long items
        if (( ${#text} > max_width )); then
            text="${text:0:$(( max_width - 3 ))}..."
        fi

        # Build display line
        local line=""
        if (( i == _SYNAPSE_DROPDOWN_INDEX )); then
            line=$'\n'"  > ${text}"
        else
            line=$'\n'"    ${text}"
        fi

        # Append description if present
        if [[ -n "$desc" ]]; then
            local remaining=$(( max_width - ${#text} - 4 ))
            if (( remaining > 10 )); then
                if (( ${#desc} > remaining )); then
                    desc="${desc:0:$(( remaining - 3 ))}..."
                fi
                line+="  (${desc})"
            fi
        fi

        display+="$line"
    done

    # Status line
    local src="${_SYNAPSE_DROPDOWN_SOURCES[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    display+=$'\n'"  [${src:-?}] $(( _SYNAPSE_DROPDOWN_INDEX + 1 ))/${_SYNAPSE_DROPDOWN_COUNT}"

    POSTDISPLAY="$display"

    # Apply region highlights (memo=synapse for cleanup)
    region_highlight=()

    local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
    local ghost_end=$base_offset

    if [[ "$selected_text" == "$BUFFER"* ]]; then
        local ghost="${selected_text#$BUFFER}"
        ghost_end=$(( base_offset + ${#ghost} ))
        # Ghost text dim
        region_highlight+=("${base_offset} ${ghost_end} fg=8")
    fi

    # Highlight dropdown items
    local pos=$ghost_end
    for (( i = start; i < end; i++ )); do
        local text="${_SYNAPSE_DROPDOWN_ITEMS[$(( i + 1 ))]}"
        if (( ${#text} > max_width )); then
            text="${text:0:$(( max_width - 3 ))}..."
        fi
        local desc="${_SYNAPSE_DROPDOWN_DESCS[$(( i + 1 ))]}"

        local line_start=$(( pos + 1 )) # after \n
        local marker_len=4 # both "  > " and "    " are 4 chars
        local text_start=$(( line_start + marker_len ))
        local text_end=$(( text_start + ${#text} ))

        if (( i == _SYNAPSE_DROPDOWN_INDEX )); then
            # Selected: standout
            region_highlight+=("${line_start} ${text_end} standout")
        else
            # Unselected: dim
            region_highlight+=("${line_start} ${text_end} fg=8")
        fi

        pos=$text_end
        # Account for description if present
        if [[ -n "$desc" ]]; then
            local remaining=$(( max_width - ${#text} - 4 ))
            if (( remaining > 10 )); then
                if (( ${#desc} > remaining )); then
                    desc="${desc:0:$(( remaining - 3 ))}..."
                fi
                pos=$(( pos + ${#desc} + 4 )) # "  (" + desc + ")"
            fi
        fi
    done
}

_synapse_clear_dropdown() {
    _SYNAPSE_DROPDOWN_OPEN=0
    _SYNAPSE_DROPDOWN_INDEX=0
    _SYNAPSE_DROPDOWN_COUNT=0
    _SYNAPSE_DROPDOWN_ITEMS=()
    _SYNAPSE_DROPDOWN_SOURCES=()
    _SYNAPSE_DROPDOWN_DESCS=()
    _SYNAPSE_DROPDOWN_KINDS=()
    _SYNAPSE_DROPDOWN_SCROLL=0
    POSTDISPLAY=""
    region_highlight=()
}

# --- Dropdown Protocol ---

# Parse the suggestion_list response and populate dropdown state
_synapse_parse_suggestion_list() {
    local response="$1"

    # Extract the suggestions array using a simple approach:
    # Each item has "text", "source", "description", "kind" fields
    # We parse by finding each {"text":" pattern and extracting fields
    _SYNAPSE_DROPDOWN_ITEMS=()
    _SYNAPSE_DROPDOWN_SOURCES=()
    _SYNAPSE_DROPDOWN_DESCS=()
    _SYNAPSE_DROPDOWN_KINDS=()

    local rest="$response"
    local count=0

    # Find items by looking for "text":" patterns within the suggestions array.
    # For each item, extract the current JSON object (up to next '}') first,
    # then run field regexes within that scoped string to avoid matching
    # fields from subsequent items.
    while [[ "$rest" =~ '"text"[[:space:]]*:[[:space:]]*"([^"]*)"' ]]; do
        local text="${match[1]}"
        # Advance past the "text" field match
        rest="${rest#*\"text\"*\"${text}\"}"

        # Scope field extraction to the current JSON object (up to next '}')
        local item_rest="${rest%%\}*}"

        # Extract source from scoped context
        local source=""
        if [[ "$item_rest" =~ '"source"[[:space:]]*:[[:space:]]*"([^"]*)"' ]]; then
            source="${match[1]}"
        fi

        # Extract description from scoped context (may not exist or may be null)
        local desc=""
        if [[ "$item_rest" =~ '"description"[[:space:]]*:[[:space:]]*"([^"]*)"' ]]; then
            desc="${match[1]}"
        fi

        # Extract kind from scoped context
        local kind=""
        if [[ "$item_rest" =~ '"kind"[[:space:]]*:[[:space:]]*"([^"]*)"' ]]; then
            kind="${match[1]}"
        fi

        (( count++ ))
        _SYNAPSE_DROPDOWN_ITEMS+=("$text")
        _SYNAPSE_DROPDOWN_SOURCES+=("$source")
        _SYNAPSE_DROPDOWN_DESCS+=("$desc")
        _SYNAPSE_DROPDOWN_KINDS+=("$kind")

        # Move past the current item's closing brace
        rest="${rest#*\}}"
    done

    _SYNAPSE_DROPDOWN_COUNT=$count
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
            # Skip async updates while dropdown is open
            if [[ $_SYNAPSE_DROPDOWN_OPEN -eq 1 ]]; then
                return
            fi
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

# Accept the suggestion on Tab, or fall through to normal tab completion
_synapse_tab_accept() {
    if [[ -n "$_SYNAPSE_CURRENT_SUGGESTION" ]] && [[ -n "$POSTDISPLAY" ]]; then
        _synapse_report_interaction "accept"
        BUFFER="$_SYNAPSE_CURRENT_SUGGESTION"
        CURSOR=${#BUFFER}
        _synapse_clear_suggestion
    else
        zle expand-or-complete
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

# --- Dropdown Widgets ---

# Open dropdown: triggered by Down Arrow
_synapse_dropdown_open() {
    # If dropdown is already open, move down
    if [[ $_SYNAPSE_DROPDOWN_OPEN -eq 1 ]]; then
        _synapse_dropdown_down_impl
        _synapse_render_dropdown
        zle -R
        return
    fi

    # Only open if we have a current suggestion or buffer content
    if [[ -z "$_SYNAPSE_CURRENT_SUGGESTION" ]] && [[ -z "$BUFFER" ]]; then
        # Fall through to normal down-arrow behavior (history search)
        zle .down-line-or-history
        return
    fi

    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || { zle .down-line-or-history; return; }

    # Send list_suggestions request
    local json
    json="$(_synapse_build_list_request "$BUFFER" "$CURSOR" "$PWD" 10)"

    local response
    response="$(_synapse_request "$json")" || { zle .down-line-or-history; return; }

    # Parse response
    _synapse_parse_suggestion_list "$response"

    # Need at least 2 items to show a dropdown
    if (( _SYNAPSE_DROPDOWN_COUNT < 2 )); then
        _synapse_clear_dropdown
        zle .down-line-or-history
        return
    fi

    # Open dropdown
    _SYNAPSE_DROPDOWN_OPEN=1
    _SYNAPSE_DROPDOWN_INDEX=0
    _SYNAPSE_DROPDOWN_SCROLL=0

    # Update current suggestion to the selected item
    _SYNAPSE_CURRENT_SUGGESTION="${_SYNAPSE_DROPDOWN_ITEMS[1]}"
    _SYNAPSE_CURRENT_SOURCE="${_SYNAPSE_DROPDOWN_SOURCES[1]}"

    _synapse_render_dropdown
    zle -R

    # Enter modal navigation via recursive-edit with dropdown keymap
    zle recursive-edit -K synapse-dropdown
    local ret=$?

    # After recursive-edit exits, check what happened
    if (( ret == 0 )); then
        # User accepted — BUFFER was already set inside _synapse_dropdown_accept
        _synapse_report_interaction "accept"
    fi
    # ret != 0 means dismissed (Escape) or typed-through

    _synapse_clear_dropdown
    zle -R
}

_synapse_dropdown_down_impl() {
    (( _SYNAPSE_DROPDOWN_INDEX++ ))
    if (( _SYNAPSE_DROPDOWN_INDEX >= _SYNAPSE_DROPDOWN_COUNT )); then
        _SYNAPSE_DROPDOWN_INDEX=0
    fi
    _SYNAPSE_CURRENT_SUGGESTION="${_SYNAPSE_DROPDOWN_ITEMS[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    _SYNAPSE_CURRENT_SOURCE="${_SYNAPSE_DROPDOWN_SOURCES[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
}

# Move selection down within recursive-edit
_synapse_dropdown_down() {
    _synapse_dropdown_down_impl
    _synapse_render_dropdown
    zle -R
}

# Move selection up within recursive-edit
_synapse_dropdown_up() {
    (( _SYNAPSE_DROPDOWN_INDEX-- ))
    if (( _SYNAPSE_DROPDOWN_INDEX < 0 )); then
        _SYNAPSE_DROPDOWN_INDEX=$(( _SYNAPSE_DROPDOWN_COUNT - 1 ))
    fi
    _SYNAPSE_CURRENT_SUGGESTION="${_SYNAPSE_DROPDOWN_ITEMS[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    _SYNAPSE_CURRENT_SOURCE="${_SYNAPSE_DROPDOWN_SOURCES[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    _synapse_render_dropdown
    zle -R
}

# Accept selected item: set buffer to selected text, then exit recursive-edit
_synapse_dropdown_accept() {
    # Set BUFFER to the selected item BEFORE exiting recursive-edit,
    # because .accept-line would execute the current BUFFER contents.
    local selected="${_SYNAPSE_DROPDOWN_ITEMS[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    if [[ -n "$selected" ]]; then
        BUFFER="$selected"
        CURSOR=${#BUFFER}
    fi
    # Use .accept-search to exit recursive-edit with ret=0 without executing
    zle .accept-search 2>/dev/null || zle .send-break
    return 0
}

# Dismiss dropdown: exit recursive-edit with failure
_synapse_dropdown_dismiss() {
    # Set return value 1 (dismissed)
    zle .send-break
    return 1
}

# Close dropdown and pass the typed character through
_synapse_dropdown_close_and_insert() {
    # We need to exit recursive-edit, then the character will be processed
    # by the main keymap
    LBUFFER+="${KEYS}"
    zle .send-break
    return 1
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

    # Clear any leftover ghost text and dropdown
    _synapse_clear_dropdown
    _synapse_clear_suggestion
}

# preexec: runs before each command execution
_synapse_preexec() {
    local cmd="$1"

    # Track recent commands
    _SYNAPSE_RECENT_COMMANDS=("$cmd" "${_SYNAPSE_RECENT_COMMANDS[@]:0:$(( _SYNAPSE_RECENT_CMD_MAX - 1 ))}")

    # Clear ghost text and dropdown
    _synapse_clear_dropdown
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
    zle -N synapse-tab-accept _synapse_tab_accept
    zle -N synapse-dropdown-open _synapse_dropdown_open
    zle -N synapse-dropdown-down _synapse_dropdown_down
    zle -N synapse-dropdown-up _synapse_dropdown_up
    zle -N synapse-dropdown-accept _synapse_dropdown_accept
    zle -N synapse-dropdown-dismiss _synapse_dropdown_dismiss
    zle -N synapse-dropdown-close-and-insert _synapse_dropdown_close_and_insert

    # Create dropdown keymap (based on main, with overrides)
    bindkey -N synapse-dropdown main
    bindkey -M synapse-dropdown '^[[B' synapse-dropdown-down     # Down arrow (normal)
    bindkey -M synapse-dropdown '^[OB' synapse-dropdown-down     # Down arrow (application)
    bindkey -M synapse-dropdown '^[[A' synapse-dropdown-up       # Up arrow (normal)
    bindkey -M synapse-dropdown '^[OA' synapse-dropdown-up       # Up arrow (application)
    bindkey -M synapse-dropdown '^M' synapse-dropdown-accept     # Enter
    bindkey -M synapse-dropdown '\t' synapse-dropdown-accept     # Tab
    bindkey -M synapse-dropdown '^[[C' synapse-dropdown-accept   # Right arrow (normal)
    bindkey -M synapse-dropdown '^[OC' synapse-dropdown-accept   # Right arrow (application)
    bindkey -M synapse-dropdown '^[' synapse-dropdown-dismiss    # Escape

    # In dropdown keymap, any normal character closes dropdown and inserts
    local key
    for key in {a..z} {A..Z} {0..9} ' ' '/' '.' '-' '_' '~'; do
        bindkey -M synapse-dropdown "$key" synapse-dropdown-close-and-insert
    done

    # Main keymap bindings
    bindkey '\t' synapse-tab-accept       # Tab (accept suggestion or normal completion)
    bindkey '^[[C' synapse-accept         # Right arrow (normal mode)
    bindkey '^[OC' synapse-accept         # Right arrow (application mode)
    bindkey '^[[1;5C' synapse-accept-word # Ctrl+Right arrow
    bindkey '^[' synapse-dismiss          # Escape
    bindkey '^[[B' synapse-dropdown-open  # Down arrow (normal) — opens dropdown
    bindkey '^[OB' synapse-dropdown-open  # Down arrow (application) — opens dropdown

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
