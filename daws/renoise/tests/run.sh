#!/bin/sh
# Runs the Renoise tool's Lua tests on LuaJIT 2.1, then lua-language-server's check when it's installed.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/kumi-renoise-tests.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
luajit "$here/run.lua" "$tmp" "$@"
if [ "${KUMI_SKIP_LUALS:-}" != 1 ] && command -v lua-language-server >/dev/null 2>&1; then
  "$here/check.sh"
fi
