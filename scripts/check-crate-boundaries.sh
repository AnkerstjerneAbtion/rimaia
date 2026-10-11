#!/usr/bin/env bash
#
# The transitive half of ADR-0027 point 6: rimaia-server never links the
# runner or the desktop shell, through any path in the build graph.
#
# Cargo refuses a cycle through `[dependencies]` but not a sibling edge, so
# nothing stops `rimaia-server` gaining `rimaia-runner` directly or through a
# crate that has it. `rimaia_server_does_not_depend_on_rimaia_runner` reads the
# server's own manifest; this asks the resolved graph itself, normal, build and
# dev edges alike, and fails if any line names `rimaia-runner`, the `rimaia`
# shell package or a `tauri` package. It fails closed if the output does not
# contain `rimaia-core`, which every honest graph of the server does.
#
# Run with the pinned toolchain, as CI does. bash 3.2-safe.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)
cd "$repo_root"

tree=$(cargo tree -p rimaia-server -e normal,build,dev --prefix none)

if ! printf '%s\n' "$tree" | grep -q '^rimaia-core '; then
  echo "check-crate-boundaries: 'cargo tree -p rimaia-server' does not list rimaia-core — its output has changed shape, update this script" >&2
  exit 1
fi

forbidden=$(printf '%s\n' "$tree" | grep -E '^(rimaia-runner|rimaia|tauri[A-Za-z0-9_-]*) ' | sort -u || true)
if [[ -n "$forbidden" ]]; then
  echo "check-crate-boundaries: rimaia-server reaches crates it must never link (ADR-0027 point 6):" >&2
  printf '  %s\n' "$forbidden" >&2
  exit 1
fi

echo "check-crate-boundaries: OK (rimaia-server links neither the runner, the shell nor tauri)"
