#!/usr/bin/env zsh
# Synapse — Intelligent Zsh command suggestions via ghost text
# Source this file in your .zshrc via: eval "$(synapse)"

# Clean up previous instance on re-source (e.g. `source ~/.zshrc`)
if [[ -n "$_SYNAPSE_LOADED" ]]; then
    _synapse_cleanup 2>/dev/null
fi
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
typeset -gi _SYNAPSE_REQUEST_FAILURES=0
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
typeset -g _SYNAPSE_DROPDOWN_SELECTED=""
typeset -g _SYNAPSE_DROPDOWN_INSERT_KEY=""
typeset -gi _SYNAPSE_HISTORY_BROWSING=0

# --- Debounce State ---
typeset -g _SYNAPSE_LAST_SUGGEST_TIME=0
typeset -gi _SYNAPSE_LAST_SUGGEST_BUFLEN=0

# --- Paste State ---
typeset -gi _SYNAPSE_PASTING=0
typeset -g _SYNAPSE_BRACKETED_PASTE_WIDGET="_synapse-orig-bracketed-paste"

# --- Natural Language State ---
typeset -gi _SYNAPSE_NL_MODE=0
typeset -gi _SYNAPSE_NL_ERROR_SHOWN=0
typeset -g _SYNAPSE_NL_PREFIX="?"
typeset -g _SYNAPSE_ASYNC_BUFFER=""

# --- Modules ---
zmodload zsh/net/socket 2>/dev/null || { return; }
zmodload zsh/zle 2>/dev/null || { return; }
zmodload zsh/system 2>/dev/null  # for sysread/syswrite
zmodload zsh/datetime 2>/dev/null  # for EPOCHREALTIME (debounce)

# --- Helpers ---

# Generate a short session ID
_synapse_generate_session_id() {
    _SYNAPSE_SESSION_ID="$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
}

# Find the daemon binary
_synapse_find_binary() {
    # Allow explicit override via env var
    if [[ -n "$SYNAPSE_BIN" ]] && [[ -x "$SYNAPSE_BIN" ]]; then
        echo "$SYNAPSE_BIN"
        return 0
    fi
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
    if [[ -n "$SYNAPSE_SOCKET" ]]; then
        echo "$SYNAPSE_SOCKET"
    elif [[ -n "$XDG_RUNTIME_DIR" ]]; then
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
    setopt local_options no_monitor
    _synapse_daemon_running && return 0

    local bin
    bin="$(_synapse_find_binary)" || return 1
    local lock_file="$(_synapse_lock_path)"

    # Use zsystem flock (from zsh/system) — works on both macOS and Linux
    local lock_fd
    if ! zsystem flock -t 0 -f lock_fd "$lock_file" 2>/dev/null; then
        return 0  # Another shell is starting it
    fi

    # Double-check after acquiring lock
    if _synapse_daemon_running; then
        exec {lock_fd}>&-
        return 0
    fi

    "$bin" start &>/dev/null &
    disown

    # Wait briefly for daemon to start
    local i
    for i in 1 2 3 4 5; do
        sleep 0.1
        if _synapse_daemon_running; then
            exec {lock_fd}>&-
            return 0
        fi
    done

    exec {lock_fd}>&-
    return 1
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
    _SYNAPSE_REQUEST_FAILURES=0
    _SYNAPSE_ASYNC_BUFFER=""

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
    _SYNAPSE_ASYNC_BUFFER=""
    POSTDISPLAY=""
}

# --- Protocol ---

