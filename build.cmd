@echo off
rem All-in-1 Manager: build it on Windows.
rem   build             a menu (also when you double-click this file)
rem   build release     the program to hand out  -> public\release\all-in-1-manager.exe (with plugins\)
rem                     Release and dev also leave a shortcut to the program next to this file.
rem   build dev         a debug build            -> public\dev\all-in-1-manager.exe
rem   build run         a debug build, and start it
rem   build test        all tests
rem Needs Rust (rustup.rs) and Visual Studio's "Desktop development with C++".
setlocal
cd /d "%~dp0"
set "EXE=all-in-1-manager.exe"
set "PAUSE_AT_END="
set "CHOICE=%~1"

if not "%CHOICE%"=="" goto :chosen
set "PAUSE_AT_END=1"
echo.
echo  All-in-1 Manager - build
echo.
echo    1  Release  (the program to hand out, in public\release)
echo    2  Dev      (debug build, in public\dev)
echo    3  Dev, and start it
echo    4  Tests
echo.
set /p "CHOICE=  Choose 1-4: "
if "%CHOICE%"=="1" set "CHOICE=release"
if "%CHOICE%"=="2" set "CHOICE=dev"
if "%CHOICE%"=="3" set "CHOICE=run"
if "%CHOICE%"=="4" set "CHOICE=test"

:chosen
where cargo >nul 2>nul
if errorlevel 1 (
  echo.
  echo Rust is not installed, or this window started before it was: install it from https://rustup.rs
  echo and open a new window.
  goto :failed
)
if /i "%CHOICE%"=="release" goto :release
if /i "%CHOICE%"=="dev" goto :dev
if /i "%CHOICE%"=="run" goto :dev
if /i "%CHOICE%"=="test" goto :test
echo Unknown choice "%CHOICE%": use release, dev, run or test.
goto :failed

:release
echo ^> cargo build --release -p aio-app
cargo build --release -p aio-app
if errorlevel 1 goto :failed
if not exist "public\release" mkdir "public\release"
copy /y "target\release\%EXE%" "public\release\%EXE%" >nul
if errorlevel 1 goto :inuse
rem The data packs of the Plugins page, next to the program, so it needs no download.
robocopy "source\Assets" "public\release\plugins\client-assets" /MIR /NFL /NDL /NJH /NJS /NP >nul
if errorlevel 8 (
  echo Copying source\Assets to public\release\plugins\client-assets failed.
  goto :failed
)
if not exist "public\release\plugins\default-catalog" mkdir "public\release\plugins\default-catalog"
copy /y "source\crates\app\assets\catalog\Full_Catalog.zip" "public\release\plugins\default-catalog\Full_Catalog.zip" >nul
if errorlevel 1 goto :failed
call :shortcut "public\release" "All-in-1 Manager"
echo.
echo Ready: %CD%\public\release\%EXE%
echo        and the shortcut "All-in-1 Manager" in %CD%
goto :done

:dev
echo ^> cargo build -p aio-app
cargo build -p aio-app
if errorlevel 1 goto :failed
if not exist "public\dev" mkdir "public\dev"
copy /y "target\debug\%EXE%" "public\dev\%EXE%" >nul
if errorlevel 1 goto :inuse
call :shortcut "public\dev" "All-in-1 Manager (dev)"
echo.
echo Ready: %CD%\public\dev\%EXE%
if /i "%CHOICE%"=="run" start "" /d "%CD%\public\dev" "%CD%\public\dev\%EXE%"
goto :done

:test
echo ^> cargo test --workspace
cargo test --workspace
if errorlevel 1 goto :failed
goto :done

:shortcut
rem A shortcut next to this file that starts the program in its own folder, so it keeps its own configuration\.
rem %1 = the folder of the program, %2 = the name of the shortcut. A failure only warns: the build itself worked.
powershell -NoProfile -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; $d=(Resolve-Path '%~1').Path; $s=(New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path (Get-Location) '%~2.lnk')); $s.TargetPath=(Join-Path $d '%EXE%'); $s.WorkingDirectory=$d; $s.IconLocation=(Join-Path $d '%EXE%')+',0'; $s.Description='All-in-1 Manager'; $s.Save()" >nul 2>nul
if errorlevel 1 echo Note: the shortcut "%~2" could not be made; the program is in %~1.
exit /b 0

:inuse
echo.
echo The program could not be copied: is it still running? Close the installer and build again.
goto :failed

:failed
echo.
echo Build failed (see above).
if defined PAUSE_AT_END pause
exit /b 1

:done
if defined PAUSE_AT_END pause
exit /b 0
