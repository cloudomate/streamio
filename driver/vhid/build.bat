@echo off
REM Build script for Streamio Virtual HID Driver (KMDF + VHF)
REM Requires: VS2022 Build Tools + Windows SDK + WDK

setlocal

REM ---- Toolchain paths ----
set MSVC_VER=14.44.35207
set SDK_VER=10.0.26100.0
set MSVC_ROOT=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\%MSVC_VER%
set SDK_INC=C:\Program Files (x86)\Windows Kits\10\Include\%SDK_VER%
set SDK_LIB=C:\Program Files (x86)\Windows Kits\10\Lib\%SDK_VER%
set WDF_INC=C:\Program Files (x86)\Windows Kits\10\Include\wdf\kmdf\1.33
set WDF_LIB=C:\Program Files (x86)\Windows Kits\10\Lib\wdf\kmdf\x64\1.33

set CC="%MSVC_ROOT%\bin\Hostx64\x64\cl.exe"
set LD="%MSVC_ROOT%\bin\Hostx64\x64\link.exe"

echo.
echo === Building Streamio Virtual HID Driver ===
echo.

REM ---- Compile ----
echo [1/2] Compiling streamio-vhid.c ...
%CC% /nologo /c /Zi /W4 /WX- /Od ^
    /D "_WIN64" /D "_AMD64_" /D "AMD64" ^
    /D "NTDDI_VERSION=0x0A00000C" ^
    /D "_KERNEL_MODE=1" ^
    /D "KMDF_VERSION_MAJOR=1" /D "KMDF_VERSION_MINOR=33" ^
    /kernel /GS /EHs-c- /Zp8 ^
    /I"%SDK_INC%\km\crt" ^
    /I"%SDK_INC%\km" ^
    /I"%SDK_INC%\shared" ^
    /I"%SDK_INC%\um" ^
    /I"%WDF_INC%" ^
    /I. ^
    streamio-vhid.c
if errorlevel 1 (
    echo COMPILE FAILED
    exit /b 1
)

REM ---- Link ----
echo [2/2] Linking streamio-vhid.sys ...
%LD% /nologo /DEBUG /DRIVER:WDM ^
    /ENTRY:FxDriverEntry ^
    /SUBSYSTEM:NATIVE ^
    /NODEFAULTLIB ^
    /OUT:streamio-vhid.sys ^
    /LIBPATH:"%MSVC_ROOT%\lib\x64" ^
    /LIBPATH:"%SDK_LIB%\km\x64" ^
    /LIBPATH:"%SDK_LIB%\um\x64" ^
    /LIBPATH:"%WDF_LIB%" ^
    streamio-vhid.obj ^
    ntoskrnl.lib hal.lib wmilib.lib ^
    WdfDriverEntry.lib WdfLdr.lib ^
    vhfkm.lib ^
    BufferOverflowFastFailK.lib
if errorlevel 1 (
    echo LINK FAILED
    exit /b 1
)

echo.
echo === Build successful: streamio-vhid.sys ===
echo.
echo Next steps:
echo   1. Enable test signing:  bcdedit /set testsigning on  (then reboot)
echo   2. Sign the driver:      signtool sign /v /s My /sm /sha1 EB9A1C3C7CDF8B38E8EF4EFE99C59645F1733151 /fd SHA256 streamio-vhid.sys
echo   3. Install:              pnputil /add-driver streamio-vhid.inf /install
echo.
