#!/usr/bin/env zsh
# Tests for the TSV parsing logic in synapse.zsh
# Run: zsh tests/plugin_parse_test.zsh

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

# --- Extract the parsing function from the plugin ---

# We redefine it here to test in isolation (no zle dependency).
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

# --- Test: empty descriptions don't misalign fields ---

print "test: empty descriptions preserve field alignment..."

# Simulates: 3 history items with no description (empty field between tabs)
local tsv="list	3	daft-sync	history		history	daft-sync stop	history		history	daft-sync watch	history		history"

_synapse_parse_suggestion_list "$tsv"

assert_eq "count" "3" "$_SYNAPSE_DROPDOWN_COUNT"

assert_eq "item[1] text"   "daft-sync"       "${_SYNAPSE_DROPDOWN_ITEMS[1]}"
assert_eq "item[1] source" "history"          "${_SYNAPSE_DROPDOWN_SOURCES[1]}"
assert_eq "item[1] desc"   ""                 "${_SYNAPSE_DROPDOWN_DESCS[1]}"
assert_eq "item[1] kind"   "history"          "${_SYNAPSE_DROPDOWN_KINDS[1]}"

assert_eq "item[2] text"   "daft-sync stop"   "${_SYNAPSE_DROPDOWN_ITEMS[2]}"
assert_eq "item[2] source" "history"           "${_SYNAPSE_DROPDOWN_SOURCES[2]}"
assert_eq "item[2] desc"   ""                  "${_SYNAPSE_DROPDOWN_DESCS[2]}"
assert_eq "item[2] kind"   "history"           "${_SYNAPSE_DROPDOWN_KINDS[2]}"

assert_eq "item[3] text"   "daft-sync watch"  "${_SYNAPSE_DROPDOWN_ITEMS[3]}"
assert_eq "item[3] source" "history"           "${_SYNAPSE_DROPDOWN_SOURCES[3]}"
assert_eq "item[3] desc"   ""                  "${_SYNAPSE_DROPDOWN_DESCS[3]}"
assert_eq "item[3] kind"   "history"           "${_SYNAPSE_DROPDOWN_KINDS[3]}"

# --- Test: items with descriptions parse correctly ---

print "test: items with descriptions parse correctly..."

local tsv2="list	2	git status	history		command	git stash	spec	Stash changes	subcommand"

_synapse_parse_suggestion_list "$tsv2"

assert_eq "count" "2" "$_SYNAPSE_DROPDOWN_COUNT"

assert_eq "item[1] text"   "git status" "${_SYNAPSE_DROPDOWN_ITEMS[1]}"
assert_eq "item[1] source" "history"    "${_SYNAPSE_DROPDOWN_SOURCES[1]}"
assert_eq "item[1] desc"   ""           "${_SYNAPSE_DROPDOWN_DESCS[1]}"
assert_eq "item[1] kind"   "command"    "${_SYNAPSE_DROPDOWN_KINDS[1]}"

assert_eq "item[2] text"   "git stash"      "${_SYNAPSE_DROPDOWN_ITEMS[2]}"
assert_eq "item[2] source" "spec"            "${_SYNAPSE_DROPDOWN_SOURCES[2]}"
assert_eq "item[2] desc"   "Stash changes"  "${_SYNAPSE_DROPDOWN_DESCS[2]}"
assert_eq "item[2] kind"   "subcommand"     "${_SYNAPSE_DROPDOWN_KINDS[2]}"

# --- Test: mixed empty and non-empty descriptions ---

print "test: mixed empty and non-empty descriptions..."

local tsv3="list	3	cargo build	history		history	cargo test	spec	Run tests	command	cargo fmt	history		history"

_synapse_parse_suggestion_list "$tsv3"

assert_eq "count" "3" "$_SYNAPSE_DROPDOWN_COUNT"

assert_eq "item[1] text"   "cargo build" "${_SYNAPSE_DROPDOWN_ITEMS[1]}"
assert_eq "item[1] desc"   ""            "${_SYNAPSE_DROPDOWN_DESCS[1]}"

assert_eq "item[2] text"   "cargo test"  "${_SYNAPSE_DROPDOWN_ITEMS[2]}"
assert_eq "item[2] desc"   "Run tests"   "${_SYNAPSE_DROPDOWN_DESCS[2]}"

assert_eq "item[3] text"   "cargo fmt"   "${_SYNAPSE_DROPDOWN_ITEMS[3]}"
assert_eq "item[3] desc"   ""            "${_SYNAPSE_DROPDOWN_DESCS[3]}"

# --- Summary ---

print ""
if (( FAIL > 0 )); then
    print "FAILED: $PASS passed, $FAIL failed"
    exit 1
else
    print "OK: $PASS passed"
fi
