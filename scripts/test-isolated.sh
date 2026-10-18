#!/bin/sh
# Runs `cargo test` in a home of its own: HOME, USERPROFILE, APPDATA, LOCALAPPDATA, XDG_CONFIG_HOME and
# KUMI_HOME point into a fresh temporary folder, so no test can reach this machine's Live folders,
# Remote Scripts or ~/.kumi through a default it forgot to override (home_dir(), kumi_dir() and the like).
# Live's folders set in this shell (KUMI_REMOTE_SCRIPTS_DIR, KUMI_LIVE_EXTENSIONS_DIR) aren't passed on.
# Cargo and rustup keep their own folders, named before HOME moves. The arguments are `cargo test`'s, or
# `cargo nextest run`'s with KUMI_NEXTEST=1; KUMI_PACKAGES narrows the run to those packages.
set -eu
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
test_home="$(mktemp -d "${TMPDIR:-/tmp}/kumi-test-home-XXXXXX")"
trap 'rm -rf "$test_home"' EXIT INT TERM
export HOME="$test_home" USERPROFILE="$test_home" APPDATA="$test_home/AppData/Roaming" LOCALAPPDATA="$test_home/AppData/Local" XDG_CONFIG_HOME="$test_home/.config" KUMI_HOME="$test_home/.kumi"
unset KUMI_REMOTE_SCRIPTS_DIR KUMI_LIVE_EXTENSIONS_DIR
# The whole workspace, or only the packages named in KUMI_PACKAGES (space separated), with every test of theirs.
scope="--workspace"
if [ -n "${KUMI_PACKAGES:-}" ]; then
  scope=""
  for package in $KUMI_PACKAGES; do scope="$scope -p $package"; done
fi
if [ "${KUMI_NEXTEST:-}" = 1 ]; then
  # shellcheck disable=SC2086
  cargo nextest run $scope --locked "$@"
else
  # shellcheck disable=SC2086
  cargo test $scope --locked "$@"
fi
