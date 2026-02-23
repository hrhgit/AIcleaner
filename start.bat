@echo off
title Dust Space Cleaner
echo ===================================================
echo        Starting Dust Space Cleaner Agent...
echo ===================================================
echo.

REM Kill existing processes on port 3001 and 5173
echo [0/2] Freeing ports...
for /f "tokens=5" %%a in ('netstat -aon ^| findstr ":3001 " ^| findstr "LISTENING"') do (
    taskkill /PID %%a /F >nul 2>&1
)
for /f "tokens=5" %%a in ('netstat -aon ^| findstr ":5173 " ^| findstr "LISTENING"') do (
    taskkill /PID %%a /F >nul 2>&1
)

echo [1/2] Starting Express Backend (Port 3001)
echo [2/2] Starting Vite Frontend  (Port 5173)
echo.
echo   Frontend: http://localhost:5173/
echo   Backend:  http://localhost:3001/
echo.
echo Press Ctrl+C to stop both servers.
echo.

npm start

echo.
pause
