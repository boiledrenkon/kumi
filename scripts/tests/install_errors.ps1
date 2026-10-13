# install.ps1 at the edges, in Windows PowerShell 5.1: what it says when a release isn't there, nothing answers, a
# Wi-Fi sign-in page answers, a checksum doesn't match or the new Kumi doesn't start; that a failure is the exit code
# of a run from a file and leaves an iex window open; that a repair keeps the Kumi to roll back to; and what the kumi
# command does without Kumi's app. Releases come from a local server, and each run has a folder of its own, with
# PATH left alone. The Installer workflow runs it:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/tests/install_errors.ps1
param([string]$Installer = (Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'install.ps1'))
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2

$failures = 0
function Check([string]$What, [bool]$Ok, [string]$Said = '') {
  if ($Ok) { Write-Host "ok    $What" }
  else { Write-Host "FAIL  $What"; if ($Said) { Write-Host $Said }; $script:failures++ }
}

$root = Join-Path ([IO.Path]::GetTempPath()) ('kumi-install-errors-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$releases = Join-Path $root 'releases'
New-Item -ItemType Directory -Force -Path $releases | Out-Null
$cpu = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$target = $(if ($cpu -eq 'ARM64') { 'aarch64' } else { 'x86_64' }) + '-pc-windows-msvc'
$tar = Join-Path $env:SystemRoot 'System32\tar.exe'

# A Kumi that says its version, and otherwise where it runs from.
$fixture = Join-Path $root 'kumi.exe'
Add-Type -OutputAssembly $fixture -OutputType ConsoleApplication -TypeDefinition @'
public static class Program {
  public static int Main(string[] args) {
    if (args.Length == 1 && args[0] == "--version") { System.Console.WriteLine("Kumi fixture"); return 0; }
    System.Console.WriteLine("fixture in " + System.AppDomain.CurrentDomain.BaseDirectory);
    return 0;
  }
}
'@
# One that doesn't start: where.exe says on its error output that it doesn't know --version.
$broken = Join-Path $env:SystemRoot 'System32\where.exe'

$port = 8791
$base = "http://127.0.0.1:$port"
$server = Start-Process -FilePath python -ArgumentList @('-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', "`"$releases`"") -PassThru -WindowStyle Hidden
for ($attempt = 1; $attempt -le 30; $attempt++) {
  try { Invoke-WebRequest -Uri "$base/" -UseBasicParsing -TimeoutSec 2 | Out-Null; break } catch { Start-Sleep -Seconds 1 }
}

# Kumi $Version as a release serves it, in releases\$Name: its address.
function Release([string]$Name, [string]$Version, [string]$Exe = $fixture, [string]$Sha = '') {
  $folder = Join-Path $releases $Name
  $stage = Join-Path $root "stage\$Name"
  New-Item -ItemType Directory -Force -Path $folder, $stage | Out-Null
  Copy-Item -LiteralPath $Exe -Destination (Join-Path $stage 'kumi.exe')
  [IO.File]::WriteAllText((Join-Path $stage 'package.json'), "{`"name`": `"kumi`", `"version`": `"$Version`"}")
  $bundle = "kumi-$target.tar.gz"
  & $tar -czf (Join-Path $folder $bundle) -C $stage kumi.exe package.json
  if ($LASTEXITCODE -ne 0) { throw 'tar failed' }
  if (-not $Sha) { $Sha = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $folder $bundle)).Hash.ToLowerInvariant() }
  [IO.File]::WriteAllText((Join-Path $folder "kumi-release-$target.json"),
    "{`"kumi`": `"$Version`", `"bundle`": `"$bundle`", `"sha256`": `"$Sha`", `"runtime`": `"rust-native`", `"target`": `"$target`"}")
  [IO.File]::WriteAllText((Join-Path $folder 'kumi-release.json'), "{`"kumi`": `"$Version`"}")
  "$base/$Name"
}
function Version([string]$Folder) {
  $file = Join-Path $Folder 'package.json'
  if (Test-Path -LiteralPath $file) { (Get-Content -Raw -LiteralPath $file | ConvertFrom-Json).version } else { '' }
}
# A program's output and error output, line by line, and its exit code.
function Run([scriptblock]$Command) {
  $preference = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
  try { $said = (& $Command 2>&1 | ForEach-Object { "$_" }) -join "`n"; @{ Said = $said; Code = $LASTEXITCODE } }
  finally { $ErrorActionPreference = $preference }
}
# install.ps1 in a powershell of its own, from the file (or piped into iex), with these settings, PATH left alone.
function Install([hashtable]$Settings, [switch]$Iex) {
  $names = @('KUMI_HOME', 'KUMI_RELEASES', 'KUMI_VERSION', 'KUMI_NO_MODIFY_PATH', 'KUMI_NO_LAUNCH', 'HTTPS_PROXY')
  $saved = @{}; foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }
  try {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $null) }
    $env:KUMI_NO_MODIFY_PATH = '1'; $env:KUMI_NO_LAUNCH = '1'
    foreach ($name in $Settings.Keys) { [Environment]::SetEnvironmentVariable($name, $Settings[$name]) }
    if ($Iex) {
      $piped = "Get-Content -Raw -LiteralPath '$Installer' | Invoke-Expression; Write-Host ('still here ' + `$LASTEXITCODE)"
      Run { powershell -NoProfile -ExecutionPolicy Bypass -Command $piped }
    } else {
      Run { powershell -NoProfile -ExecutionPolicy Bypass -File $Installer }
    }
  } finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
  }
}
function Home([string]$Name) { Join-Path $root "homes\$Name\.kumi" }

