@echo off
rem Developer build: compiles JAI and refreshes JAI.exe (native window, no console) here.
rem To run J.A.I. just double-click JAI.exe; add --console to see the live log.
title Building J.A.I.
cd /d "%~dp0"

rem Toolchain for building whisper.cpp (cmake + libclang for bindgen)
set "PATH=%USERPROFILE%\.cargo\bin;C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;%PATH%"
if not defined LIBCLANG_PATH set "LIBCLANG_PATH=%~dp0tools\libclang"

cargo build --release || goto :failed
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0tools\package.ps1" || goto :failed
echo.
echo Done: %~dp0JAI.exe
exit /b 0

:failed
echo.
echo Build failed.
pause
exit /b 1
