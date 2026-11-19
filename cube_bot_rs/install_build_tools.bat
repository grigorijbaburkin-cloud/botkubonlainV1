@echo off
rem ======= Установка Visual Studio C++ Build Tools (нужны для сборки Rust на Windows) =======
rem Запусти этот файл, если build.bat пишет "linker `link.exe` not found".
where winget >nul 2>&1
if errorlevel 1 (
  echo winget не найден. Открой https://visualstudio.microsoft.com/ru/visual-cpp-build-tools/
  echo и установи вручную: галка "Desktop development with C++", кнопки "Установить".
  pause
  exit /b 1
)
echo Ставлю C++ Build Tools через winget. Это может занять 10-30 минут, не закрывай окно.
winget install --id Microsoft.VisualStudio.2022.BuildTools --exact --accept-package-agreements --accept-source-agreements --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
echo.
echo Готово. Закрой это окно, открой НОВОЕ окно, зайди в папку cube_bot_rs и запусти build.bat снова.
pause
