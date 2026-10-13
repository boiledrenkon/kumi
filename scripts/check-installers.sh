#!/bin/sh
# Checks the installers in a folder: this checkout's, or copies fetched from where producers get them. install.sh
# has to be POSIX sh, and install.ps1 has to parse and be ASCII. Every check runs, and the exit status says whether
# all passed. Needs shellcheck and pwsh.
#
#   sh scripts/check-installers.sh FOLDER

set -u
folder="$(cd "${1:?usage: sh scripts/check-installers.sh FOLDER}" && pwd)" || exit 1
failed=0
fail() { echo "::error::$*"; failed=1; }

sh -n "$folder/install.sh" || fail "install.sh isn't valid sh."
# A "~/…" shown to producers is meant as text (SC2088).
shellcheck --shell=sh --severity=warning --exclude=SC2088 "$folder/install.sh" || fail "install.sh isn't POSIX sh (shellcheck, above)."
# shellcheck disable=SC2016 # PowerShell's own variables.
KUMI_INSTALLER="$folder/install.ps1" pwsh -NoProfile -NonInteractive -Command '
  $errors = $null
  [void][Management.Automation.Language.Parser]::ParseFile($env:KUMI_INSTALLER, [ref]$null, [ref]$errors)
  foreach ($e in $errors) { "install.ps1:$($e.Extent.StartLineNumber): $($e.Message)" }
  exit [int][bool]$errors' || fail "install.ps1 doesn't parse (above)."
# GitHub serves a release's files with no charset, and Windows PowerShell 5.1's irm reads them as ISO-8859-1 (as -File
# reads a file with no byte order mark in the system's code page): only ASCII reads the same everywhere. Write any other
# character as [char]0x2026 and the like.
if LC_ALL=C grep -n "$(printf '[^\t\r -~]')" "$folder/install.ps1"; then fail "install.ps1 has non-ASCII characters (lines above)."; fi

exit "$failed"
