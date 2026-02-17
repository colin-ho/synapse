#!/usr/bin/env zsh
# Tests for paste-state behavior in synapse.zsh
# Run: zsh tests/plugin_paste_test.zsh

set -e

PASS=0
FAIL=0

assert_eq() {
    local label="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        PASS=$(( PASS + 1 ))
    else
        FAIL=$(( FAIL + 1 ))
        print -u2 "FAIL: $label"
        print -u2 "  expected: $(printf '%q' "$expected")"
        print -u2 "  actual:   $(printf '%q' "$actual")"
    fi
}

_reset_state() {
    _SYNAPSE_DROPDOWN_OPEN=0
    _SYNAPSE_HISTORY_BROWSING=0
    _SYNAPSE_NL_MODE=0
    _SYNAPSE_NL_ERROR_SHOWN=0
    _SYNAPSE_PASTING=0
    _SYNAPSE_NL_PREFIX="?"
    _SYNAPSE_CURRENT_SUGGESTION=""
    _SYNAPSE_CURRENT_SOURCE=""
    _SYNAPSE_BRACKETED_PASTE_WIDGET="_synapse-orig-bracketed-paste"

    BUFFER=""
    KEYS=""
    POSTDISPLAY=""
    region_highlight=()

    TEST_PASTE_TEXT=""
    SHOW_COUNT=0
    CLEAR_COUNT=0
    SUGGEST_COUNT=0
    NL_SUGGEST_COUNT=0
    DELEGATE_CALL_COUNT=0
    BUILTIN_BRACKETED_PASTE_CALL_COUNT=0
    SIMULATE_MISSING_DELEGATE=0
    LAST_SHOWN_TEXT=""
}

_synapse_report_interaction() { :; }

_synapse_show_suggestion() {
    LAST_SHOWN_TEXT="$1"
    SHOW_COUNT=$(( SHOW_COUNT + 1 ))
}

_synapse_clear_suggestion() {
    POSTDISPLAY=""
    _SYNAPSE_CURRENT_SUGGESTION=""
    _SYNAPSE_CURRENT_SOURCE=""
    region_highlight=()
    CLEAR_COUNT=$(( CLEAR_COUNT + 1 ))
}

_synapse_suggest() {
    SUGGEST_COUNT=$(( SUGGEST_COUNT + 1 ))
}

_synapse_nl_suggest() {
    _SYNAPSE_NL_MODE=1
    NL_SUGGEST_COUNT=$(( NL_SUGGEST_COUNT + 1 ))
}

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

_simulate_bracketed_paste_insert() {
    local i
    for (( i=1; i<=${#TEST_PASTE_TEXT}; i++ )); do
        KEYS="${TEST_PASTE_TEXT[i]}"
        _synapse_self_insert
    done
}

zle() {
    local widget="$1"
    shift

    if [[ "$widget" == ".self-insert" ]]; then
        BUFFER+="$KEYS"
        return 0
    fi

    if [[ "$widget" == "$_SYNAPSE_BRACKETED_PASTE_WIDGET" ]]; then
        DELEGATE_CALL_COUNT=$(( DELEGATE_CALL_COUNT + 1 ))
        if (( SIMULATE_MISSING_DELEGATE )); then
            return 1
        fi
        _simulate_bracketed_paste_insert
        return 0
    fi

    if [[ "$widget" == ".bracketed-paste" ]]; then
        BUILTIN_BRACKETED_PASTE_CALL_COUNT=$(( BUILTIN_BRACKETED_PASTE_CALL_COUNT + 1 ))
        _simulate_bracketed_paste_insert
        return 0
    fi

    return 1
}

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

# --- Tests ---

print "test: async updates are ignored while pasting..."
_reset_state
_SYNAPSE_PASTING=1
typeset rc=0
if _synapse_handle_update $'update\techo one\thistory'; then
    rc=0
else
    rc=$?
fi
assert_eq "ignored update returns 1" "1" "$rc"
assert_eq "ignored update does not render suggestion" "0" "$SHOW_COUNT"

print "test: async updates apply when not pasting..."
_reset_state
if _synapse_handle_update $'update\techo two\thistory'; then
    rc=0
else
    rc=$?
fi
assert_eq "normal update returns 0" "0" "$rc"
assert_eq "normal update renders suggestion once" "1" "$SHOW_COUNT"
assert_eq "normal update sets source" "history" "$_SYNAPSE_CURRENT_SOURCE"
assert_eq "normal update uses text payload" "echo two" "$LAST_SHOWN_TEXT"

print "test: bracketed paste does not suggest for plain shell text..."
_reset_state
TEST_PASTE_TEXT="echo hello"
_synapse_bracketed_paste
assert_eq "delegate called once" "1" "$DELEGATE_CALL_COUNT"
assert_eq "builtin not used when delegate works" "0" "$BUILTIN_BRACKETED_PASTE_CALL_COUNT"
assert_eq "buffer gets full paste text" "echo hello" "$BUFFER"
assert_eq "clear suggestion called before paste" "1" "$CLEAR_COUNT"
assert_eq "suggest not called after paste" "0" "$SUGGEST_COUNT"
assert_eq "nl suggest not called for shell text" "0" "$NL_SUGGEST_COUNT"
assert_eq "pasting flag reset" "0" "$_SYNAPSE_PASTING"

print "test: bracketed paste enters NL flow once..."
_reset_state
TEST_PASTE_TEXT="? list files"
_synapse_bracketed_paste
assert_eq "buffer gets NL paste text" "? list files" "$BUFFER"
assert_eq "normal suggest skipped for NL text" "0" "$SUGGEST_COUNT"
assert_eq "nl suggest called once after paste" "1" "$NL_SUGGEST_COUNT"
assert_eq "nl mode enabled" "1" "$_SYNAPSE_NL_MODE"

print "test: bracketed paste falls back to builtin when delegate is missing..."
_reset_state
TEST_PASTE_TEXT="pwd"
SIMULATE_MISSING_DELEGATE=1
_synapse_bracketed_paste
assert_eq "delegate attempted once" "1" "$DELEGATE_CALL_COUNT"
assert_eq "builtin fallback used once" "1" "$BUILTIN_BRACKETED_PASTE_CALL_COUNT"
assert_eq "fallback still inserts text" "pwd" "$BUFFER"
assert_eq "fallback does not suggest after paste" "0" "$SUGGEST_COUNT"

# --- Summary ---

print ""
if (( FAIL > 0 )); then
    print "FAILED: $PASS passed, $FAIL failed"
    exit 1
else
    print "OK: $PASS passed"
fi
