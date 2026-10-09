@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul
set "VULKAN_SDK=C:\VulkanSDK\1.4.350.0"
set "PATH=C:\VulkanSDK\1.4.350.0\Bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;%PATH%"
set "LIBCLANG_PATH=C:\Program Files\LLVM\bin"
set "CMAKE_GENERATOR=Ninja"
set "CARGO_TARGET_DIR=D:\kt"
cd /d "C:\Users\Owner\Documents\Hytale Code\ken"
cargo test --release -p ken-core workspace:: 2>&1
echo CORE_TEST_EXIT=%ERRORLEVEL%
cargo check --release -p ken-app 2>&1
echo APP_CHECK_EXIT=%ERRORLEVEL%
