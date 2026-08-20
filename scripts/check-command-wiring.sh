#!/usr/bin/env bash
#
# Guards against the one failure mode nothing else in CI catches: a Tauri
# command registered in only one of the two `generate_handler!` lists in
# src-tauri/src/lib.rs, or a frontend wrapper in src/lib/commands.ts that
# calls a command name that isn't registered anywhere.
#
# `generate_handler!` needs a literal list, so lib.rs carries two of them —
# one under `#[cfg(debug_assertions)]`, one under `#[cfg(not(debug_assertions))]`
# — and a command meant to ship has to be added to both by hand. Forgetting
# the second one compiles, passes clippy, passes every Rust and Vitest test,
# and works in `npm run tauri dev` (always a debug build); it only 404s in
# the shipped app. ADR-0015 accepts no end-to-end tests and names this exact
# gap as the accepted consequence — this script is what stands in for the
# E2E test that would otherwise catch it.
#
# Two assertions:
#   1. debug list == release list, plus whatever in the debug list is itself
#      #[cfg(debug_assertions)]-gated at its definition site.
#   2. every command name literal passed to call<T>() in commands.ts is
#      registered in at least one of the two lists.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)

lib_rs="$repo_root/src-tauri/src/lib.rs"
commands_dir="$repo_root/src-tauri/src/commands"
commands_ts="$repo_root/src/lib/commands.ts"

for f in "$lib_rs" "$commands_ts"; do
  if [[ ! -f "$f" ]]; then
    echo "check-command-wiring: expected file not found: $f" >&2
    exit 1
  fi
done
if [[ ! -d "$commands_dir" ]]; then
  echo "check-command-wiring: expected directory not found: $commands_dir" >&2
  exit 1
fi

# The three awk programs below are written out to real files rather than fed
# in as `<<'HEREDOC'` blocks on the same line as a `$(...)` capture: bash has
# a long-standing parser bug where a heredoc nested inside a command
# substitution miscounts the quotes and parens in its own body (both of
# which an awk regex is thick with) and never finds the substitution's
# closing `)`. Heredocs as standalone statements are fine; it's specifically
# the nesting that breaks.
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT

# --- 1. Read both generate_handler! lists out of lib.rs --------------------
#
# Assumed shape: exactly one `#[cfg(debug_assertions)]` line and one
# `#[cfg(not(debug_assertions))]` line, each directly followed by a
# `tauri::generate_handler![` call whose body is one `module::path::name,`
# entry per line up to a closing `]);`. If lib.rs stops looking like that,
# `handler_lines` comes back empty and the empty-list check below fails
# closed instead of silently passing.
cat >"$workdir/handler_lists.awk" <<'AWK'
/^[ \t]*#\[cfg\(debug_assertions\)\][ \t]*$/        { pending = "debug"; next }
/^[ \t]*#\[cfg\(not\(debug_assertions\)\)\][ \t]*$/ { pending = "release"; next }
/generate_handler!\[/ {
  if (pending == "") next
  collecting = pending
  pending = ""
  next
}
collecting != "" {
  if ($0 ~ /\]\);/) { collecting = ""; next }
  line = $0
  gsub(/^[ \t]+/, "", line)
  gsub(/,[ \t]*$/, "", line)
  if (line == "") next
  n = split(line, parts, "::")
  print collecting ":" parts[n]
  next
}
AWK

handler_lines=$(awk -f "$workdir/handler_lists.awk" "$lib_rs")
debug_list=$(printf '%s\n' "$handler_lines" | awk -F: '$1 == "debug" { print $2 }')
release_list=$(printf '%s\n' "$handler_lines" | awk -F: '$1 == "release" { print $2 }')

if [[ -z "$debug_list" ]]; then
  echo "check-command-wiring: found no entries under #[cfg(debug_assertions)] in $lib_rs — its shape has changed, update the parser in this script" >&2
  exit 1
fi

# `contains NEEDLE HAYSTACK` — HAYSTACK is a newline-separated string, tested
# with a literal, whole-line match. Plain strings rather than bash arrays
# throughout this script, on purpose: an *explicitly empty* array reference
# under `set -u` is a hard error on the bash 3.2 that ships as /bin/bash on
# macOS, and this script has to run there, not just on CI's bash 5.
contains() {
  local needle="$1" haystack="$2"
  printf '%s\n' "$haystack" | grep -qxF -- "$needle"
}

