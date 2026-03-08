@echo off
title AIcleaner
cd /d "%~dp0"
set FRONTEND_PORT=4173
set VITE_PORT=%FRONTEND_PORT%
echo ===================================================
echo        Starting AIcleaner...
echo ===================================================
echo.

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
