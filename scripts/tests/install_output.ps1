# What install.ps1 prints, byte for byte, against what 1.16.1's install.ps1 printed (the last one with its glyphs
# as text, before the file went ASCII), with the console writing UTF-8: the "...", "." and arrow it builds from
# [char]s are the bytes that file printed. Both install a stand-in Kumi from a local server, then update it; the
# update's Install line also says what changed, which is new. The lines that show only in a console (a step's
# "...", and "Starting Kumi...") are compared function by function. The Installer workflow runs it in Windows
# PowerShell 5.1 and in PowerShell 7, each starting its own kind:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/tests/install_output.ps1 -Reference <1.16.1's install.ps1>
param([string]$Reference, [string]$Installer)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2
# Here, not as the parameters' defaults: Windows PowerShell has no $PSScriptRoot there for a script like this.
if (-not $Reference) { throw 'Say which install.ps1 to compare with: -Reference <1.16.1''s install.ps1>' }
if (-not $Installer) { $Installer = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'install.ps1' }
$Reference = (Resolve-Path -LiteralPath $Reference).Path

$failures = 0
function Check([string]$What, [bool]$Ok, [string]$Said = '') {
  if ($Ok) { Write-Host "ok    $What" }
  else { Write-Host "FAIL  $What"; if ($Said) { Write-Host $Said }; $script:failures++ }
}

$ellipsis = [string][char]0x2026; $dot = [string][char]0x00B7; $arrow = [string][char]0x2192
$utf8 = New-Object Text.UTF8Encoding $false
# Bytes as text, a character a byte: text compares byte for byte.
$bytes = [Text.Encoding]::GetEncoding(28591)
function Printable([string]$Text) { $utf8.GetString($bytes.GetBytes($Text)) }

