@echo off
rem Ken on Windows: set up the MSVC + Vulkan + LLVM + Ninja environment, then
rem run one task. Works from any checkout on any machine; paths are found, not
rem hard-coded. Plain `cargo` in a normal shell fails on whisper-rs/llama-cpp-2
rem (Vulkan's glslc, libclang, Ninja), which is why this exists.
rem
rem   scripts\win-build.cmd doctor     what is found and what is missing
rem   scripts\win-build.cmd core       ken-core tests, no GPU features (no Vulkan/LLVM needed)
rem   scripts\win-build.cmd test       ken-core tests with the default features
rem   scripts\win-build.cmd check      sidecar, then cargo check of the app
rem   scripts\win-build.cmd sidecar    build ken-mcp and stage it for Tauri
rem   scripts\win-build.cmd dev        sidecar, then `npm run tauri dev`
rem   scripts\win-build.cmd build      sidecar, then `npm run tauri build`
rem   scripts\win-build.cmd cargo ...  any cargo command in this environment
rem
rem CARGO_TARGET_DIR defaults to C:\kt: a short path keeps the CMake builds
rem under the Windows path limit. Set it first to use another folder.
setlocal EnableDelayedExpansion
cd /d "%~dp0.."

set "TASK=%~1"
if "%TASK%"=="" set "TASK=doctor"
if not defined CARGO_TARGET_DIR set "CARGO_TARGET_DIR=C:\kt"

rem --- MSVC ---------------------------------------------------------------
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VSROOT="
if exist "%VSWHERE%" (
  for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSROOT=%%i"
)
if defined VSROOT (
  set "PATH=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer;!PATH!"
  call "!VSROOT!\VC\Auxiliary\Build\vcvars64.bat" >nul
  set "PATH=!VSROOT!\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;!PATH!"
)

rem --- Vulkan SDK: the installer sets VULKAN_SDK; else take the newest ------
if not defined VULKAN_SDK (
  if exist "C:\VulkanSDK" for /f "delims=" %%d in ('dir /b /ad /o:n "C:\VulkanSDK" 2^>nul') do set "VULKAN_SDK=C:\VulkanSDK\%%d"
)
if defined VULKAN_SDK set "PATH=%VULKAN_SDK%\Bin;%PATH%"

rem --- LLVM (libclang for bindgen) ----------------------------------------
if not defined LIBCLANG_PATH (
  if exist "%ProgramFiles%\LLVM\bin\libclang.dll" set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin"
)
set "CMAKE_GENERATOR=Ninja"

rem --- what is there ------------------------------------------------------
set "MISSING="
if not defined VSROOT set "MISSING=!MISSING! msvc"
where ninja >nul 2>&1 || set "MISSING=!MISSING! ninja"
where glslc >nul 2>&1 || set "MISSING=!MISSING! vulkan-sdk"
if not defined LIBCLANG_PATH set "MISSING=!MISSING! llvm"
where cargo >nul 2>&1 || set "MISSING=!MISSING! rust"
rem Node: an nvm-selected older Node can shadow a newer system install. Use the
rem system one for this run only, so nvm's choice for other projects stands.
set "NODEMAJOR=0"
for /f "tokens=1 delims=v." %%v in ('node --version 2^>nul') do set "NODEMAJOR=%%v"
if !NODEMAJOR! LSS 22 if exist "%ProgramFiles%\nodejs\node.exe" (
  set "PATH=%ProgramFiles%\nodejs;!PATH!"
  for /f "tokens=1 delims=v." %%v in ('node --version 2^>nul') do set "NODEMAJOR=%%v"
)
if !NODEMAJOR! LSS 22 set "MISSING=!MISSING! node22+"

if /i "%TASK%"=="doctor" goto doctor
if /i "%TASK%"=="core" goto core
if not "!MISSING!"=="" (
  echo Missing:!MISSING!
  echo Run `scripts\win-build.cmd doctor` for details. `core` works without Vulkan and LLVM.
  exit /b 1
)
if /i "%TASK%"=="test" goto test
if /i "%TASK%"=="check" goto check
if /i "%TASK%"=="sidecar" goto sidecar
if /i "%TASK%"=="dev" goto dev
if /i "%TASK%"=="build" goto build
if /i "%TASK%"=="cargo" goto cargo
echo Unknown task "%TASK%". See the top of %~nx0.
exit /b 2

:doctor
echo repo           %CD%
echo target dir     %CARGO_TARGET_DIR%
echo msvc           %VSROOT%
echo vulkan sdk     %VULKAN_SDK%
echo libclang       %LIBCLANG_PATH%
for /f "delims=" %%v in ('node --version 2^>nul') do echo node           %%v
if "!MISSING!"=="" (echo ready          yes) else (echo missing       !MISSING!)
exit /b 0

:core
cargo test --release -p ken-core --no-default-features
exit /b %ERRORLEVEL%

:test
cargo test --release -p ken-core
exit /b %ERRORLEVEL%

:check
rem tauri-build refuses to run until the externalBin sidecar is staged.
call :sidecar || exit /b 1
cargo check --release -p ken-app
exit /b %ERRORLEVEL%

:sidecar
cargo build --release -p ken-mcp || exit /b 1
for /f "tokens=2" %%h in ('rustc -vV ^| findstr /b "host:"') do set "HOST=%%h"
if not exist "src-tauri\binaries" mkdir "src-tauri\binaries"
copy /y "%CARGO_TARGET_DIR%\release\ken-mcp.exe" "src-tauri\binaries\ken-mcp-!HOST!.exe" >nul
exit /b %ERRORLEVEL%

:dev
call :sidecar || exit /b 1
npm run tauri dev
exit /b %ERRORLEVEL%

:build
call :sidecar || exit /b 1
npm run tauri build
exit /b %ERRORLEVEL%

:cargo
rem Everything after `cargo`, run as a cargo command in this environment.
set "ARGS="
:cargo_args
shift
if "%~1"=="" goto cargo_run
set "ARGS=!ARGS! %1"
goto cargo_args
:cargo_run
cargo !ARGS!
exit /b %ERRORLEVEL%
