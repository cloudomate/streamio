@echo off
REM Build script for Streamio Virtual Display Driver (IddCx 1.2 UMDF2)
REM Requires: VS2022 Build Tools + Windows SDK + WDK
REM
REM Output: streamio-display.dll (UMDF2 user-mode driver)

setlocal

REM ---- Toolchain paths ----
set MSVC_VER=14.44.35207
set SDK_VER=10.0.26100.0
set MSVC_ROOT=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\%MSVC_VER%
set SDK_INC=C:\Program Files (x86)\Windows Kits\10\Include\%SDK_VER%
set SDK_LIB=C:\Program Files (x86)\Windows Kits\10\Lib\%SDK_VER%
set WDF_INC=C:\Program Files (x86)\Windows Kits\10\Include\wdf\umdf\2.33
set WDF_LIB=C:\Program Files (x86)\Windows Kits\10\Lib\wdf\umdf\x64\2.33
set IDDCX_INC=C:\Program Files (x86)\Windows Kits\10\Include\%SDK_VER%\um\iddcx\1.2
set IDDCX_LIB=C:\Program Files (x86)\Windows Kits\10\Lib\%SDK_VER%\um\x64\iddcx\1.2

set CC="%MSVC_ROOT%\bin\Hostx64\x64\cl.exe"
set LD="%MSVC_ROOT%\bin\Hostx64\x64\link.exe"

echo.
echo === Building Streamio Virtual Display Driver ===
echo.

REM ---- Compile ----
echo [1/2] Compiling streamio-display.cpp ...
%CC% /nologo /c /Zi /W4 /WX- /Od /EHsc ^
    /D "_WIN64" /D "_AMD64_" /D "AMD64" ^
    /D "NTDDI_VERSION=0x0A00000C" ^
    /D "_WINDLL" /D "_UNICODE" /D "UNICODE" ^
    /D "UMDF_VERSION_MAJOR=2" /D "UMDF_VERSION_MINOR=33" ^
    /D "IDDCX_VERSION_MINOR=2" ^
    /I"%MSVC_ROOT%\include" ^
    /I"%IDDCX_INC%" ^
    /I"%SDK_INC%\um" ^
    /I"%SDK_INC%\shared" ^
    /I"%SDK_INC%\ucrt" ^
    /I"%SDK_INC%\km" ^
    /I"%WDF_INC%" ^
    /I. ^
    streamio-display.cpp
if errorlevel 1 (
    echo COMPILE FAILED
    exit /b 1
)

REM ---- Link ----
echo [2/2] Linking streamio-display.dll ...
%LD% /nologo /DEBUG /DLL ^
    /OUT:streamio-display.dll ^
    /LIBPATH:"%MSVC_ROOT%\lib\x64" ^
    /LIBPATH:"%SDK_LIB%\um\x64" ^
    /LIBPATH:"%SDK_LIB%\ucrt\x64" ^
    /LIBPATH:"%WDF_LIB%" ^
    /LIBPATH:"%IDDCX_LIB%" ^
    streamio-display.obj ^
    WdfDriverStubUm.lib ^
    iddcxstub.lib ^
    d3d11.lib dxgi.lib ^
    onecoreuap.lib ^
    ntdll.lib ^
    ucrt.lib vcruntime.lib msvcrt.lib
if errorlevel 1 (
    echo LINK FAILED
    exit /b 1
)

echo.
echo === Build successful: streamio-display.dll ===
echo.
echo Next steps:
echo   1. Enable test signing:  bcdedit /set testsigning on  (then reboot if needed)
echo   2. Sign the driver:      signtool sign /v /s My /sm /sha1 EB9A1C3C7CDF8B38E8EF4EFE99C59645F1733151 /fd SHA256 streamio-display.dll
echo   3. Install device:       devcon install streamio-display.inf Root\StreamioDisplay
echo   4. Test:                 display-ctl create 1920 1080 60
echo.