$root = Join-Path ([IO.Path]::GetTempPath()) ('kumi-install-output-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$releases = Join-Path $root 'releases'
New-Item -ItemType Directory -Force -Path $releases | Out-Null
$cpu = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$target = $(if ($cpu -eq 'ARM64') { 'aarch64' } else { 'x86_64' }) + '-pc-windows-msvc'
$tar = Join-Path $env:SystemRoot 'System32\tar.exe'
$shell = (Get-Process -Id $PID).Path

# A stand-in Kumi, built with the .NET Framework's compiler (Windows PowerShell's and PowerShell 7's alike).
$fixture = Join-Path $root 'kumi.exe'
$source = Join-Path $root 'kumi.cs'
[IO.File]::WriteAllText($source, 'public static class Program { public static int Main() { System.Console.WriteLine("Kumi fixture"); return 0; } }')
& (Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe') /nologo /target:exe "/out:$fixture" $source | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'the stand-in Kumi did not build' }

# The user PATH as the registry keeps it, put back at the end: both installers add their homes' bin to it.
$environment = (Get-Item -LiteralPath 'HKCU:\').OpenSubKey('Environment', $true)
$userPath = $environment.GetValue('Path', $null, 'DoNotExpandEnvironmentNames')
$userPathKind = if ($null -ne $userPath) { $environment.GetValueKind('Path') } else { $null }

$port = 8792
$base = "http://127.0.0.1:$port"
$server = Start-Process -FilePath python -ArgumentList @('-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', "`"$releases`"") -PassThru -WindowStyle Hidden
for ($attempt = 1; $attempt -le 30; $attempt++) {
  try { Invoke-WebRequest -Uri "$base/" -UseBasicParsing -TimeoutSec 2 | Out-Null; break } catch { Start-Sleep -Seconds 1 }
}

# Kumi $Version as a release serves it, in releases\$Name: its address.
function Release([string]$Name, [string]$Version) {
  $folder = Join-Path $releases $Name
  $stage = Join-Path $root "stage\$Name"
  New-Item -ItemType Directory -Force -Path $folder, $stage | Out-Null
  Copy-Item -LiteralPath $fixture -Destination (Join-Path $stage 'kumi.exe')
  [IO.File]::WriteAllText((Join-Path $stage 'package.json'), "{`"name`": `"kumi`", `"version`": `"$Version`"}")
  $bundle = "kumi-$target.tar.gz"
  & $tar -czf (Join-Path $folder $bundle) -C $stage kumi.exe package.json
  if ($LASTEXITCODE -ne 0) { throw 'tar failed' }
  $sha = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $folder $bundle)).Hash.ToLowerInvariant()
  [IO.File]::WriteAllText((Join-Path $folder "kumi-release-$target.json"),
    "{`"kumi`": `"$Version`", `"bundle`": `"$bundle`", `"sha256`": `"$sha`", `"runtime`": `"rust-native`", `"target`": `"$target`"}")
  "$base/$Name"
}
# A Kumi folder of its own, as a whole path.
function Home([string]$Name) { (New-Item -ItemType Directory -Force -Path (Join-Path $root "homes\$Name\.kumi")).FullName }
function Quoted([string]$Text) { "'" + ($Text -replace "'", "''") + "'" }
# What the installer at $Script prints, as bytes, in a powershell of this kind whose console writes UTF-8: read as
# raw.githubusercontent.com serves it (utf8), as Windows PowerShell reads a body with no charset (latin1), or run
# from the file. Kumi's folder reads as <home>.
function Printed([string]$Script, [string]$How, [string]$KumiHome, [string]$From) {
  $run = switch ($How) {
    'utf8' { "Get-Content -Raw -Encoding UTF8 -LiteralPath $(Quoted $Script) | Invoke-Expression" }
    'latin1' { "[Text.Encoding]::GetEncoding(28591).GetString([IO.File]::ReadAllBytes($(Quoted $Script))) | Invoke-Expression" }
    'file' { "& $(Quoted $Script)" }
  }
  $command = @(
    '[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false'
    "`$env:KUMI_HOME = $(Quoted $KumiHome)"
    "`$env:KUMI_RELEASES = $(Quoted $From)"
    "`$env:KUMI_NO_LAUNCH = '1'"
    'Remove-Item Env:KUMI_NO_MODIFY_PATH, Env:KUMI_VERSION, Env:WT_SESSION, Env:COLORTERM, Env:NO_COLOR -ErrorAction SilentlyContinue'
    $run
  ) -join "`n"
  $start = New-Object Diagnostics.ProcessStartInfo
  $start.FileName = $shell
  $start.Arguments = '-NoProfile -ExecutionPolicy Bypass -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
  $start.UseShellExecute = $false
  $start.RedirectStandardOutput = $true
  $start.RedirectStandardError = $true
  $process = [Diagnostics.Process]::Start($start)
  $errors = $process.StandardError.ReadToEndAsync()
  $memory = New-Object IO.MemoryStream
  $process.StandardOutput.BaseStream.CopyTo($memory)
  $process.WaitForExit()
  $text = $bytes.GetString($memory.ToArray()).Replace($KumiHome, '<home>')
  # Windows PowerShell also writes its records to a redirected error output, as CLIXML: said only on a failure.
  if ($process.ExitCode -ne 0) { Write-Host "($How $Script exited $($process.ExitCode): $($errors.Result))" }
  $text
}
# What a script's step functions write in a console (Doing, then Done, plainly and in colour), and how it starts
# Kumi: the functions and glyphs from its own text, run with a Write-Host that keeps what it's given.
function Steps([string]$Script) {
  $text = $utf8.GetString([IO.File]::ReadAllBytes($Script))
  $ast = [Management.Automation.Language.Parser]::ParseInput($text, [ref]$null, [ref]$null)
  $code = @($ast.FindAll({ param($node) $node -is [Management.Automation.Language.AssignmentStatementAst] -and $node.Left.Extent.Text -in @('$ellipsis', '$dot', '$arrow') }, $true) | ForEach-Object { $_.Extent.Text })
  $code += @('Say', 'Doing', 'Done' | ForEach-Object { $name = $_; $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true).Extent.Text })
  $starting = $ast.Find({ param($node) $node -is [Management.Automation.Language.CommandAst] -and $node.CommandElements.Count -eq 2 -and $node.CommandElements[0].Extent.Text -eq 'Say' -and $node.CommandElements[1].Extent.Text -like "*Starting Kumi*" }, $true)
  $code += $starting.Extent.Text
  $written = New-Object Collections.ArrayList
  & {
    param($code, $written)
    function Write-Host { param([Parameter(Position = 0)]$Object, [switch]$NoNewline, $ForegroundColor) [void]$written.Add("[$Object] $NoNewline $ForegroundColor") }
    $interactive = $true; $colour = $false; $trueColour = $false; $esc = [char]27; $step = @{ Name = '' }
    $functions = $code[0..($code.Count - 2)] -join "`n"
    . ([ScriptBlock]::Create($functions))
    Doing 'Download'; Done 'checked against its checksum'
    $colour = $true
    Doing 'Install'; Done '<home>'; Doing 'PATH'; Done
    . ([ScriptBlock]::Create($code[-1]))
  } $code $written
  $written -join "`n"
}

