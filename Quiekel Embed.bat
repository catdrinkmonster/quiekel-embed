@echo off
rem Starts the Quiekel Embed desktop app (after making sure its packages are up to date).
rem For the browser version instead, run:  uv run quiekel-embed
cd /d "%~dp0"
rem The AI engine the installer chose (engine.txt): PyTorch for NVIDIA cards, else DirectML.
set "ENGINE="
if exist engine.txt findstr /x /i "directml" engine.txt >nul && set "ENGINE=--no-group nvidia --group directml"
uv sync --quiet %ENGINE%
start "" ".venv\Scripts\quiekel-embed-app.exe"
