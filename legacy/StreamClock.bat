@echo off
rem StreamClock launcher (PowerShell コンソールを隠して起動)
start "" /min powershell -NoProfile -ExecutionPolicy Bypass -STA -WindowStyle Hidden -File "%~dp0StreamClock.ps1"
