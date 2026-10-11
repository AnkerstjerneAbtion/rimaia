#!/usr/bin/env bash
#
# Keeps the one command registry and its three readers in agreement
# (seam-contract D32 point 5). Every command is a row in
# crates/core/src/api/registry.rs, `board` or `local`:
#
#   - a board row is answered through `api::dispatch` — in process by the
#     shell's `commands::board::route`, over HTTP by rimaia-server's routes —
#     and is never registered with Tauri;
#   - a local row is answered by the shell's single `generate_handler!` list
#     in src-tauri/src/lib.rs, and by nothing else;
#   - src/lib/commands.ts sends every row through exactly one wrapper, which
#     calls `board<T>` or `local<T>` to match the row's kind.
#
# None of that is generated, so drift compiles, passes clippy and every test,
# and only fails at runtime: a local row missing from the list 404s in the app,
# a board name in the list has two paths, and a wrapper calling the wrong
# function sends a command over the wrong transport once task 049 gives them
# different ones. ADR-0015 accepts no end-to-end tests and names this gap; this
# script stands in for the test that would catch it. It needs no Rust
# toolchain, and is bash 3.2-safe (macOS's /bin/bash) with no jq.
#
# It checks, and fails closed whenever a file no longer has the shape it
# parses:
#   1. the registry: every `board("<name>", Read|Write` and `local("<name>")`
#      row, line comments stripped and matched across the whole file so a row
#      rustfmt wraps cannot hide. A duplicate name, or no rows, fails.
#   2. the handler list: exactly one `generate_handler![` block, equal to the
#      local rows in both directions. `#[cfg(debug_assertions)]` may gate only
#      a `debug_`-prefixed entry.
#   3. the definitions: every `#[tauri::command]` in src-tauri/src/commands/
#      is in the handler list, and one gated on `#[cfg(debug_assertions)]` is
#      `debug_`-prefixed.
#   4. the wrappers: every row has exactly one `board<T>("<name>"` or
#      `local<T>("<name>"` call in commands.ts, of the row's kind; no call
#      names a command the registry lacks; no `call<` remains.
#   5. the protocol version: the `PROTOCOL_VERSION` literal in commands.ts
#      equals `rimaia_core::api::PROTOCOL_VERSION`.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)

registry_rs="$repo_root/crates/core/src/api/registry.rs"
api_mod_rs="$repo_root/crates/core/src/api/mod.rs"
lib_rs="$repo_root/src-tauri/src/lib.rs"
commands_dir="$repo_root/src-tauri/src/commands"
commands_ts="$repo_root/src/lib/commands.ts"

for f in "$registry_rs" "$api_mod_rs" "$lib_rs" "$commands_ts"; do
  if [[ ! -f "$f" ]]; then
    echo "check-command-wiring: expected file not found: $f" >&2
    exit 1
  fi
done
if [[ ! -d "$commands_dir" ]]; then
  echo "check-command-wiring: expected directory not found: $commands_dir" >&2
  exit 1
fi

# The awk programs are written out to real files rather than fed in as
# `<<'HEREDOC'` blocks inside a `$(...)` capture: bash has a long-standing
# parser bug where a heredoc nested inside a command substitution miscounts
# the quotes and parens in its own body (both of which an awk regex is thick
# with). Heredocs as standalone statements are fine.
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

fail=0
complain() {
  echo "check-command-wiring: $*" >&2
  fail=1
}

# `contains NEEDLE HAYSTACK` — HAYSTACK is a newline-separated string, tested
# with a literal, whole-line match. Plain strings rather than bash arrays
# throughout, on purpose: an explicitly empty array under `set -u` is a hard
# error on bash 3.2.
contains() {
  local needle="$1" haystack="$2"
  printf '%s\n' "$haystack" | grep -qxF -- "$needle"
}

count_lines() {
  printf '%s\n' "$1" | grep -c . || true
}

