@echo off
setlocal
cd /d "%~dp0"
powershell -ExecutionPolicy Bypass -File "%~dp0build-release.ps1" -OpenOutput
if errorlevel 1 (
  echo.
  echo Build failed. See errors above.
  pause
  exit /b 1
)
pause
