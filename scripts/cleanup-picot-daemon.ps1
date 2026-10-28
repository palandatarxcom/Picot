# ABOUTME: Stops a Picot daemon that outlived a killed Picot app on Windows, then clears
# ABOUTME: its stale instance and lock files. Verifies the recorded pid still runs picot-daemon.

<#
.SYNOPSIS
    Cleans up a Picot daemon process left behind by a killed Picot app.

.DESCRIPTION
    The daemon is detached from the app on purpose, so ending Picot does not end
    the daemon. This script reads the daemon's instance file, proves the recorded
    pid still runs picot-daemon.exe (a reused pid is never killed), asks for
    confirmation, stops the process tree, and removes the instance and lock
    files once the process is gone. The log is kept.

    Windows builds currently ship no separate daemon, so a missing instance file
    is the normal answer: there is nothing to clean up.

.PARAMETER DataDir
    Picot app data directory. Defaults to %APPDATA%\<bundle identifier>.

.PARAMETER Yes
    Do not ask for confirmation.

.PARAMETER DryRun
    Report what would happen and change nothing.

.PARAMETER GraceSeconds
    Seconds a graceful close gets before the process tree is force-killed.
#>

param(
    [string]$DataDir,
    [switch]$Yes,
    [switch]$DryRun,
    [int]$GraceSeconds = 10
)

$ErrorActionPreference = "Stop"

function Fail([string]$Message) {
    Write-Error "cleanup-picot-daemon: $Message"
    exit 1
}

if (-not $DataDir) {
    # Mirrors `identifier` in src-tauri/tauri.conf.json. Tauri's app_data_dir on
    # Windows resolves to the roaming app data directory plus that identifier.
    $identifier = "com.palandata.picot"
    $DataDir = Join-Path $env:APPDATA $identifier
}

$instance = Join-Path $DataDir "picot-daemon.json"
$lock = Join-Path $DataDir "picot-daemon.lock"
$log = Join-Path $DataDir "picot-daemon.log"

if (-not (Test-Path -LiteralPath $instance)) {
    Write-Host "No daemon instance file at $instance"
    Write-Host "Nothing to clean up: no Picot daemon is recorded for this installation."
    exit 0
}

$document = Get-Content -LiteralPath $instance -Raw | ConvertFrom-Json
$daemonPid = 0
if (-not [int]::TryParse([string]$document.pid, [ref]$daemonPid) -or $daemonPid -le 0) {
    Fail "the instance file has no usable pid ($instance); inspect it by hand"
}

$process = Get-Process -Id $daemonPid -ErrorAction SilentlyContinue

if (-not $process) {
    Write-Host "Picot daemon pid $daemonPid is not running; the instance file is stale."
    if ($DryRun) {
        Write-Host "Would remove $instance"
        Write-Host "Would remove $lock"
        exit 0
    }
    Remove-Item -LiteralPath $instance -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $lock -Force -ErrorAction SilentlyContinue
    Write-Host "Removed stale instance and lock files."
    Write-Host "Daemon log kept at $log"
    exit 0
}

# A reused pid must never be signalled: require the name and, when available,
# the executable path to name picot-daemon.
$name = $process.ProcessName
$path = $null
try { $path = $process.Path } catch { $path = $null }
$nameMatches = $name -ieq "picot-daemon"
$pathMatches = $path -and ([System.IO.Path]::GetFileName($path) -ieq "picot-daemon.exe")
if (-not $nameMatches -or ($path -and -not $pathMatches)) {
    Fail "pid $daemonPid is alive but is not picot-daemon.exe (pid reuse?); refusing to stop it.`nInspect $instance by hand before removing it; nothing was changed."
}

Write-Host "Picot daemon is running as pid $daemonPid"

if ($DryRun) {
    Write-Host "Would stop the process tree, wait up to ${GraceSeconds}s, force-kill if needed,"
    Write-Host "then remove $instance and $lock (log kept at $log)."
    exit 0
}

if (-not $Yes) {
    $answer = Read-Host "Stop it now? [y/N]"
    if ($answer -notmatch '^(y|yes)$') {
        Write-Host "Cancelled; nothing was changed."
        exit 1
    }
}

# taskkill /T takes the daemon's children (the pi processes) with it. The first
# pass is a close request; /F is reserved for a tree that will not close.
& taskkill.exe /PID $daemonPid /T *> $null
$waited = 0
while ((Get-Process -Id $daemonPid -ErrorAction SilentlyContinue) -and $waited -lt $GraceSeconds) {
    Start-Sleep -Seconds 1
    $waited++
}

if (Get-Process -Id $daemonPid -ErrorAction SilentlyContinue) {
    $stillSame = Get-Process -Id $daemonPid -ErrorAction SilentlyContinue
    if (-not $stillSame -or $stillSame.ProcessName -ine "picot-daemon") {
        Fail "pid $daemonPid changed identity while waiting; refusing to force-kill"
    }
    Write-Host "Daemon did not stop within ${GraceSeconds}s; force-killing the process tree."
    & taskkill.exe /PID $daemonPid /T /F *> $null
    Start-Sleep -Seconds 1
}

if (Get-Process -Id $daemonPid -ErrorAction SilentlyContinue) {
    Fail "pid $daemonPid is still alive; no files were removed"
}

Remove-Item -LiteralPath $instance -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $lock -Force -ErrorAction SilentlyContinue
Write-Host "Picot daemon stopped. Log kept at $log"