try {
  # -- What goes wrong ------------------------------------------------------
  New-Item -ItemType Directory -Force -Path (Join-Path $releases 'empty') | Out-Null
  $kumi = Home 'missing'
  $run = Install @{ KUMI_HOME = $kumi; KUMI_RELEASES = "$base/empty" }
  Check 'a missing release is named, and is the exit code of a run from a file' (($run.Code -eq 1) -and $run.Said.Contains("there's no Kumi release for this computer ($target) at $base/empty.")) $run.Said
  Check '... and the folder it made goes' (-not (Test-Path -LiteralPath $kumi))
  $run = Install @{ KUMI_HOME = (Home 'iex'); KUMI_RELEASES = "$base/empty" } -Iex
  Check 'piped into iex, a failure leaves the window open with $LASTEXITCODE 1' (($run.Said -match 'still here 1') -and $run.Said.Contains("there's no Kumi release")) $run.Said

  $run = Install @{ KUMI_HOME = (Home 'github'); KUMI_VERSION = '9.9.9' }
  Check "a version GitHub hasn't got is named" (($run.Code -eq 1) -and $run.Said.Contains("there's no Kumi 9.9.9. The releases are at https://github.com/user1303836/kumi/releases")) $run.Said

  $run = Install @{ KUMI_HOME = (Home 'offline'); KUMI_RELEASES = 'http://127.0.0.1:9' }
  Check 'nothing answering says the connection failed, and what Windows said' (($run.Code -eq 1) -and $run.Said.Contains("couldn't reach 127.0.0.1:9: the connection failed.") -and ($run.Said -match 'remote server|actively refused')) $run.Said
  $run = Install @{ KUMI_HOME = (Home 'proxy'); KUMI_RELEASES = 'http://127.0.0.1:9'; HTTPS_PROXY = 'someone:sec@ret@127.0.0.1:9' }
  Check "the proxy settings say no user name or password, with no scheme before them" ($run.Said.Contains('Proxy settings: HTTPS_PROXY=127.0.0.1:9.') -and -not $run.Said.Contains('someone') -and -not $run.Said.Contains('ret@')) $run.Said

  New-Item -ItemType Directory -Force -Path (Join-Path $releases 'page') | Out-Null
  [IO.File]::WriteAllText((Join-Path $releases "page\kumi-release-$target.json"), '<!DOCTYPE html><html><head><title>Sign in</title></head><body>Wi-Fi</body></html>')
  $run = Install @{ KUMI_HOME = (Home 'page'); KUMI_RELEASES = "$base/page" }
  Check 'a Wi-Fi sign-in page in place of the release is named' (($run.Code -eq 1) -and $run.Said.Contains('a Wi-Fi sign-in page or proxy answered instead of GitHub')) $run.Said

  $run = Install @{ KUMI_HOME = (Home 'version'); KUMI_VERSION = '1.13' }
  Check "a KUMI_VERSION that isn't a release says what it takes" (($run.Code -eq 1) -and $run.Said.Contains('KUMI_VERSION is "1.13"; it takes a release such as 1.16.0, or latest.')) $run.Said
  $run = Install @{ KUMI_HOME = (Home 'version'); KUMI_VERSION = 'v1.7.5' }
  Check "one before native Kumi gives that release's own script" (($run.Code -eq 1) -and $run.Said.Contains('came before native Kumi (1.7.6)') -and $run.Said.Contains('irm https://raw.githubusercontent.com/user1303836/kumi/v1.7.5/install.ps1 | iex')) $run.Said

  # -- The Kumi that was there stays ----------------------------------------
  $kumi = Home 'kept'
  $run = Install @{ KUMI_HOME = $kumi; KUMI_RELEASES = (Release 'one' '1.0.0') }
  Check 'a fresh install from a file exits 0' (($run.Code -eq 0) -and ((Version (Join-Path $kumi 'app')) -eq '1.0.0')) $run.Said
  $run = Install @{ KUMI_HOME = $kumi; KUMI_RELEASES = (Release 'sum' '2.0.0' -Sha ('0' * 64)) }
  Check "a checksum that doesn't match changes nothing" (($run.Code -eq 1) -and $run.Said.Contains("didn't match its checksum") -and ((Version (Join-Path $kumi 'app')) -eq '1.0.0')) $run.Said
  $run = Install @{ KUMI_HOME = $kumi; KUMI_RELEASES = (Release 'broken' '2.0.0' -Exe $broken) }
  Check "a Kumi that doesn't start says why, and changes nothing" (($run.Code -eq 1) -and ($run.Said -match "didn't start \(.+\)") -and ((Version (Join-Path $kumi 'app')) -eq '1.0.0')) $run.Said

  # -- Updates and repairs --------------------------------------------------
  $run = Install @{ KUMI_HOME = $kumi; KUMI_RELEASES = (Release 'two' '2.0.0') }
  Check 'an update says from what to what' (($run.Code -eq 0) -and ($run.Said -match '1\.0\.0 \S+ 2\.0\.0') -and ((Version (Join-Path $kumi 'app.previous')) -eq '1.0.0')) $run.Said
  $run = Install @{ KUMI_HOME = $kumi; KUMI_RELEASES = "$base/two" }
  $kept = ((Version (Join-Path $kumi 'app')) -eq '2.0.0') -and ((Version (Join-Path $kumi 'app.previous')) -eq '1.0.0') -and -not (Test-Path -LiteralPath (Join-Path $kumi 'app.replaced'))
  Check 'a repair keeps the Kumi to roll back to' (($run.Code -eq 0) -and $run.Said.Contains('reinstalled 2.0.0') -and $kept) $run.Said
  $run = Install @{ KUMI_HOME = $kumi; KUMI_RELEASES = "$base/one" }
  Check 'an older one says so' (($run.Code -eq 0) -and $run.Said.Contains('replaced 2.0.0 with older 1.0.0')) $run.Said
  Check 'PATH left alone says how to start Kumi in a new window' ($run.Said.Contains('In a new window, add') -or $run.Said.Contains('Next (in this window): kumi')) $run.Said

  # -- The kumi command without Kumi's app ----------------------------------
  $cmd = Join-Path $kumi 'bin\kumi.cmd'
  Remove-Item -LiteralPath (Join-Path $kumi 'app') -Recurse -Force
  $run = Run { & $cmd }
  Check 'without its app, the kumi command starts the Kumi before it and says so' (($run.Code -eq 0) -and ($run.Said -match 'fixture in .*app\.previous') -and $run.Said.Contains("Kumi's app is missing, so the Kumi before it starts.")) $run.Said
  Remove-Item -LiteralPath (Join-Path $kumi 'app.previous') -Recurse -Force
  $run = Run { & $cmd --version }
  Check 'with neither, it says how to repair Kumi' (($run.Code -eq 1) -and $run.Said.Contains("Kumi's app is missing: run the installer again: irm https://raw.githubusercontent.com/user1303836/kumi/main/install.ps1 | iex")) $run.Said
} finally {
  if ($server) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
  Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures) { throw "$failures check(s) failed" }
