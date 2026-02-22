#!/usr/bin/env zsh
if [[ -n "$_SYNAPSE_LOADED" ]]; then
    _synapse_cleanup 2>/dev/null
fi
_SYNAPSE_LOADED=1
typeset -g _SYNAPSE_SESSION_ID=""
typeset -g _SYNAPSE_SOCKET_FD=""
typeset -g _SYNAPSE_CONNECTED=0
typeset -g _SYNAPSE_RECONNECT_ATTEMPTS=0
typeset -g _SYNAPSE_MAX_RECONNECT=3
typeset -g _SYNAPSE_LAST_RECONNECT_TIME=0
typeset -gi _SYNAPSE_DISCONNECT_WARNED=0
typeset -gi _SYNAPSE_RECENT_CMD_MAX=10
typeset -ga _SYNAPSE_RECENT_COMMANDS=()
typeset -gi _SYNAPSE_DROPDOWN_INDEX=0
typeset -gi _SYNAPSE_DROPDOWN_COUNT=0
typeset -ga _SYNAPSE_DROPDOWN_ITEMS=()
typeset -ga _SYNAPSE_DROPDOWN_SOURCES=()
typeset -ga _SYNAPSE_DROPDOWN_DESCS=()
typeset -gi _SYNAPSE_DROPDOWN_MAX_VISIBLE=8
typeset -gi _SYNAPSE_DROPDOWN_SCROLL=0
typeset -g _SYNAPSE_NL_PREFIX="?"
zmodload zsh/net/socket 2>/dev/null || { return; }
zmodload zsh/zle 2>/dev/null || { return; }
zmodload zsh/system 2>/dev/null  # for zsystem flock
zmodload zsh/datetime 2>/dev/null  # for EPOCHSECONDS
_synapse_generate_session_id() {
    _SYNAPSE_SESSION_ID="$(head -c 6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
}
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
_synapse_socket_path() {
    if [[ -n "$SYNAPSE_SOCKET" ]]; then
        echo "$SYNAPSE_SOCKET"
    elif [[ -n "$XDG_RUNTIME_DIR" ]]; then
        echo "${XDG_RUNTIME_DIR}/synapse.sock"
    else
        echo "/tmp/synapse-$(id -u).sock"
    fi
}
_synapse_pid_path() {
    local sock="$(_synapse_socket_path)"
    echo "${sock%.sock}.pid"
}
_synapse_lock_path() {
    local sock="$(_synapse_socket_path)"
    echo "${sock%.sock}.lock"
}
_synapse_daemon_running() {
    local pid_file="$(_synapse_pid_path)"
    [[ -f "$pid_file" ]] || return 1
    local pid="$(< "$pid_file")"
    [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}
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
_synapse_buffer_has_nl_prefix() {
    local prefix_len=${#_SYNAPSE_NL_PREFIX}
    (( prefix_len > 0 )) || return 1
    (( ${#BUFFER} >= prefix_len + 2 )) || return 1
    [[ "${BUFFER[1,$prefix_len]}" == "$_SYNAPSE_NL_PREFIX" ]] || return 1
    [[ "${BUFFER[$(( prefix_len + 1 ))]}" == " " ]]
}
_synapse_nl_query_from_buffer() {
    echo "${BUFFER[$(( ${#_SYNAPSE_NL_PREFIX} + 2 )),-1]}"
}
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
_synapse_json_escape() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    value="${value//$'\n'/\\n}"
    value="${value//$'\t'/\\t}"
    echo "$value"
}
_synapse_send_event() {
    local event_type="$1"
    local command="$2"
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || return 0
    [[ -n "$_SYNAPSE_SOCKET_FD" ]] || return 0
    local escaped_cwd="$(_synapse_json_escape "$PWD")"
    if [[ "$event_type" == "command_executed" ]]; then
        [[ -n "$command" ]] || return 0
        local escaped_cmd="$(_synapse_json_escape "$command")"
        print -u "$_SYNAPSE_SOCKET_FD" "{\"type\":\"command_executed\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"command\":\"${escaped_cmd}\",\"cwd\":\"${escaped_cwd}\"}" 2>/dev/null
        return 0
    fi
    if [[ "$event_type" == "cwd_changed" ]]; then
        print -u "$_SYNAPSE_SOCKET_FD" "{\"type\":\"cwd_changed\",\"session_id\":\"${_SYNAPSE_SESSION_ID}\",\"cwd\":\"${escaped_cwd}\"}" 2>/dev/null
    fi
}
_synapse_build_nl_request() {
    local escaped_query="$(_synapse_json_escape "$1")"
    local escaped_cwd="$(_synapse_json_escape "$2")"
    local items=() cmd
    for cmd in "${_SYNAPSE_RECENT_COMMANDS[@]}"; do
        items+=("\"$(_synapse_json_escape "$cmd")\"")
    done
    local recent="[]"
    (( ${#items[@]} )) && recent="[${(j:,:)items}]"
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
_synapse_render_dropdown() {
    if [[ $_SYNAPSE_DROPDOWN_COUNT -eq 0 ]]; then
        POSTDISPLAY=""
        return
    fi
    local max_vis=$_SYNAPSE_DROPDOWN_MAX_VISIBLE
    if (( max_vis > LINES - 3 )); then
        max_vis=$(( LINES - 3 ))
    fi
    (( max_vis < 1 )) && max_vis=1
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
    local src="${_SYNAPSE_DROPDOWN_SOURCES[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    display+=$'\n'"  [${src:-?}] $(( _SYNAPSE_DROPDOWN_INDEX + 1 ))/${_SYNAPSE_DROPDOWN_COUNT}"
    POSTDISPLAY="$display"
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
    POSTDISPLAY=""
    region_highlight=()
}
_synapse_dropdown_exit() {
    _synapse_clear_dropdown
    zle -K main
    unset _ZSH_AUTOSUGGEST_DISABLED 2>/dev/null
    (( $+functions[_zsh_autosuggest_enable] )) && _zsh_autosuggest_enable
    zle reset-prompt
}
_synapse_pre_redraw() {
    (( _SYNAPSE_DROPDOWN_COUNT > 0 )) && _synapse_render_dropdown
}
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
_synapse_drain_stale() {
    [[ $_SYNAPSE_CONNECTED -eq 1 ]] || return
    [[ -n "$_SYNAPSE_SOCKET_FD" ]] || return
    local _junk
    while read -t 0 -u "$_SYNAPSE_SOCKET_FD" _junk 2>/dev/null; do :; done
}
_synapse_set_status_message() {
    local text="$1"
    local color="${2:-8}"
    POSTDISPLAY=$'\n'"  ${text}"
    local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
    region_highlight=("${base_offset} $(( base_offset + ${#POSTDISPLAY} )) fg=${color}")
}
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
    typeset -g _ZSH_AUTOSUGGEST_DISABLED=1
    (( $+functions[_zsh_autosuggest_disable] )) && _zsh_autosuggest_disable
    _synapse_render_dropdown
    zle -R
    zle -K synapse-dropdown
}
_synapse_accept_line() {
    POSTDISPLAY=""
    region_highlight=()
    if _synapse_buffer_has_nl_prefix; then
        _synapse_nl_execute
    else
        zle .accept-line
    fi
}
_synapse_tab_accept() {
    if _synapse_buffer_has_nl_prefix; then
        _synapse_nl_execute
    else
        zle expand-or-complete
    fi
}
_synapse_dropdown_down() {
    _synapse_dropdown_move 1
}
_synapse_dropdown_up() {
    _synapse_dropdown_move -1
}
_synapse_dropdown_move() {
    local delta="$1"
    (( _SYNAPSE_DROPDOWN_INDEX += delta ))
    if (( _SYNAPSE_DROPDOWN_INDEX < 0 )); then
        _SYNAPSE_DROPDOWN_INDEX=$(( _SYNAPSE_DROPDOWN_COUNT - 1 ))
    elif (( _SYNAPSE_DROPDOWN_INDEX >= _SYNAPSE_DROPDOWN_COUNT )); then
        _SYNAPSE_DROPDOWN_INDEX=0
    fi
    _synapse_render_dropdown
    zle -R
}
_synapse_dropdown_accept() {
    BUFFER="${_SYNAPSE_DROPDOWN_ITEMS[$(( _SYNAPSE_DROPDOWN_INDEX + 1 ))]}"
    CURSOR=${#BUFFER}
    _synapse_dropdown_exit
}
_synapse_dropdown_dismiss() {
    _synapse_dropdown_exit
}
_synapse_dropdown_close_and_insert() {
    LBUFFER+="${KEYS}"
    _synapse_dropdown_exit
}
_synapse_precmd() {
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
_synapse_preexec() {
    local cmd="$1"
    _SYNAPSE_RECENT_COMMANDS=("$cmd" "${_SYNAPSE_RECENT_COMMANDS[@]:0:$(( _SYNAPSE_RECENT_CMD_MAX - 1 ))}")
    _synapse_send_event "command_executed" "$cmd"
    _synapse_clear_dropdown
}
_synapse_chpwd() {
    _synapse_send_event "cwd_changed"
}
_synapse_cleanup() {
    _synapse_disconnect
    _synapse_clear_dropdown
    add-zsh-hook -d precmd _synapse_precmd 2>/dev/null
    add-zsh-hook -d preexec _synapse_preexec 2>/dev/null
    add-zsh-hook -d chpwd _synapse_chpwd 2>/dev/null
    (( $+functions[add-zle-hook-widget] )) && add-zle-hook-widget -d zle-line-pre-redraw _synapse_pre_redraw 2>/dev/null
    zle -A .accept-line accept-line 2>/dev/null
    bindkey -D synapse-dropdown &>/dev/null
    bindkey '^M' accept-line 2>/dev/null
    bindkey '^J' accept-line 2>/dev/null
    unset _SYNAPSE_LOADED
}
synapse() {
    local bin="${SYNAPSE_BIN:-synapse}"
    if [[ "$1" == "add" ]]; then
        command "$bin" "$@" || return $?
        shift
        local cmd=""
        while [[ $# -gt 0 ]]; do
            case "$1" in
                --output-dir) shift ;;
                --*) ;;
                *) cmd="$1"; break ;;
            esac
            shift
        done
        [[ -n "$cmd" ]] && _synapse_register_completion "_${cmd}" "${cmd}"
    elif [[ "$1" == "scan" ]]; then
        command "$bin" "$@" || return $?
        local comp_dir="${HOME}/.synapse/completions"
        if [[ -d "$comp_dir" ]]; then
            local f
            for f in "$comp_dir"/_*(N); do
                local func="${f:t}"
                local cmd="${func#_}"
                _synapse_register_completion "$func" "$cmd"
            done
        fi
    else
        command "$bin" "$@"
    fi
}
_synapse_register_completion() {
    local func="$1"
    local cmd="$2"
    (( $+functions[compdef] )) || return 0
    autoload -Uz "$func"
    compdef "$func" "$cmd"
}
_synapse_init() {
    _synapse_generate_session_id
    zle -N synapse-tab-accept _synapse_tab_accept
    zle -N synapse-dropdown-down _synapse_dropdown_down
    zle -N synapse-dropdown-up _synapse_dropdown_up
    zle -N synapse-dropdown-accept _synapse_dropdown_accept
    zle -N synapse-dropdown-dismiss _synapse_dropdown_dismiss
    zle -N synapse-dropdown-close-and-insert _synapse_dropdown_close_and_insert
    zle -N synapse-accept-line _synapse_accept_line
    bindkey '^M' synapse-accept-line
    bindkey '^J' synapse-accept-line
    bindkey -D synapse-dropdown &>/dev/null
    bindkey -N synapse-dropdown
    bindkey -M synapse-dropdown -R ' '-'~' synapse-dropdown-close-and-insert
    local seq
    for seq in '^[[' '^[O'; do
        bindkey -M synapse-dropdown "${seq}B" synapse-dropdown-down
        bindkey -M synapse-dropdown "${seq}A" synapse-dropdown-up
        bindkey -M synapse-dropdown "${seq}C" synapse-dropdown-accept
    done
    bindkey -M synapse-dropdown '^M' synapse-dropdown-accept     # CR (Enter)
    bindkey -M synapse-dropdown '\t' synapse-dropdown-accept     # Tab
    bindkey -M synapse-dropdown '^[' synapse-dropdown-dismiss    # Escape
    bindkey -M synapse-dropdown '^G' synapse-dropdown-dismiss    # Ctrl-G
    bindkey -M synapse-dropdown '^C' synapse-dropdown-dismiss    # Ctrl-C
    bindkey '\t' synapse-tab-accept
    autoload -Uz add-zsh-hook
    add-zsh-hook precmd _synapse_precmd
    add-zsh-hook preexec _synapse_preexec
    add-zsh-hook chpwd _synapse_chpwd
    autoload -Uz add-zle-hook-widget 2>/dev/null
    if (( $+functions[add-zle-hook-widget] )); then
        add-zle-hook-widget zle-line-pre-redraw _synapse_pre_redraw
    fi
    _synapse_ensure_daemon
    _synapse_connect
}
_synapse_init