# --- 1. The registry ---------------------------------------------------------
#
# The whole file as one string, with `//` comments removed (a line comment
# names the task that flips a local row, and must not be read as a row), then
# every `board("name", Read|Write` and `local("name")` in order, as
# `board:name` or `local:name`. The quote is built with sprintf for the
# heredoc reason above.
cat >"$workdir/registry_rows.awk" <<'AWK'
BEGIN { q = sprintf("%c", 34) }
{
  line = $0
  sub("//.*$", "", line)
  content = content line "\n"
}
END {
  rest = content
  pattern = "(board|local)\\([ \t\n]*" q "[A-Za-z0-9_]+" q "[ \t\n]*(,[ \t\n]*(Read|Write)|\\))"
  while (match(rest, pattern)) {
    row = substr(rest, RSTART, RLENGTH)
    kind = substr(row, 1, 5)
    name = row
    sub("^(board|local)\\([ \t\n]*" q, "", name)
    sub(q ".*$", "", name)
    if (kind == "board" && row !~ /(Read|Write)$/) {
      print "malformed:" name
    } else if (kind == "local" && row !~ /\)$/) {
      print "malformed:" name
    } else {
      print kind ":" name
    }
    rest = substr(rest, RSTART + RLENGTH)
  }
}
AWK

registry_lines=$(awk -f "$workdir/registry_rows.awk" "$registry_rs")
if [[ -z "$registry_lines" ]]; then
  echo "check-command-wiring: found no board(...) or local(...) rows in $registry_rs — its shape has changed, update the parser in this script" >&2
  exit 1
fi
malformed=$(printf '%s\n' "$registry_lines" | awk -F: '$1 == "malformed" { print $2 }')
while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  complain "'$name' in $registry_rs is neither board(\"$name\", Read|Write, …) nor local(\"$name\") — the parser cannot read it"
done <<<"$malformed"
board_rows=$(printf '%s\n' "$registry_lines" | awk -F: '$1 == "board" { print $2 }')
local_rows=$(printf '%s\n' "$registry_lines" | awk -F: '$1 == "local" { print $2 }')
all_rows=$(printf '%s\n%s\n' "$board_rows" "$local_rows" | grep . || true)

duplicates=$(printf '%s\n' "$all_rows" | sort | uniq -d)
while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  complain "'$name' has more than one row in $registry_rs — a command is one row of one kind"
done <<<"$duplicates"

# --- 2. The handler list -----------------------------------------------------
#
# Exactly one `generate_handler![` block, whose body is one
# `module::path::name,` entry per line, optionally preceded by a
# `#[cfg(debug_assertions)]` line, up to the `]` that closes it. Each entry
# prints as `plain:name` or `gated:name`; a line that is neither prints as
# `unparsed:<line>`, so a changed shape fails rather than passes.
cat >"$workdir/handler_list.awk" <<'AWK'
/generate_handler!\[/ { blocks++; collecting = 1; gated = 0; next }
collecting {
  line = $0
  gsub(/^[ \t]+|[ \t]+$/, "", line)
  if (line == "") next
  if (line ~ /^\]/) { collecting = 0; next }
  if (line ~ /^\/\//) next
  if (line == "#[cfg(debug_assertions)]") { gated = 1; next }
  if (line ~ /^[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*,?$/) {
    sub(/,$/, "", line)
    n = split(line, parts, "::")
    print (gated ? "gated:" : "plain:") parts[n]
    gated = 0
    next
  }
  print "unparsed:" line
}
END { print "blocks:" blocks + 0 }
AWK

handler_lines=$(awk -f "$workdir/handler_list.awk" "$lib_rs")
block_count=$(printf '%s\n' "$handler_lines" | awk -F: '$1 == "blocks" { print $2 }')
if [[ "$block_count" != "1" ]]; then
  echo "check-command-wiring: expected exactly one generate_handler![ block in $lib_rs, found $block_count — the local rows are one list (seam-contract D32 point 4)" >&2
  exit 1
fi
unparsed=$(printf '%s\n' "$handler_lines" | awk '/^unparsed:/ { sub(/^unparsed:/, ""); print }')
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  complain "cannot read '$line' in the generate_handler! list in $lib_rs — one module::path::name entry per line, optionally under #[cfg(debug_assertions)]"
done <<<"$unparsed"
handler_list=$(printf '%s\n' "$handler_lines" | awk -F: '$1 == "plain" || $1 == "gated" { print $2 }')
gated_entries=$(printf '%s\n' "$handler_lines" | awk -F: '$1 == "gated" { print $2 }')
if [[ -z "$handler_list" ]]; then
  echo "check-command-wiring: found no entries in the generate_handler! list in $lib_rs — its shape has changed, update the parser in this script" >&2
  exit 1
fi

while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  case "$name" in
    debug_*) ;;
    *) complain "'$name' is under #[cfg(debug_assertions)] in the generate_handler! list in $lib_rs, but only a debug_-prefixed command may be gated" ;;
  esac
