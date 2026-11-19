@echo off
echo Ustanovka Visual Studio Build Tools (C++) - nuzhno dlya sborki Rust na Windows.
echo Eto ~3-6 GB, zhdi poka zakonchitsya. Soglasis' na zapros UAC.
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
echo.
echo Gotovo. ZAKROJ eto okno, otkroj NOVOE i zapusti build.bat
pause
