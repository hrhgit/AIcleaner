@echo off
title AIcleaner
cd /d "%~dp0"
set FRONTEND_PORT=4173
set VITE_PORT=%FRONTEND_PORT%
echo ===================================================
echo        Starting AIcleaner...
echo ===================================================
echo.

REM Kill existing processes on port 3001 and frontend port
echo [0/2] Freeing ports...
for /f "tokens=5" %%a in ('netstat -aon ^| findstr ":3001 " ^| findstr "LISTENING"') do (
    taskkill /PID %%a /F >nul 2>&1
)
for /f "tokens=5" %%a in ('netstat -aon ^| findstr ":%FRONTEND_PORT% " ^| findstr "LISTENING"') do (
    taskkill /PID %%a /F >nul 2>&1
)

echo [1/2] Starting Express Backend (Port 3001)
echo [2/2] Starting Vite Frontend  (Port %FRONTEND_PORT%)
echo.
echo   Frontend: http://127.0.0.1:%FRONTEND_PORT%/
echo   Backend:  http://localhost:3001/
echo.
echo Press Ctrl+C to stop both servers.
echo.

npm start

echo.
pause
