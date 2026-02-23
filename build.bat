@echo off
setlocal
chcp 65001 >nul
title 打包 AIcleaner
echo ===================================
echo   开始打包 AIcleaner 
echo ===================================

echo [1/6] 前端构建 (Vite Build)...
call npm run build
if %ERRORLEVEL% neq 0 (
    echo.
    echo Error: Vite build failed! Check the error messages above.
    pause
    exit /b 1
)

echo [2/6] 清理旧目录...
if exist release rmdir /s /q release
mkdir release

echo [3/6] 检查独立的 Node.js 运行环境...
if not exist "bin" mkdir bin
if not exist "bin\node.exe" (
    echo   初次打包，正在从服务器下载 node.exe ^(约 40MB^)...
    powershell -Command "$ErrorActionPreference = 'Stop'; [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri 'https://nodejs.org/dist/v20.11.1/win-x64/node.exe' -OutFile 'bin\node.exe'"
) else (
    echo   发现缓存的 node.exe,跳过下载。
)

echo [4/6] 迁移核心文件...
xcopy dist release\dist\ /E /I /Q
xcopy server release\server\ /E /I /Q
xcopy scripts release\scripts\ /E /I /Q
if exist bin xcopy bin release\bin\ /E /I /Q

copy package.json release\ >nul
copy package-lock.json release\ >nul

if exist release\server\data\settings.json del release\server\data\settings.json

echo [5/6] 安装生产依赖并获取必要组件 (dust)...
cd release
call npm install --omit=dev
if %ERRORLEVEL% neq 0 (
    echo.
    echo Error: npm install failed! Check the error messages above.
    cd ..
    pause
    exit /b 1
)
cd ..

echo [6/6] 生成一键启动脚本...
(
echo @echo off
echo chcp 65001 ^>nul
echo title AIcleaner
echo echo.
echo echo 正在启动服务...
echo echo 请在浏览器中访问 http://localhost:3001
echo echo.
echo start http://localhost:3001
echo bin\node.exe server/index.js
echo.
echo echo 服务已退出...
echo pause
) > release\start.bat

echo.
echo [7/7] Inno Setup...
set "ISCC=%~d0\Program Files (x86)\Inno Setup 6\ISCC.exe"
if not exist "%ISCC%" set "ISCC=E:\Program Files (x86)\Inno Setup 6\ISCC.exe"
if not exist "%ISCC%" (
    echo   Error: ISCC.exe not found. Please install Inno Setup 6.
    goto SKIP_INSTALLER
)
if exist AIcleaner_Setup.exe del AIcleaner_Setup.exe
"%ISCC%" scripts\dust_setup.iss
if %ERRORLEVEL% neq 0 (
    echo.
    echo Error: Inno Setup compile failed.
    pause
    exit /b 1
)

:SKIP_INSTALLER
echo.
echo ===================================
echo 构建全部完成！
echo.
echo 方式A（免安装绿色版）：直接把 "release" 文件夹发给他人，运行 start.bat 即可启动。
echo 方式B（专业安装包）  ：发给他人根目录的 "AIcleaner_Setup.exe"，双击弹出安装向导，
echo                         支持选择安装目录、创建快捷方式，并可从控制面板卸载。
echo ===================================
pause
