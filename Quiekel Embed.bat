@echo off
rem Starts the Quiekel Embed desktop app (after making sure its packages are up to date).
rem For the browser version instead, run:  uv run quiekel-embed
cd /d "%~dp0"
uv sync --quiet
start "" ".venv\Scripts\quiekel-embed-app.exe"