done <<<"$gated_entries"

while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  if contains "$name" "$board_rows"; then
    complain "'$name' is a board row in $registry_rs and must not be in the generate_handler! list in $lib_rs — commands::board::route answers it through api::dispatch, and a command has one path"
  elif ! contains "$name" "$local_rows"; then
    complain "'$name' is in the generate_handler! list in $lib_rs but has no row in $registry_rs — add local(\"$name\") there"
  fi
done <<<"$handler_list"

while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  if ! contains "$name" "$handler_list"; then
    complain "'$name' is a local row in $registry_rs but is missing from the generate_handler! list in $lib_rs — the app would answer it 'command not found'"
  fi
done <<<"$local_rows"

# --- 3. The definitions ------------------------------------------------------
#
# The attributes and doc comments belonging to one `fn` sit on contiguous
# lines directly above it. Among them, `#[tauri::command]` marks a command and
# `#[cfg(debug_assertions)]` marks it debug-only. Each prints as
# `command:name` or `gated:name`.
cat >"$workdir/definitions.awk" <<'AWK'
function reset() { in_cmd = 0; in_cfg = 0 }
FNR == 1 { reset() }
{
  t = $0
  gsub(/^[ \t]+/, "", t)
  if (t ~ /^#\[tauri::command/)            { in_cmd = 1; next }
  if (t ~ /^#\[cfg\(debug_assertions\)\]/) { in_cfg = 1; next }
  if (t ~ /^#\[/ || t ~ /^\/\/\// || t ~ /^\/\/!/) next
  if (in_cmd && t ~ /^pub(\([a-zA-Z_:]+\))?[ \t]+(async[ \t]+)?fn[ \t]+[A-Za-z0-9_]+/) {
    match(t, /fn[ \t]+[A-Za-z0-9_]+/)
    name = substr(t, RSTART + 3, RLENGTH - 3)
    gsub(/^[ \t]+/, "", name)
    print (in_cfg ? "gated:" : "command:") name
  }
  reset()
}
AWK

rs_files=""
while IFS= read -r -d '' f; do
  rs_files="$rs_files$f"$'\n'
done < <(find "$commands_dir" -type f -name '*.rs' -print0)
definition_lines=""
if [[ -n "$rs_files" ]]; then
  # One file per line; no command source path contains a newline.
  definition_lines=$(printf '%s' "$rs_files" | tr '\n' '\0' | xargs -0 awk -f "$workdir/definitions.awk")
fi
if [[ -z "$definition_lines" ]]; then
  echo "check-command-wiring: found no #[tauri::command] definitions under $commands_dir — their shape has changed, update the parser in this script" >&2
  exit 1
fi

while IFS= read -r entry; do
  [[ -z "$entry" ]] && continue
  kind=${entry%%:*}
  name=${entry#*:}
  if [[ "$kind" == "gated" ]]; then
    case "$name" in
      debug_*) ;;
      *) complain "'$name' is #[cfg(debug_assertions)]-gated at its definition but its name doesn't start with 'debug_' — rename it to that convention" ;;
    esac
  fi
  if ! contains "$name" "$handler_list"; then
    complain "'$name' is a #[tauri::command] under $commands_dir but is in no generate_handler! list — classify it with a row in $registry_rs, and register a local row in $lib_rs"
  fi
done <<<"$definition_lines"

# --- 4. The wrappers ---------------------------------------------------------
#
# The whole file as one string, so a call broken across lines still matches:
# every `board<...>("name"` and `local<...>("name"` as `board:name` or
# `local:name`. `[^(]*` for the generic so a nested one such as
# `board<Record<string, unknown>>(` still finds the `>(` that closes it.
cat >"$workdir/wrappers.awk" <<'AWK'
BEGIN { q = sprintf("%c", 39); d = sprintf("%c", 34) }
{ content = content $0 "\n" }
END {
  line = content
  while (match(line, "(board|local)<[^(]*>\\([ \t\n]*[" d q "][A-Za-z0-9_]+[" d q "]")) {
    seg = substr(line, RSTART, RLENGTH)
    kind = substr(seg, 1, 5)
    sub("^(board|local)<[^(]*>\\([ \t\n]*[" d q "]", "", seg)
    sub("[" d q "]$", "", seg)
    print kind ":" seg
    line = substr(line, RSTART + RLENGTH)
  }
}
AWK

if grep -q 'call<' "$commands_ts"; then
  complain "$commands_ts still has a call< — every wrapper calls board<T> or local<T> (seam-contract D32 point 6)"
fi

wrapper_lines=$(awk -f "$workdir/wrappers.awk" "$commands_ts")
if [[ -z "$wrapper_lines" ]]; then
  echo "check-command-wiring: found no board<T>(...) or local<T>(...) calls in $commands_ts — its shape has changed, update the parser in this script" >&2
  exit 1
fi
wrapped=$(printf '%s\n' "$wrapper_lines" | awk -F: '{ print $2 }')

while IFS= read -r entry; do
  [[ -z "$entry" ]] && continue
  kind=${entry%%:*}
  name=${entry#*:}
  if contains "$name" "$board_rows"; then
    [[ "$kind" == "board" ]] || complain "'$name' is a board row in $registry_rs but its wrapper in $commands_ts calls $kind<T> — call board<T>"
  elif contains "$name" "$local_rows"; then
    [[ "$kind" == "local" ]] || complain "'$name' is a local row in $registry_rs but its wrapper in $commands_ts calls $kind<T> — call local<T>"
  else
    complain "'$name' is sent by $commands_ts but has no row in $registry_rs — the backend would answer it 'command not found'"
  fi
done <<<"$wrapper_lines"

twice=$(printf '%s\n' "$wrapped" | sort | uniq -d)
while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  complain "'$name' is sent by more than one wrapper in $commands_ts — each command has exactly one"
done <<<"$twice"

while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  if ! contains "$name" "$wrapped"; then
    complain "'$name' has a row in $registry_rs but no wrapper in $commands_ts — add one that calls board<T> or local<T>"
  fi
done <<<"$all_rows"

# --- 5. The protocol version -------------------------------------------------
rust_version=$(sed -n 's/^pub const PROTOCOL_VERSION: &str = "\([^"]*\)";$/\1/p' "$api_mod_rs")
ts_version=$(sed -n 's/^export const PROTOCOL_VERSION = "\([^"]*\)";$/\1/p' "$commands_ts")
if [[ -z "$rust_version" || $(count_lines "$rust_version") != "1" ]]; then
  echo "check-command-wiring: expected one 'pub const PROTOCOL_VERSION: &str = \"…\";' line in $api_mod_rs — its shape has changed, update the parser in this script" >&2
  exit 1
fi
if [[ -z "$ts_version" || $(count_lines "$ts_version") != "1" ]]; then
  echo "check-command-wiring: expected one 'export const PROTOCOL_VERSION = \"…\";' line in $commands_ts — its shape has changed, update the parser in this script" >&2
  exit 1
fi
if [[ "$rust_version" != "$ts_version" ]]; then
  complain "PROTOCOL_VERSION is \"$ts_version\" in $commands_ts but \"$rust_version\" in $api_mod_rs — they must be equal"
fi

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

echo "check-command-wiring: OK ($(count_lines "$board_rows") board, $(count_lines "$local_rows") local, $(count_lines "$gated_entries") debug-only, protocol $rust_version)"
