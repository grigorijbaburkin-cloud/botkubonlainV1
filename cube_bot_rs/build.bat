@echo off
where cargo >nul 2>nul
if errorlevel 1 (
  echo.
  echo Rust is not installed ^(cargo not found^).
  echo 1. Download and run rustup-init.exe from https://rustup.rs
  echo 2. Agree to install the Visual Studio C++ Build Tools when it asks.
  echo 3. CLOSE this window, open a NEW one and run build.bat again.
  echo.
  pause
  exit /b 1
)
cargo build --release
if errorlevel 1 (
  echo.
  echo Build failed. If you see "link.exe not found": run install_build_tools.bat,
  echo then CLOSE this window, open a NEW one and run build.bat again.
  echo Otherwise copy the error text above and send it to me.
  pause
  exit /b 1
)
echo.
echo Done: target\release\cube_bot.exe
pause
