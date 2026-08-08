# Windows entry point. The real work is in run.mjs, which runs the same way on
# every platform.
#
# Arguments are forwarded verbatim, so the POSIX-style flags documented in
# run.mjs work here too:  .\run.ps1 --dev --file clip.mov
#
# This script deliberately declares no param() block. PowerShell only performs
# parameter binding against declared parameters, so without one, flags like
# --file land in $args untouched instead of being captured by the script.

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

if (-not (Get-Command bun -ErrorAction SilentlyContinue)) {
    Write-Host 'error: bun is not installed.' -ForegroundColor Red
    Write-Host '  Install Bun:  powershell -c "irm bun.sh/install.ps1 | iex"'
    exit 1
}

# `@args` splats, `$args` would not: bun is often an npm-generated .ps1 shim,
# and passing an array to a PowerShell script hands it over as one argument.
bun run.mjs @args
exit $LASTEXITCODE