# --- 2. Find every command definition gated on #[cfg(debug_assertions)] ----
#
# Assumed shape: the attributes and doc comments belonging to one `pub fn`
# sit on contiguous lines directly above it, in either order. Among those
# lines, `#[tauri::command]` marks it as a command; `#[cfg(debug_assertions)]`
# marks it debug-only. A blank line between the attribute block and the `pub
# fn` breaks this — none of the current command definitions have one.
cat >"$workdir/gated_commands.awk" <<'AWK'
function reset() { in_cmd = 0; in_cfg = 0 }
FNR == 1 { reset() }
{
  t = $0
  gsub(/^[ \t]+/, "", t)
  if (t ~ /^#\[tauri::command/)            { in_cmd = 1; next }
  if (t ~ /^#\[cfg\(debug_assertions\)\]/) { in_cfg = 1; next }
  if (t ~ /^#\[/ || t ~ /^\/\/\// || t ~ /^\/\/!/) next
  if (t ~ /^pub(\([a-zA-Z_:]+\))?[ \t]+fn[ \t]+[A-Za-z0-9_]+/) {
    if (in_cmd && in_cfg) {
      match(t, /fn[ \t]+[A-Za-z0-9_]+/)
      name = substr(t, RSTART + 3, RLENGTH - 3)
      gsub(/^[ \t]+/, "", name)
      print name
    }
    reset()
    next
  }
  reset()
}
AWK

rs_files=()
while IFS= read -r -d '' f; do
  rs_files+=("$f")
done < <(find "$commands_dir" -type f -name '*.rs' -print0)

gated_commands=""
if [[ ${#rs_files[@]} -gt 0 ]]; then
  gated_commands=$(awk -f "$workdir/gated_commands.awk" "${rs_files[@]}")
fi

fail=0

# Convention check: a #[cfg(debug_assertions)]-gated command must carry the
# `debug_` name prefix. This is deliberately a second, independent read of
# "is this debug-only" (naming, not gating) so that a debug-only command
# that forgets the prefix fails here loudly instead of quietly falling out
# of the diff below and passing as if it needed no release registration.
while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  case "$name" in
    debug_*) ;;
    *)
      echo "check-command-wiring: '$name' is #[cfg(debug_assertions)]-gated at its definition but its name doesn't start with 'debug_' — rename it to that convention (this script relies on the prefix to trust that a debug-only command was left out of the release list on purpose)" >&2
      fail=1
      ;;
  esac
done <<<"$gated_commands"

# --- 3. release list must equal debug list minus the gated commands --------
while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  if ! contains "$name" "$gated_commands" && ! contains "$name" "$release_list"; then
    echo "check-command-wiring: '$name' is registered in the #[cfg(debug_assertions)] handler list in $lib_rs, is not itself #[cfg(debug_assertions)]-gated, but is missing from the #[cfg(not(debug_assertions))] handler list — it will 404 in a release build. Add it to the release generate_handler! list." >&2
    fail=1
  fi
done <<<"$debug_list"

while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  if ! contains "$name" "$debug_list"; then
    echo "check-command-wiring: '$name' is registered in the #[cfg(not(debug_assertions))] handler list in $lib_rs but not in the #[cfg(debug_assertions)] one — every release command must also be listed in the debug list. Add it there." >&2
    fail=1
  fi
done <<<"$release_list"

# --- 4. every command name literal call<T>() uses must be registered -------
#
# Assumed shape: every backend call goes through the private `call<T>(...)`
# in commands.ts (per that file's own top-of-file comment), and the command
# name is always the first argument, a plain quoted string literal. The
# quote character itself is built with sprintf rather than written literally
# in the awk source, for the same bash heredoc-quote-counting reason as
# above — this file is written by a plain heredoc, so it would be safe here,
# but staying consistent avoids re-discovering the bug the next time this
# gets refactored.
cat >"$workdir/ts_commands.awk" <<'AWK'
BEGIN { q = sprintf("%c", 39) }
# Read the whole file into one string rather than matching line by line: a
# call broken across lines (the command name on its own line, or trailing
# args after it) has no line that contains both `call<...>(` and the quoted
# name, so a per-line match misses it. `[^(]*` (not `[^>]*`) for the generic
# so a nested generic like `call<Record<string, unknown>>(...)` still finds
# the literal `>(` that closes it — a class excluding only `>` stops one `>`
# short whenever the generic itself contains a `>`.
{ content = content $0 "\n" }
END {
  line = content
  while (match(line, "call<[^(]*>\\([ \t\n]*[\"" q "][A-Za-z0-9_]+[\"" q "]")) {
    seg = substr(line, RSTART, RLENGTH)
    sub("^call<[^(]*>\\([ \t\n]*", "", seg)
    gsub("[\"" q "]", "", seg)
    print seg
    line = substr(line, RSTART + RLENGTH)
  }
}
AWK

ts_commands=$(awk -f "$workdir/ts_commands.awk" "$commands_ts")

while IFS= read -r name; do
  [[ -z "$name" ]] && continue
  if ! contains "$name" "$debug_list" && ! contains "$name" "$release_list"; then
    echo "check-command-wiring: '$name' is passed to call<T>() in $commands_ts but is not registered in either generate_handler! list in $lib_rs — the frontend will get a command-not-found error at runtime." >&2
    fail=1
  fi
done <<<"$ts_commands"

if [[ "$fail" -ne 0 ]]; then
  exit 1
fi

debug_count=$(printf '%s\n' "$debug_list" | grep -c . || true)
release_count=$(printf '%s\n' "$release_list" | grep -c . || true)
gated_count=$(printf '%s\n' "$gated_commands" | grep -c . || true)
echo "check-command-wiring: OK ($debug_count debug, $release_count release, $gated_count debug-only)"
