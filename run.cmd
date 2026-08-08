@echo off
REM Windows entry point, also double-clickable. The real work is in run.mjs,
REM which runs the same way on every platform.
where bun >nul 2>nul || (
  echo error: bun is not installed.
  echo   Install Bun:  powershell -c "irm bun.sh/install.ps1 ^| iex"
  exit /b 1
)
bun "%~dp0run.mjs" %*
