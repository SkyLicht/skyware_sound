# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
    Tests the checked-out code on this Windows PC, unattended, in one command.

.DESCRIPTION
    Runs what CI's Windows job runs (rustfmt, clippy, the test suite), builds
    the release executable, then runs the system check on the real audio
    device: `skyware check` plays at a 128-frame buffer and counts dropouts,
    then exports a 16-bar loop and verifies it. The device check is silent
    unless -Audible is given, so it can run while nobody is listening.

    Everything is written to target\local-test\<time>\: a log of each step,
    the check report as JSON, and summary.txt. The exit code is 0 only if
    every step passed.

.EXAMPLE
    pwsh scripts/local-test.ps1
    The full run: code checks, release build, 10 minutes on the device.

.EXAMPLE
    pwsh scripts/local-test.ps1 -Quick
    Release build and a 30-second device check only.
#>
param(
    # Minutes to play through the audio device; 0 skips the device check.
    [double]$Minutes = 10,
    # Buffer size to ask the device for, in frames.
    [int]$Buffer = 128,
    # Bars in the exported test loop; 0 skips the loop check.
    [int]$Bars = 16,
    # Part of an output device name; default: the system default device.
    [string]$Device = "",
    # How to share the device: auto, shared, low-latency or exclusive.
    [ValidateSet("auto", "shared", "low-latency", "exclusive")]
    [string]$Mode = "auto",
    # Let the device check be heard.
    [switch]$Audible,
    # Skip rustfmt, clippy and the test suite, and play for 30 seconds
    # unless -Minutes is given.
    [switch]$Quick
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# rustup installs to the user profile; a fresh shell may not have it on PATH.
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path $cargoBin) -and -not (($env:Path -split ";") -contains $cargoBin)) {
    $env:Path = "$cargoBin;$env:Path"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo was not found. Install Rust with: winget install Rustlang.Rustup"
}
if ($Quick -and -not $PSBoundParameters.ContainsKey("Minutes")) {
    $Minutes = 0.5
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$out = Join-Path $root "target\local-test\$stamp"
New-Item -ItemType Directory -Force $out | Out-Null
$results = [System.Collections.Generic.List[string]]::new()
$failed = $false

# Runs one step with its output in <name>.log, or with stdout in $stdout and
# stderr in the log.
function Step([string]$name, [string]$exe, [string[]]$arguments, [string]$stdout = "") {
    $log = Join-Path $out "$name.log"
    Write-Host "==> $name"
    $started = Get-Date
    # Native tools write progress to stderr; keep it in the log, not as errors.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    if ($stdout) {
        & $exe @arguments 1> $stdout 2> $log
    } else {
        & $exe @arguments *> $log
    }
    $code = $LASTEXITCODE
    $ErrorActionPreference = $previous
    $seconds = [int]((Get-Date) - $started).TotalSeconds
    if ($code -eq 0) {
        $script:results.Add("PASS  $name (${seconds}s)")
    } else {
        $script:results.Add("FAIL  $name (exit $code, ${seconds}s): see $log")
        $script:failed = $true
        Get-Content $log -Tail 30 | Write-Host
    }
    return ($code -eq 0)
}

$commit = (git rev-parse --short HEAD).Trim()
$branch = (git rev-parse --abbrev-ref HEAD).Trim()
$dirty = if (git status --porcelain) { " (with uncommitted changes)" } else { "" }
$results.Add("Commit $commit on $branch$dirty, $(cargo --version)")

if (-not $Quick) {
    Step "fmt" "cargo" @("fmt", "--all", "--", "--check") | Out-Null
    Step "clippy" "cargo" @("clippy", "--workspace", "--all-targets", "--", "-D", "warnings") | Out-Null
    Step "test" "cargo" @("test", "--workspace") | Out-Null
}

$built = Step "build" "cargo" @("build", "--release", "-p", "skyware")
if ($built -and ($Minutes -gt 0 -or $Bars -gt 0)) {
    $exe = Join-Path $root "target\release\skyware.exe"
    $check = @(
        "check",
        "--minutes", "$Minutes",
        "--bars", "$Bars",
        "--buffer", "$Buffer",
        "--mode", $Mode,
        "--out", (Join-Path $out "export"),
        "--json"
    )
    if (-not $Audible) { $check += "--silent" }
    if ($Device) { $check += @("--device", $Device) }
    Write-Host "    playing for $Minutes min at $Buffer frames in $Mode mode$(if (-not $Audible) { ', silently' })"
    $reportFile = Join-Path $out "report.json"
    Step "check" $exe $check $reportFile | Out-Null
    $json = if (Test-Path $reportFile) { Get-Content $reportFile -Raw } else { "" }
    if ($json -and $json.Trim().StartsWith("{")) {
        $report = $json | ConvertFrom-Json
        if ($report.device) {
            $results.Add("      device: $($report.device.device), $($report.device.host) in $($report.device.mode) mode")
            if ($report.device.note) { $results.Add("      note: $($report.device.note)") }
            foreach ($c in $report.device.checks) {
                $mark = if ($c.passed) { "PASS" } else { "FAIL" }
                $results.Add("      $mark device $($c.name): $($c.detail)")
            }
        }
        if ($report.deviceError) { $results.Add("      FAIL device: $($report.deviceError)") }
        if ($report.loop) {
            foreach ($c in $report.loop.checks) {
                $mark = if ($c.passed) { "PASS" } else { "FAIL" }
                $results.Add("      $mark loop $($c.name): $($c.detail)")
            }
        }
        if ($report.loopError) { $results.Add("      FAIL loop: $($report.loopError)") }
    }
}

$results.Add($(if ($failed) { "FAIL" } else { "PASS" }))
$results | Set-Content (Join-Path $out "summary.txt")
Write-Host ""
$results | Write-Host
Write-Host "Logs and report: $out"
exit $(if ($failed) { 1 } else { 0 })
