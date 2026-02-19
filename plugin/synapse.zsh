#!/usr/bin/env zsh
# Synapse — Spec engine + NL translation layer for Zsh
# Source this file in your .zshrc via: eval "$(synapse)"
#
# Synapse generates compsys completion functions for CLI tools and provides
# natural language to command translation (? query). Ghost text and dropdown
# suggestions are handled by companion tools (zsh-autosuggestions, fzf-tab).

# Clean up previous instance on re-source (e.g. `source ~/.zshrc`)
if [[ -n "$_SYNAPSE_LOADED" ]]; then
    _synapse_cleanup 2>/dev/null
fi
_SYNAPSE_LOADED=1

# --- Configuration ---
typeset -g _SYNAPSE_SESSION_ID=""
typeset -g _SYNAPSE_SOCKET_FD=""
typeset -g _SYNAPSE_CONNECTED=0
typeset -g _SYNAPSE_RECONNECT_ATTEMPTS=0
typeset -g _SYNAPSE_MAX_RECONNECT=3
typeset -g _SYNAPSE_LAST_RECONNECT_TIME=0
typeset -gi _SYNAPSE_DISCONNECT_WARNED=0
typeset -gi _SYNAPSE_RECENT_CMD_MAX=10
typeset -ga _SYNAPSE_RECENT_COMMANDS=()

# --- NL Dropdown State ---
typeset -gi _SYNAPSE_DROPDOWN_INDEX=0
typeset -gi _SYNAPSE_DROPDOWN_COUNT=0
typeset -ga _SYNAPSE_DROPDOWN_ITEMS=()
typeset -ga _SYNAPSE_DROPDOWN_SOURCES=()
typeset -ga _SYNAPSE_DROPDOWN_DESCS=()
typeset -gi _SYNAPSE_DROPDOWN_MAX_VISIBLE=8
typeset -gi _SYNAPSE_DROPDOWN_SCROLL=0
typeset -g _SYNAPSE_DROPDOWN_SELECTED=""
typeset -g _SYNAPSE_DROPDOWN_INSERT_KEY=""

# --- Natural Language ---
typeset -g _SYNAPSE_NL_PREFIX="?"

# --- Modules ---
zmodload zsh/net/socket 2>/dev/null || { return; }
zmodload zsh/zle 2>/dev/null || { return; }
zmodload zsh/system 2>/dev/null  # for zsystem flock
zmodload zsh/datetime 2>/dev/null  # for EPOCHSECONDS

# --- Helpers ---

# Generate a short session ID
_synapse_generate_session_id() {
    _SYNAPSE_SESSION_ID="$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
}

# Find the daemon binary
_synapse_find_binary() {
    if [[ -n "$SYNAPSE_BIN" ]] && [[ -x "$SYNAPSE_BIN" ]]; then
        echo "$SYNAPSE_BIN"
        return 0
    fi
    local bin
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

    zsocket "$sock" 2>/dev/null || return 1
    _SYNAPSE_SOCKET_FD="$REPLY"
    _SYNAPSE_CONNECTED=1
    return 0
}

_synapse_disconnect() {
    if [[ -n "$_SYNAPSE_SOCKET_FD" ]]; then
        exec {_SYNAPSE_SOCKET_FD}>&- 2>/dev/null
        _SYNAPSE_SOCKET_FD=""
    fi
    _SYNAPSE_CONNECTED=0
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
    echo "${BUFFER[$(( ${#_SYNAPSE_NL_PREFIX} + 2 )),-1]}"
}

