@echo off
rem ======= Cube Bot - cборка (запусти один раз на Windows 10/11) =======
rem Нужен Rust: https://rustup.rs  (rustup-init.exe, соглашайся на C++ Build Tools)
where cargo >nul 2>&1
if errorlevel 1 (
  echo Rust не установлен. Скачай rustup-init.exe с https://rustup.rs и запусти.
  echo Если только что установил - закрой это окно, открой новое и повтори.
  pause
  exit /b 1
)
cargo build --release
if errorlevel 1 (
  echo.
  echo Ошибка сборки. Если написано "linker link.exe not found" - запусти install_build_tools.bat,
  echo дождись конца, закрой окно, открой новое и снова build.bat
  pause
  exit /b 1
)
echo.
echo ГОТОВО! Файл бота:  %cd%\target\release\cube_bot.exe
echo Скопируй cube_bot.exe куда угодно - он один, никаких библиотек не нужно.
pause