# Return 0 if BUFFER starts with "<nl_prefix> ".
_synapse_buffer_has_nl_prefix() {
    local prefix_len=${#_SYNAPSE_NL_PREFIX}
    (( prefix_len > 0 )) || return 1
    (( ${#BUFFER} >= prefix_len + 2 )) || return 1
    [[ "${BUFFER[1,$prefix_len]}" == "$_SYNAPSE_NL_PREFIX" ]] || return 1
    [[ "${BUFFER[$(( prefix_len + 1 ))]}" == " " ]]
}

# Extract NL query text from BUFFER (without "<nl_prefix> ").
_synapse_nl_query_from_buffer() {
    local prefix_len=${#_SYNAPSE_NL_PREFIX}
    local start=$(( prefix_len + 2 ))
    if (( start > ${#BUFFER} )); then
        echo ""
    else
        echo "${BUFFER[$start,-1]}"
    fi
}

# Send a JSON request and read the response
_synapse_request() {
    local json="$1"
    local expected_type="${2:-}"
    local timeout="${3:-0.05}"
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || return 1
    [[ -n "$_SYNAPSE_SOCKET_FD" ]] || return 1

    # Temporarily disable async fd handler while waiting for this request's response.
    zle -F "$_SYNAPSE_SOCKET_FD" 2>/dev/null

    # Write request
    print -u "$_SYNAPSE_SOCKET_FD" "$json" 2>/dev/null || {
        _synapse_disconnect
        return 1
    }

    # Read response, skipping async update frames while waiting for the expected type.
    local response=""
    local reads=0
    local max_reads=$(( timeout / 0.01 ))
    max_reads="${max_reads%.*}"
    [[ -n "$max_reads" ]] || max_reads=5
    (( max_reads < 1 )) && max_reads=1
    while (( reads < max_reads )); do
        if read -t 0.01 -u "$_SYNAPSE_SOCKET_FD" response 2>/dev/null; then
            local -a _tsv_fields
            IFS=$'\t' read -rA _tsv_fields <<< "$response"
            local frame_type="${_tsv_fields[1]}"

            if [[ "$frame_type" == "update" ]]; then
                _synapse_handle_update "$response"
                continue
            fi

            if [[ -n "$expected_type" ]] && [[ "$frame_type" != "$expected_type" ]]; then
                # Ignore unrelated frames and continue waiting for the request response.
                (( reads++ ))
                continue
            fi

            _SYNAPSE_REQUEST_FAILURES=0
            zle -F "$_SYNAPSE_SOCKET_FD" _synapse_async_handler 2>/dev/null
            echo "$response"
            return 0
        fi
        (( reads++ ))
    done

    # Track consecutive failures to detect dead connections
    (( _SYNAPSE_REQUEST_FAILURES++ ))
    if (( _SYNAPSE_REQUEST_FAILURES >= 3 )); then
        _synapse_disconnect
    else
        zle -F "$_SYNAPSE_SOCKET_FD" _synapse_async_handler 2>/dev/null
    fi

    return 1
}

# Escape a string for safe JSON inclusion.
_synapse_json_escape() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    value="${value//$'\n'/\\n}"
    value="${value//$'\t'/\\t}"
    echo "$value"
}

# Build recent_commands JSON array.
_synapse_build_recent_commands_json() {
    local items=()
    local cmd
    for cmd in "${_SYNAPSE_RECENT_COMMANDS[@]}"; do
        items+=("\"$(_synapse_json_escape "$cmd")\"")
    done

    if (( ${#items[@]} == 0 )); then
        echo "[]"
    else
        echo "[${(j:,:)items}]"
    fi
}

# Build env_hints JSON object for daemon requests.
_synapse_build_env_hints_json() {
    local hints=()
    local key val
    for key in PATH VIRTUAL_ENV; do
        val="${(P)key}"
        [[ -n "$val" ]] || continue
        hints+=("\"${key}\":\"$(_synapse_json_escape "$val")\"")
    done

    if (( ${#hints[@]} == 0 )); then
        echo "{}"
    else
        echo "{${(j:,:)hints}}"
    fi
}

# Build a suggest request JSON
_synapse_build_suggest_request() {
    local buffer="$1" cursor_pos="$2" cwd="$3"

    local escaped_buffer escaped_cwd recent_json
    escaped_buffer="$(_synapse_json_escape "$buffer")"
    escaped_cwd="$(_synapse_json_escape "$cwd")"
    recent_json="$(_synapse_build_recent_commands_json)"
    local env_hints_json
    env_hints_json="$(_synapse_build_env_hints_json)"

    echo "{\"type\":\"suggest\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"buffer\":\"${escaped_buffer}\",\"cursor_pos\":${cursor_pos},\"cwd\":\"${escaped_cwd}\",\"last_exit_code\":${_SYNAPSE_LAST_EXIT:-0},\"recent_commands\":${recent_json},\"env_hints\":${env_hints_json}}"
}

# Build a list_suggestions request JSON
_synapse_build_list_request() {
    local buffer="$1" cursor_pos="$2" cwd="$3" max_results="${4:-10}"

    local escaped_buffer escaped_cwd recent_json
    escaped_buffer="$(_synapse_json_escape "$buffer")"
    escaped_cwd="$(_synapse_json_escape "$cwd")"
    recent_json="$(_synapse_build_recent_commands_json)"
    local env_hints_json
    env_hints_json="$(_synapse_build_env_hints_json)"

    echo "{\"type\":\"list_suggestions\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"buffer\":\"${escaped_buffer}\",\"cursor_pos\":${cursor_pos},\"cwd\":\"${escaped_cwd}\",\"max_results\":${max_results},\"last_exit_code\":${_SYNAPSE_LAST_EXIT:-0},\"recent_commands\":${recent_json},\"env_hints\":${env_hints_json}}"
}

# Build a natural_language request JSON
_synapse_build_nl_request() {
    local query="$1" cwd="$2"

    local escaped_query escaped_cwd recent_json env_hints_json
    escaped_query="$(_synapse_json_escape "$query")"
    escaped_cwd="$(_synapse_json_escape "$cwd")"
    recent_json="$(_synapse_build_recent_commands_json)"
    env_hints_json="$(_synapse_build_env_hints_json)"

    echo "{\"type\":\"natural_language\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"query\":\"${escaped_query}\",\"cwd\":\"${escaped_cwd}\",\"recent_commands\":${recent_json},\"env_hints\":${env_hints_json}}"
}

# Build an explain request JSON
_synapse_build_explain_request() {
    local command="$1"

    local escaped_command
    escaped_command="$(_synapse_json_escape "$command")"

    echo "{\"type\":\"explain\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"command\":\"${escaped_command}\"}"
}

# --- Ghost Text Rendering ---

_synapse_show_suggestion() {
    local full_suggestion="$1"
    local buffer="$BUFFER"

    # Only show suggestions when cursor is at end of buffer
    if [[ "$CURSOR" -ne "${#BUFFER}" ]]; then
        POSTDISPLAY=""
        _SYNAPSE_CURRENT_SUGGESTION=""
        region_highlight=()
        return
    fi

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
            # Style ghost text dim (same as dropdown items)
            local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
            region_highlight=("${base_offset} $(( base_offset + ${#completion} )) fg=240")
        else
            POSTDISPLAY=""
            _SYNAPSE_CURRENT_SUGGESTION=""
            region_highlight=()
        fi
    else
        POSTDISPLAY=""
        _SYNAPSE_CURRENT_SUGGESTION=""
        region_highlight=()
    fi
}

_synapse_clear_suggestion() {
    POSTDISPLAY=""
    _SYNAPSE_CURRENT_SUGGESTION=""
    _SYNAPSE_CURRENT_SOURCE=""
    region_highlight=()
}

# Clear stale ghost text after any widget that moves cursor or changes buffer.
# This catches widgets we don't explicitly override (cursor movement, kill-word, etc.).
_synapse_line_pre_redraw() {
    [[ -z "$_SYNAPSE_CURRENT_SUGGESTION" ]] && return
    if [[ -z "$BUFFER" ]] || [[ "$CURSOR" -ne "${#BUFFER}" ]] || \
       [[ "$_SYNAPSE_CURRENT_SUGGESTION" != "$BUFFER"* ]]; then
        _synapse_clear_suggestion
    fi
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
        region_highlight+=("${base_offset} ${ghost_end} fg=240")
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
            region_highlight+=("${line_start} ${text_end} fg=240")
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
    _SYNAPSE_DROPDOWN_SELECTED=""
    _SYNAPSE_DROPDOWN_INSERT_KEY=""
    POSTDISPLAY=""
    region_highlight=()
}

# --- Dropdown Protocol ---

# Parse the suggestion_list response and populate dropdown state.
# TSV format: list\t<count>\t<text>\t<source>\t<desc>\t<kind>\t...
_synapse_parse_suggestion_list() {
    local response="$1"

    _SYNAPSE_DROPDOWN_ITEMS=()
    _SYNAPSE_DROPDOWN_SOURCES=()
    _SYNAPSE_DROPDOWN_DESCS=()
    _SYNAPSE_DROPDOWN_KINDS=()

    local -a _tsv_fields
    _tsv_fields=("${(@s:	:)response}")
    if [[ "${_tsv_fields[1]}" != "list" ]]; then
        _SYNAPSE_DROPDOWN_COUNT=0
        return
    fi

    # _tsv_fields[1]=list, _tsv_fields[2]=count, then 4 fields per item
    local count="${_tsv_fields[2]}"
    local i
    for (( i=0; i<count; i++ )); do
        local base=$(( 3 + i * 4 ))
        _SYNAPSE_DROPDOWN_ITEMS+=("${_tsv_fields[$base]}")
        _SYNAPSE_DROPDOWN_SOURCES+=("${_tsv_fields[$(( base + 1 ))]}")
        _SYNAPSE_DROPDOWN_DESCS+=("${_tsv_fields[$(( base + 2 ))]}")
        _SYNAPSE_DROPDOWN_KINDS+=("${_tsv_fields[$(( base + 3 ))]}")
    done

    _SYNAPSE_DROPDOWN_COUNT=$count
}

# --- Core Widget ---

# Request a suggestion for the current buffer
_synapse_suggest() {
    # Try quick reconnect if disconnected (daemon may have restarted)
    if [[ $_SYNAPSE_CONNECTED -eq 0 ]]; then
        _synapse_connect 2>/dev/null || return
    fi

    local buffer="$BUFFER"
    local cursor="$CURSOR"

    # Don't suggest for empty buffer
    if [[ -z "$buffer" ]]; then
        _synapse_clear_suggestion
        return
    fi

    # Only suggest when cursor is at end of buffer
    if [[ "$cursor" -ne "${#buffer}" ]]; then
        _synapse_clear_suggestion
        return
    fi

    # Debounce: skip if <30ms since last suggest and buffer changed by 1 char
    if (( ${+EPOCHREALTIME} )); then
        local now=${EPOCHREALTIME}
        local elapsed=$(( now - _SYNAPSE_LAST_SUGGEST_TIME ))
        local buflen_diff=$(( ${#buffer} - _SYNAPSE_LAST_SUGGEST_BUFLEN ))
        if (( elapsed < 0.030 )) && (( buflen_diff == 1 || buflen_diff == -1 )); then
            return
        fi
        _SYNAPSE_LAST_SUGGEST_TIME=$now
        _SYNAPSE_LAST_SUGGEST_BUFLEN=${#buffer}
    fi

    local json
    json="$(_synapse_build_suggest_request "$buffer" "$cursor" "$PWD")"

    local response
    response="$(_synapse_request "$json" "suggest")" || return

    # TSV: suggest\ttext\tsource
    local -a _tsv_fields
    IFS=$'\t' read -rA _tsv_fields <<< "$response"
    [[ "${_tsv_fields[1]}" == "suggest" ]] || return
    _SYNAPSE_CURRENT_SOURCE="${_tsv_fields[3]}"

    _synapse_show_suggestion "${_tsv_fields[2]}"
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

    # Read available bytes with sysread to avoid zsh read-buffering behavior with
    # zle -F. Keep a persistent buffer and only process complete newline-delimited
    # frames to avoid dropping partial lines split across reads.
    local raw_data=""
    if sysread -i "$fd" -c 4096 raw_data 2>/dev/null; then
        _SYNAPSE_ASYNC_BUFFER+="$raw_data"
        local line
        local needs_redraw=0
        while [[ "$_SYNAPSE_ASYNC_BUFFER" == *$'\n'* ]]; do
            line="${_SYNAPSE_ASYNC_BUFFER%%$'\n'*}"
            _SYNAPSE_ASYNC_BUFFER="${_SYNAPSE_ASYNC_BUFFER#*$'\n'}"
            [[ -n "$line" ]] || continue
            _synapse_handle_update "$line" && needs_redraw=1
        done
        if (( needs_redraw )); then
            zle -R 2>/dev/null
        fi
    else
        # EOF or read error — daemon connection lost
        _synapse_disconnect
    fi
}

_synapse_handle_update() {
    local response="$1"
    local -a _tsv_fields
    IFS=$'\t' read -rA _tsv_fields <<< "$response"

    local msg_type="${_tsv_fields[1]}"

    [[ "$msg_type" == "update" ]] || return 1

    # Skip async updates while dropdown is open, in NL mode, or during paste
    if [[ $_SYNAPSE_DROPDOWN_OPEN -eq 1 ]] || [[ $_SYNAPSE_NL_MODE -eq 1 ]] || (( _SYNAPSE_PASTING )); then
        return 1
    fi

    local text="${_tsv_fields[2]}"
    local source="${_tsv_fields[3]}"

    _SYNAPSE_CURRENT_SOURCE="$source"
    _synapse_show_suggestion "$text"
    return 0
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
    _SYNAPSE_HISTORY_BROWSING=0

    # During paste, just insert the character — no suggestion logic
    if (( _SYNAPSE_PASTING )); then
        zle .self-insert
        return
    fi

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

    # Check for natural language mode: "? " prefix with enough characters
    local query=""
    if _synapse_buffer_has_nl_prefix; then
        query="$(_synapse_nl_query_from_buffer)"
    fi
    if [[ -n "$query" ]]; then
        _synapse_nl_suggest
    else
        if ! _synapse_buffer_has_nl_prefix; then
            _SYNAPSE_NL_MODE=0
            _SYNAPSE_NL_ERROR_SHOWN=0
        fi
        _synapse_suggest
    fi
}

# Override bracketed-paste to suppress suggestions during paste
_synapse_bracketed_paste() {
    _SYNAPSE_PASTING=1
    _synapse_clear_suggestion

    # Delegate to the previously-registered widget when available so we don't
    # clobber user/plugin bracketed-paste customizations.
    local paste_widget="${_SYNAPSE_BRACKETED_PASTE_WIDGET}"
    if ! zle "$paste_widget" "$@" 2>/dev/null; then
        zle .bracketed-paste "$@" 2>/dev/null
    fi

    _SYNAPSE_PASTING=0

    # Show NL hint if pasted text has the NL prefix, otherwise leave clean
    if _synapse_buffer_has_nl_prefix; then
        local query="$(_synapse_nl_query_from_buffer)"
        if [[ -n "$query" ]]; then
            _synapse_nl_suggest
        fi
    else
        _SYNAPSE_NL_MODE=0
        _SYNAPSE_NL_ERROR_SHOWN=0
    fi
}

# Mark NL mode active and show hint (no request sent — Enter triggers that)
_synapse_nl_suggest() {
    _SYNAPSE_NL_MODE=1
    _synapse_clear_suggestion
    POSTDISPLAY=$'\n'"  > press Enter to translate"
    local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
    region_highlight=("${base_offset} $(( base_offset + ${#POSTDISPLAY} )) fg=8")
}

# Execute NL query synchronously: send request, wait for LLM result, show in dropdown.
_synapse_nl_execute() {
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || { zle .accept-line; return; }

    local query
    query="$(_synapse_nl_query_from_buffer)"
    if [[ -z "$query" ]]; then
        zle .accept-line
        return
    fi

    # Show thinking indicator (works from widget context)
    POSTDISPLAY=$'\n'"  thinking..."
    local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
    region_highlight=("${base_offset} $(( base_offset + ${#POSTDISPLAY} )) fg=8")
    zle -R

    # Build and send NL request
    local json
    json="$(_synapse_build_nl_request "$query" "$PWD")"

    # Disable async handler while doing synchronous read
    zle -F "$_SYNAPSE_SOCKET_FD" 2>/dev/null

    print -u "$_SYNAPSE_SOCKET_FD" "$json" 2>/dev/null || {
        _synapse_disconnect
        POSTDISPLAY=""
        zle -R
        return
    }

    # Collect update results (skip ack frames), up to 15s
    local -a nl_results=()
    local -a nl_sources=()
    local response="" reads=0
    local max_reads=1500

    while (( reads < max_reads )); do
        if read -t 0.01 -u "$_SYNAPSE_SOCKET_FD" response 2>/dev/null; then
            local -a _fields
            IFS=$'\t' read -rA _fields <<< "$response"

            case "${_fields[1]}" in
                update)
                    nl_results+=("${_fields[2]}")
                    nl_sources+=("${_fields[3]}")
                    # After first update, wait briefly for more
                    max_reads=$(( reads + 300 ))
                    ;;
                error)
                    POSTDISPLAY=$'\n'"  [${_fields[2]}]"
                    base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
                    region_highlight=("${base_offset} $(( base_offset + ${#POSTDISPLAY} )) fg=1")
                    zle -R
                    zle -F "$_SYNAPSE_SOCKET_FD" _synapse_async_handler 2>/dev/null
                    return
                    ;;
                ack) ;;  # skip
            esac
        fi
        (( reads++ ))
    done

    # Re-enable async handler
    zle -F "$_SYNAPSE_SOCKET_FD" _synapse_async_handler 2>/dev/null

    if (( ${#nl_results[@]} == 0 )); then
        POSTDISPLAY=$'\n'"  [no results]"
        base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
        region_highlight=("${base_offset} $(( base_offset + ${#POSTDISPLAY} )) fg=1")
        zle -R
        return
    fi

    # Populate dropdown with NL results
    _SYNAPSE_DROPDOWN_ITEMS=("${nl_results[@]}")
    _SYNAPSE_DROPDOWN_SOURCES=("${nl_sources[@]}")
    _SYNAPSE_DROPDOWN_DESCS=()
    _SYNAPSE_DROPDOWN_KINDS=()
    local i
    for (( i=0; i < ${#nl_results[@]}; i++ )); do
        _SYNAPSE_DROPDOWN_DESCS+=("")
        _SYNAPSE_DROPDOWN_KINDS+=("command")
    done
    _SYNAPSE_DROPDOWN_COUNT=${#nl_results[@]}
    _SYNAPSE_DROPDOWN_INDEX=0
    _SYNAPSE_DROPDOWN_SCROLL=0
    _SYNAPSE_DROPDOWN_OPEN=1

    _SYNAPSE_CURRENT_SUGGESTION="${nl_results[1]}"
    _SYNAPSE_CURRENT_SOURCE="${nl_sources[1]}"

    _synapse_render_dropdown
    zle -R

    # Enter modal dropdown navigation
    zle recursive-edit -K synapse-dropdown

    # Handle selection — replace buffer with the chosen command
    if [[ -n "$_SYNAPSE_DROPDOWN_SELECTED" ]]; then
        BUFFER="$_SYNAPSE_DROPDOWN_SELECTED"
        CURSOR=${#BUFFER}
    fi

    _SYNAPSE_NL_MODE=0
    _SYNAPSE_NL_ERROR_SHOWN=0
    _SYNAPSE_DROPDOWN_SELECTED=""
    _SYNAPSE_DROPDOWN_INSERT_KEY=""
    _synapse_clear_dropdown
    zle reset-prompt
}

# Override backward-delete-char to re-suggest
_synapse_backward_delete_char() {
    _SYNAPSE_HISTORY_BROWSING=0
    zle .backward-delete-char

    (( _SYNAPSE_PASTING )) && return

    local query=""
    if _synapse_buffer_has_nl_prefix; then
        query="$(_synapse_nl_query_from_buffer)"
    fi
    if [[ -n "$query" ]]; then
        _synapse_nl_suggest
    else
        if ! _synapse_buffer_has_nl_prefix; then
            _SYNAPSE_NL_MODE=0
            _SYNAPSE_NL_ERROR_SHOWN=0
        fi
        _synapse_suggest
    fi
}

# Accept the full suggestion
_synapse_accept() {
    if [[ -n "$_SYNAPSE_CURRENT_SUGGESTION" ]] && [[ -n "$POSTDISPLAY" ]]; then
        _synapse_report_interaction "accept"
        BUFFER="$_SYNAPSE_CURRENT_SUGGESTION"
        CURSOR=${#BUFFER}
        _SYNAPSE_NL_MODE=0
        _SYNAPSE_NL_ERROR_SHOWN=0
        _synapse_clear_suggestion
    else
        # Fall through to default behavior (move cursor right)
        zle .forward-char
    fi
}

# Accept line: intercept Enter in NL mode to trigger synchronous NL execution
_synapse_accept_line() {
    _synapse_clear_suggestion
    if _synapse_buffer_has_nl_prefix; then
        _synapse_nl_execute
    else
        _SYNAPSE_NL_MODE=0
        _SYNAPSE_NL_ERROR_SHOWN=0
        zle .accept-line
    fi
}

# Accept the suggestion on Tab, or fall through to normal tab completion
_synapse_tab_accept() {
    if _synapse_buffer_has_nl_prefix; then
        _synapse_nl_execute
    elif [[ -n "$_SYNAPSE_CURRENT_SUGGESTION" ]] && [[ -n "$POSTDISPLAY" ]]; then
        _synapse_report_interaction "accept"
        BUFFER="$_SYNAPSE_CURRENT_SUGGESTION"
        CURSOR=${#BUFFER}
        _SYNAPSE_NL_MODE=0
        _SYNAPSE_NL_ERROR_SHOWN=0
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

# Explain the current NL-generated command (Ctrl+E)
_synapse_explain() {
    # Only works when viewing an LLM-generated command
    if [[ -z "$_SYNAPSE_CURRENT_SUGGESTION" ]] || [[ "$_SYNAPSE_CURRENT_SOURCE" != "llm" ]] || [[ $_SYNAPSE_CONNECTED -ne 1 ]]; then
        zle .end-of-line
        return
    fi

    local command="$_SYNAPSE_CURRENT_SUGGESTION"
    local json
    json="$(_synapse_build_explain_request "$command")"

    local response
    response="$(_synapse_request "$json" "suggest" 5.0)" || return

    local -a _tsv_fields
    IFS=$'\t' read -rA _tsv_fields <<< "$response"

    if [[ "${_tsv_fields[1]}" == "suggest" ]] && [[ -n "${_tsv_fields[2]}" ]]; then
        # Show command + explanation below the query
        POSTDISPLAY=$'\n'"  > ${_SYNAPSE_CURRENT_SUGGESTION}"$'\n'"  ─────────────────────────────"$'\n'"  ${_tsv_fields[2]}"
        local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
        region_highlight=("${base_offset} $(( base_offset + ${#POSTDISPLAY} )) fg=14")
        zle -R
    fi
}

# --- History Navigation ---

# Override up-arrow to track history browsing state
_synapse_up_line_or_history() {
    _SYNAPSE_HISTORY_BROWSING=1
    _synapse_clear_suggestion
    zle .up-line-or-history
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

    # If user is browsing history (via up arrow), pass through to history navigation
    if [[ $_SYNAPSE_HISTORY_BROWSING -eq 1 ]]; then
        zle .down-line-or-history
        # If we returned to the newest entry, stop history browsing mode
        if [[ "$HISTNO" -eq "$HISTCMD" ]]; then
            _SYNAPSE_HISTORY_BROWSING=0
        fi
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
    json="$(_synapse_build_list_request "$BUFFER" "$CURSOR" "$PWD" 50)"

    local response
    response="$(_synapse_request "$json" "list")" || { zle .down-line-or-history; return; }

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

    # Apply results AFTER recursive-edit exits to avoid buffer restoration
    if [[ -n "$_SYNAPSE_DROPDOWN_SELECTED" ]]; then
        BUFFER="$_SYNAPSE_DROPDOWN_SELECTED"
        CURSOR=${#BUFFER}
        _synapse_report_interaction "accept"
    elif [[ -n "$_SYNAPSE_DROPDOWN_INSERT_KEY" ]]; then
        LBUFFER+="$_SYNAPSE_DROPDOWN_INSERT_KEY"
    fi
    _SYNAPSE_DROPDOWN_SELECTED=""
    _SYNAPSE_DROPDOWN_INSERT_KEY=""

    _synapse_clear_dropdown
    zle reset-prompt
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

# Accept selected item: save selection and exit recursive-edit
_synapse_dropdown_accept() {
    # Save selection to flag variable — BUFFER is set by the caller AFTER
    # recursive-edit exits to avoid send-break restoring the pre-edit buffer
    _SYNAPSE_DROPDOWN_SELECTED="${_SYNAPSE_DROPDOWN_ITEMS[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    zle .send-break
}

# Dismiss dropdown: exit recursive-edit
_synapse_dropdown_dismiss() {
    _SYNAPSE_DROPDOWN_SELECTED=""
    zle .send-break
}

# Close dropdown and pass the typed character through
_synapse_dropdown_close_and_insert() {
    # Save the key to insert AFTER recursive-edit exits
    _SYNAPSE_DROPDOWN_INSERT_KEY="${KEYS}"
    _SYNAPSE_DROPDOWN_SELECTED=""
    zle .send-break
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

    # Clear any leftover ghost text, dropdown, history browsing, and NL state
    _SYNAPSE_HISTORY_BROWSING=0
    _SYNAPSE_NL_MODE=0
    _SYNAPSE_NL_ERROR_SHOWN=0
    _SYNAPSE_PASTING=0
    _synapse_clear_dropdown
    _synapse_clear_suggestion
}

# preexec: runs before each command execution
_synapse_preexec() {
    local cmd="$1"

    # Track recent commands
    _SYNAPSE_RECENT_COMMANDS=("$cmd" "${_SYNAPSE_RECENT_COMMANDS[@]:0:$(( _SYNAPSE_RECENT_CMD_MAX - 1 ))}")

    # Notify daemon so history provider stays up to date
    if [[ $_SYNAPSE_CONNECTED -eq 1 ]] && [[ -n "$cmd" ]]; then
        local escaped_cmd="${cmd//\\/\\\\}"
        escaped_cmd="${escaped_cmd//\"/\\\"}"
        print -u "$_SYNAPSE_SOCKET_FD" "{\"type\":\"command_executed\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"command\":\"${escaped_cmd}\"}" 2>/dev/null
    fi

    # Clear ghost text and dropdown
    _synapse_clear_dropdown
    _synapse_clear_suggestion
}

# --- Cleanup (for dev reload) ---

_synapse_cleanup() {
    _synapse_disconnect
    _synapse_clear_dropdown
    _synapse_clear_suggestion
    _SYNAPSE_NL_MODE=0
    _SYNAPSE_NL_ERROR_SHOWN=0
    _SYNAPSE_PASTING=0
    # Restore any bracketed-paste widget that was in place before Synapse.
    zle -A "$_SYNAPSE_BRACKETED_PASTE_WIDGET" bracketed-paste 2>/dev/null
    zle -D "$_SYNAPSE_BRACKETED_PASTE_WIDGET" 2>/dev/null
    add-zsh-hook -d precmd _synapse_precmd 2>/dev/null
    add-zsh-hook -d preexec _synapse_preexec 2>/dev/null
    bindkey -D synapse-dropdown &>/dev/null
    unset _SYNAPSE_LOADED
}

# --- Setup ---

_synapse_init() {
    # Generate session ID
    _synapse_generate_session_id

    # Preserve any existing bracketed-paste widget before installing ours.
    zle -A bracketed-paste "$_SYNAPSE_BRACKETED_PASTE_WIDGET" 2>/dev/null

    # Register widgets
    zle -N self-insert _synapse_self_insert
    zle -N backward-delete-char _synapse_backward_delete_char
    zle -N bracketed-paste _synapse_bracketed_paste
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
    zle -N synapse-up-line-or-history _synapse_up_line_or_history
    zle -N synapse-explain _synapse_explain
    zle -N accept-line _synapse_accept_line
    zle -N zle-line-pre-redraw _synapse_line_pre_redraw

    # Create dropdown keymap (based on main, with overrides)
    # Delete and recreate to pick up any main keymap changes on reload
    bindkey -D synapse-dropdown &>/dev/null
    bindkey -N synapse-dropdown main &>/dev/null
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
        bindkey -M synapse-dropdown -- "$key" synapse-dropdown-close-and-insert
    done

    # Main keymap bindings
    bindkey '\t' synapse-tab-accept       # Tab (accept suggestion or normal completion)
    bindkey '^[[C' synapse-accept         # Right arrow (normal mode)
    bindkey '^[OC' synapse-accept         # Right arrow (application mode)
    bindkey '^[[1;5C' synapse-accept-word # Ctrl+Right arrow
    bindkey '^[' synapse-dismiss          # Escape
    bindkey '^[[A' synapse-up-line-or-history  # Up arrow (normal) — history + flag
    bindkey '^[OA' synapse-up-line-or-history  # Up arrow (application) — history + flag
    bindkey '^[[B' synapse-dropdown-open  # Down arrow (normal) — opens dropdown
    bindkey '^[OB' synapse-dropdown-open  # Down arrow (application) — opens dropdown
    bindkey '^E' synapse-explain          # Ctrl+E — explain NL-generated command (fallback: end-of-line)

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