# Send a JSON request and read the response.
# Runs inside $() so state mutations don't propagate to the parent shell.
_synapse_request() {
    local json="$1"
    local timeout="${2:-0.05}"
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || return 1
    [[ -n "$_SYNAPSE_SOCKET_FD" ]] || return 1

    print -u "$_SYNAPSE_SOCKET_FD" "$json" 2>/dev/null || return 1

    local response="" reads=0
    local max_reads=$(( timeout / 0.01 ))
    max_reads="${max_reads%.*}"
    [[ -n "$max_reads" ]] || max_reads=5
    (( max_reads < 1 )) && max_reads=1
    while (( reads++ < max_reads )); do
        if read -t 0.01 -u "$_SYNAPSE_SOCKET_FD" response 2>/dev/null; then
            echo "$response"
            return 0
        fi
    done
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

# Build a natural_language request JSON.
_synapse_build_nl_request() {
    local escaped_query="$(_synapse_json_escape "$1")"
    local escaped_cwd="$(_synapse_json_escape "$2")"

    # recent_commands array
    local items=() cmd
    for cmd in "${_SYNAPSE_RECENT_COMMANDS[@]}"; do
        items+=("\"$(_synapse_json_escape "$cmd")\"")
    done
    local recent="[]"
    (( ${#items[@]} )) && recent="[${(j:,:)items}]"

    # env_hints object
    local hints=() key val
    for key in PATH VIRTUAL_ENV; do
        val="${(P)key}"
        [[ -n "$val" ]] || continue
        hints+=("\"${key}\":\"$(_synapse_json_escape "$val")\"")
    done
    local env="{}"
    (( ${#hints[@]} )) && env="{${(j:,:)hints}}"

    echo "{\"type\":\"natural_language\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"query\":\"${escaped_query}\",\"cwd\":\"${escaped_cwd}\",\"recent_commands\":${recent},\"env_hints\":${env}}"
}

# --- Dropdown Rendering (NL results) ---

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

    # Build dropdown lines
    for (( i = start; i < end; i++ )); do
        local text="${_SYNAPSE_DROPDOWN_ITEMS[$(( i + 1 ))]}"
        local desc="${_SYNAPSE_DROPDOWN_DESCS[$(( i + 1 ))]}"

        if (( ${#text} > max_width )); then
            text="${text:0:$(( max_width - 3 ))}..."
        fi

        local line=""
        if (( i == _SYNAPSE_DROPDOWN_INDEX )); then
            line=$'\n'"  > ${text}"
        else
            line=$'\n'"    ${text}"
        fi

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

    # Apply region highlights
    region_highlight=()

    local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
    local pos=$base_offset

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
            region_highlight+=("${line_start} ${text_end} standout")
        else
            region_highlight+=("${line_start} ${text_end} fg=240")
        fi

        pos=$text_end
        if [[ -n "$desc" ]]; then
            local remaining=$(( max_width - ${#text} - 4 ))
            if (( remaining > 10 )); then
                if (( ${#desc} > remaining )); then
                    desc="${desc:0:$(( remaining - 3 ))}..."
                fi
                pos=$(( pos + ${#desc} + 4 ))
            fi
        fi
    done
}

_synapse_clear_dropdown() {
    _SYNAPSE_DROPDOWN_INDEX=0
    _SYNAPSE_DROPDOWN_COUNT=0
    _SYNAPSE_DROPDOWN_ITEMS=()
    _SYNAPSE_DROPDOWN_SOURCES=()
    _SYNAPSE_DROPDOWN_DESCS=()
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

    local -a _tsv_fields
    _tsv_fields=("${(@s:	:)response}")
    if [[ "${_tsv_fields[1]}" != "list" ]]; then
        _SYNAPSE_DROPDOWN_COUNT=0
        return
    fi

    local count="${_tsv_fields[2]}"
    local i
    for (( i=0; i<count; i++ )); do
        local base=$(( 3 + i * 4 ))
        _SYNAPSE_DROPDOWN_ITEMS+=("${_tsv_fields[$base]}")
        _SYNAPSE_DROPDOWN_SOURCES+=("${_tsv_fields[$(( base + 1 ))]}")
        _SYNAPSE_DROPDOWN_DESCS+=("${_tsv_fields[$(( base + 2 ))]}")
    done

    _SYNAPSE_DROPDOWN_COUNT=$count
}

# --- Socket Helpers ---

# Drain stale responses (e.g. Ack from fire-and-forget command_executed/cwd_changed).
# Must be called in the main shell (not a subshell) before _synapse_request.
_synapse_drain_stale() {
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || return
    [[ -n "$_SYNAPSE_SOCKET_FD" ]] || return
    local _junk
    while read -t 0 -u "$_SYNAPSE_SOCKET_FD" _junk 2>/dev/null; do :; done
}

# --- NL Translation ---

# Show a one-line status message under the prompt using a highlight color.
_synapse_set_status_message() {
    local text="$1"
    local color="${2:-8}"
    POSTDISPLAY=$'\n'"  ${text}"
    local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
    region_highlight=("${base_offset} $(( base_offset + ${#POSTDISPLAY} )) fg=${color}")
}

# Execute NL query synchronously
_synapse_nl_execute() {
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || { zle .accept-line; return; }

    local query
    query="$(_synapse_nl_query_from_buffer)"
    if [[ -z "$query" ]]; then
        zle .accept-line
        return
    fi

    _synapse_set_status_message "thinking..." 8
    zle -R

    # Drain stale responses (Ack from fire-and-forget hooks) so the
    # subshell in _synapse_request reads the actual NL response.
    _synapse_drain_stale

    local json
    json="$(_synapse_build_nl_request "$query" "$PWD")"

    local response
    response="$(_synapse_request "$json" 30.0)" || {
        _synapse_set_status_message "[timed out waiting for translation]" 1
        zle -R
        return
    }

    local -a _tsv_fields
    IFS=$'\t' read -rA _tsv_fields <<< "$response"
    if [[ "${_tsv_fields[1]}" == "error" ]]; then
        _synapse_set_status_message "[${_tsv_fields[2]}]" 1
        zle -R
        return
    fi

    if [[ "${_tsv_fields[1]}" != "list" ]]; then
        _synapse_set_status_message "[unexpected NL response]" 1
        zle -R
        return
    fi

    _synapse_parse_suggestion_list "$response"

    if (( _SYNAPSE_DROPDOWN_COUNT == 0 )); then
        _synapse_set_status_message "[no results]" 1
        zle -R
        return
    fi

    _SYNAPSE_DROPDOWN_INDEX=0
    _SYNAPSE_DROPDOWN_SCROLL=0

    _synapse_render_dropdown
    zle -R

    # Enter modal dropdown navigation
    zle recursive-edit -K synapse-dropdown
    _synapse_dropdown_finish
}

# Finish dropdown after recursive-edit
_synapse_dropdown_finish() {
    if [[ -n "$_SYNAPSE_DROPDOWN_SELECTED" ]]; then
        BUFFER="$_SYNAPSE_DROPDOWN_SELECTED"
        CURSOR=${#BUFFER}
    elif [[ -n "$_SYNAPSE_DROPDOWN_INSERT_KEY" ]]; then
        LBUFFER+="$_SYNAPSE_DROPDOWN_INSERT_KEY"
    fi
    _synapse_clear_dropdown
    zle reset-prompt
}

# --- Key Widgets ---

# Accept line: intercept Enter in NL mode to trigger synchronous NL execution
_synapse_accept_line() {
    POSTDISPLAY=""
    region_highlight=()
    if _synapse_buffer_has_nl_prefix; then
        _synapse_nl_execute
    else
        zle .accept-line
    fi
}

# Tab: in NL mode trigger NL execution, otherwise pass to normal completion
_synapse_tab_accept() {
    if _synapse_buffer_has_nl_prefix; then
        _synapse_nl_execute
    else
        zle expand-or-complete
    fi
}

# --- Dropdown Widgets ---

_synapse_dropdown_down() {
    (( _SYNAPSE_DROPDOWN_INDEX++ ))
    (( _SYNAPSE_DROPDOWN_INDEX >= _SYNAPSE_DROPDOWN_COUNT )) && _SYNAPSE_DROPDOWN_INDEX=0
    _synapse_render_dropdown
    zle -R
}

_synapse_dropdown_up() {
    (( _SYNAPSE_DROPDOWN_INDEX-- ))
    if (( _SYNAPSE_DROPDOWN_INDEX < 0 )); then
        _SYNAPSE_DROPDOWN_INDEX=$(( _SYNAPSE_DROPDOWN_COUNT - 1 ))
    fi
    _synapse_render_dropdown
    zle -R
}

_synapse_dropdown_accept() {
    _SYNAPSE_DROPDOWN_SELECTED="${_SYNAPSE_DROPDOWN_ITEMS[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    zle .send-break
}

_synapse_dropdown_dismiss() {
    _SYNAPSE_DROPDOWN_SELECTED=""
    zle .send-break
}

_synapse_dropdown_close_and_insert() {
    _SYNAPSE_DROPDOWN_INSERT_KEY="${KEYS}"
    _SYNAPSE_DROPDOWN_SELECTED=""
    zle .send-break
}

# --- Lifecycle Hooks ---

# precmd: runs before each prompt
_synapse_precmd() {
    # Try to connect/reconnect if needed
    if [[ $_SYNAPSE_CONNECTED -eq 0 ]]; then
        if [[ $_SYNAPSE_DISCONNECT_WARNED -eq 0 ]]; then
            print -u2 "[synapse] daemon not reachable"
            _SYNAPSE_DISCONNECT_WARNED=1
        fi

        local now="$EPOCHSECONDS"
        local elapsed=$(( now - _SYNAPSE_LAST_RECONNECT_TIME ))
        if [[ $elapsed -ge 30 ]]; then
            _SYNAPSE_RECONNECT_ATTEMPTS=0
            _SYNAPSE_LAST_RECONNECT_TIME="$now"
        fi

        if [[ $_SYNAPSE_RECONNECT_ATTEMPTS -lt $_SYNAPSE_MAX_RECONNECT ]]; then
            (( _SYNAPSE_RECONNECT_ATTEMPTS++ ))
            _synapse_ensure_daemon
            _synapse_connect
            if [[ $_SYNAPSE_CONNECTED -eq 1 ]]; then
                _SYNAPSE_DISCONNECT_WARNED=0
            fi
        fi
    fi

    _synapse_clear_dropdown
}

# preexec: runs before each command execution
_synapse_preexec() {
    local cmd="$1"

    _SYNAPSE_RECENT_COMMANDS=("$cmd" "${_SYNAPSE_RECENT_COMMANDS[@]:0:$(( _SYNAPSE_RECENT_CMD_MAX - 1 ))}")

    if [[ $_SYNAPSE_CONNECTED -eq 1 ]] && [[ -n "$cmd" ]]; then
        local escaped_cmd="$(_synapse_json_escape "$cmd")"
        local escaped_cwd="$(_synapse_json_escape "$PWD")"
        print -u "$_SYNAPSE_SOCKET_FD" "{\"type\":\"command_executed\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"command\":\"${escaped_cmd}\",\"cwd\":\"${escaped_cwd}\"}" 2>/dev/null
    fi

    _synapse_clear_dropdown
}

# chpwd: runs when the working directory changes
_synapse_chpwd() {
    if [[ $_SYNAPSE_CONNECTED -eq 1 ]]; then
        local escaped_cwd="$(_synapse_json_escape "$PWD")"
        print -u "$_SYNAPSE_SOCKET_FD" "{\"type\":\"cwd_changed\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"cwd\":\"${escaped_cwd}\"}" 2>/dev/null
    fi
}

# --- Cleanup (for dev reload) ---

_synapse_cleanup() {
    _synapse_disconnect
    _synapse_clear_dropdown
    add-zsh-hook -d precmd _synapse_precmd 2>/dev/null
    add-zsh-hook -d preexec _synapse_preexec 2>/dev/null
    add-zsh-hook -d chpwd _synapse_chpwd 2>/dev/null
    bindkey -D synapse-dropdown &>/dev/null
    unset _SYNAPSE_LOADED
}

# --- Setup ---

_synapse_init() {
    _synapse_generate_session_id

    # Register widgets
    zle -N synapse-tab-accept _synapse_tab_accept
    zle -N synapse-dropdown-down _synapse_dropdown_down
    zle -N synapse-dropdown-up _synapse_dropdown_up
    zle -N synapse-dropdown-accept _synapse_dropdown_accept
    zle -N synapse-dropdown-dismiss _synapse_dropdown_dismiss
    zle -N synapse-dropdown-close-and-insert _synapse_dropdown_close_and_insert
    zle -N accept-line _synapse_accept_line

    # Create dropdown keymap from scratch (NOT from main) so we don't
    # inherit accept-line or other widgets that interfere with the modal UI.
    bindkey -D synapse-dropdown &>/dev/null
    bindkey -N synapse-dropdown

    # Any printable character closes dropdown and inserts
    bindkey -M synapse-dropdown -R ' '-'~' synapse-dropdown-close-and-insert
    # Navigation (arrow keys)
    local seq
    for seq in '^[[' '^[O'; do
        bindkey -M synapse-dropdown "${seq}B" synapse-dropdown-down
        bindkey -M synapse-dropdown "${seq}A" synapse-dropdown-up
        bindkey -M synapse-dropdown "${seq}C" synapse-dropdown-accept
    done
    # Accept
    bindkey -M synapse-dropdown '^M' synapse-dropdown-accept     # CR (Enter)
    bindkey -M synapse-dropdown '^J' synapse-dropdown-accept     # NL (Enter)
    bindkey -M synapse-dropdown '\t' synapse-dropdown-accept     # Tab
    # Dismiss
    bindkey -M synapse-dropdown '^[' synapse-dropdown-dismiss    # Escape
    bindkey -M synapse-dropdown '^G' synapse-dropdown-dismiss    # Ctrl-G
    bindkey -M synapse-dropdown '^C' synapse-dropdown-dismiss    # Ctrl-C

    # Main keymap: Tab for NL accept / normal completion
    bindkey '\t' synapse-tab-accept

    # Hooks
    autoload -Uz add-zsh-hook
    add-zsh-hook precmd _synapse_precmd
    add-zsh-hook preexec _synapse_preexec
    add-zsh-hook chpwd _synapse_chpwd

    # Initial connection
    _synapse_ensure_daemon
    _synapse_connect
}

# Initialize
_synapse_init
