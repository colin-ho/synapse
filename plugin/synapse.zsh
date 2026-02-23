#!/usr/bin/env zsh
if [[ -n "$_SYNAPSE_LOADED" ]]; then
    _synapse_cleanup 2>/dev/null
fi
_SYNAPSE_LOADED=1
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
zmodload zsh/zle 2>/dev/null || { return; }
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
_synapse_set_status_message() {
    local text="$1"
    local color="${2:-8}"
    POSTDISPLAY=$'\n'"  ${text}"
    local base_offset=$(( ${#BUFFER} + ${#PREDISPLAY} ))
    region_highlight=("${base_offset} $(( base_offset + ${#POSTDISPLAY} )) fg=${color}")
}
_synapse_nl_execute() {
    local query
    query="$(_synapse_nl_query_from_buffer)"
    if [[ -z "$query" ]]; then
        zle .accept-line
        return
    fi
    _synapse_set_status_message "thinking..." 8
    zle -R
    local bin
    bin="$(_synapse_find_binary)" || { zle .accept-line; return; }
    local -a args=(translate "$query" --cwd "$PWD")
    local cmd; for cmd in "${_SYNAPSE_RECENT_COMMANDS[@]}"; do
        args+=(--recent-command "$cmd")
    done
    local key val; for key in PATH VIRTUAL_ENV; do
        val="${(P)key}"; [[ -n "$val" ]] && args+=(--env-hint "${key}=${val}")
    done
    local response
    response="$(command "$bin" "${args[@]}" 2>/dev/null)" || {
        _synapse_set_status_message "[translation failed]" 1; zle -R; return
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
    _synapse_clear_dropdown
}
_synapse_preexec() {
    local cmd="$1"
    _SYNAPSE_RECENT_COMMANDS=("$cmd" "${_SYNAPSE_RECENT_COMMANDS[@]:0:$(( _SYNAPSE_RECENT_CMD_MAX - 1 ))}")
    _synapse_clear_dropdown
}
_synapse_cleanup() {
    _synapse_clear_dropdown
    add-zsh-hook -d precmd _synapse_precmd 2>/dev/null
    add-zsh-hook -d preexec _synapse_preexec 2>/dev/null
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
    autoload -Uz add-zle-hook-widget 2>/dev/null
    if (( $+functions[add-zle-hook-widget] )); then
        add-zle-hook-widget zle-line-pre-redraw _synapse_pre_redraw
    fi
}
_synapse_init
