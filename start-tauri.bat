@echo off
setlocal
cd /d "%~dp0"
powershell -ExecutionPolicy Bypass -File "%~dp0start-tauri.ps1"
if errorlevel 1 (
  echo.
  echo Failed to start Tauri app.
  pause
  exit /b 1
)