try {
  $one = Release 'one' '1.0.0'
  $two = Release 'two' '2.0.0'
  $before = Home 'before'; $after = Home 'after'
  $glyph = { param($Text) $bytes.GetString($utf8.GetBytes($Text)) }

  # A first install: everything it prints, as 1.16.1 printed it.
  $was = Printed $Reference 'utf8' $before $one
  $is = Printed $Installer 'utf8' $after $one
  Write-Host '--- 1.16.1, a first install, as printed:'; Write-Host (Printable $was)
  Check 'a first install prints what 1.16.1 printed, byte for byte' ($is -ceq $was) (Printable $is)
  Check '... glyphs included (. is C2 B7)' ($was.Contains((& $glyph " $dot checked against its checksum")) -and $was.Contains((& $glyph "kumi update $dot Remove")))
  foreach ($how in 'file', 'latin1') {
    $also = Printed $Installer $how (Home $how) $one
    Check "... and run from $(if ($how -eq 'file') { 'the file' } else { 'a body read as ISO-8859-1 (a release asset)' }), the same" ($also -ceq $is) (Printable $also)
  }

  # An update: the same, and the Install line says what changed (new since 1.16.1).
  $was = Printed $Reference 'utf8' $before $two
  $is = Printed $Installer 'utf8' $after $two
  $changed = & $glyph ", 1.0.0 $arrow 2.0.0"
  $expected = [regex]::Replace($was, '(?m)^(  Install +done \xC2\xB7 <home>)(\r?)$', { param($m) $m.Groups[1].Value + $changed + $m.Groups[2].Value })
  Write-Host '--- this install.ps1, an update, as printed:'; Write-Host (Printable $is)
  Check 'an update prints what 1.16.1 printed, and the Install line adds ", 1.0.0 -> 2.0.0" (the arrow is E2 86 92)' (($expected -cne $was) -and ($is -ceq $expected)) (Printable $is)

  # In a console: each step's "..." and "Starting Kumi...".
  $was = Steps $Reference
  $is = Steps $Installer
  Check 'in a console, the steps and "Starting Kumi..." print what 1.16.1 printed' ($is -ceq $was) "1.16.1:`n$was`nthis:`n$is"
  Check '... glyphs included (... is E2 80 A6)' ($was.Contains("Download            $ellipsis]") -and $was.Contains("Starting Kumi$ellipsis]"))
} finally {
  if ($server) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
  if ($null -ne $userPath) { $environment.SetValue('Path', $userPath, $userPathKind) } else { $environment.DeleteValue('Path', $false) }
  $environment.Close()
  Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures) { throw "$failures check(s) failed" }
