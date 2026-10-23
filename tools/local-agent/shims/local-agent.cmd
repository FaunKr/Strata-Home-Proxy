@echo off
rem local-agent for PowerShell and cmd: hands every argument to the PowerShell
rem runner installed next to it, past the execution policy and without a profile.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0lib\local-agent.ps1" %*
exit /b %ERRORLEVEL%
