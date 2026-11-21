@echo off
rem Tandem MIDI recorder. Saves every message a MIDI controller sends to a
rem text file, so its mapping can be checked or fixed. Keep this file next to
rem tandem.exe and double-click it. It works with any Tandem version that has
rem --midi-monitor. The PowerShell part is at the end of this file.
setlocal EnableExtensions DisableDelayedExpansion
title Tandem MIDI recorder
cd /d "%~dp0"

echo Tandem MIDI recorder
echo.
echo This saves every message your controller sends to a text file, so its
echo mapping can be checked or fixed. Plug in only the controller you want to
echo record, and close any other DJ software.
echo.
if not exist "tandem.exe" goto noexe

:closetandem
tasklist /FI "IMAGENAME eq tandem.exe" /NH 2>nul | find /I "tandem.exe" >nul
if errorlevel 1 goto askname
echo Tandem is open. Only one app can use the controller at a time, so close
echo Tandem first, then press any key here.
pause >nul
echo.
goto closetandem

:askname
set "REC_NAME="
set /p "REC_NAME=Name for the recording, for example sc-live-4: "
if not defined REC_NAME goto askname
set "REC_DIR=%CD%"
set "REC_BAT=%~f0"
set "REC_SECONDS=600"
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "iex (((Get-Content -LiteralPath $env:REC_BAT -Raw) -split '(?m)^:::PS\r?$')[1])"
echo.
pause
exit /b

:noexe
echo tandem.exe isn't in this folder. Put this file next to tandem.exe and
echo run it again.
echo.
pause
exit /b 1

:::PS
# Run by the batch part above, which sets REC_NAME, REC_DIR and REC_SECONDS.
# tandem.exe writes straight into the file, line by line, so closing the
# window keeps everything recorded so far; this part shows the lines as they
# arrive and stops the recording on Enter.
$name = ($env:REC_NAME -replace '[\\/:*?"<>|\[\]]', '-').Trim(' .')
if (-not $name) { $name = 'midi-recording' }
if (-not [IO.Path]::GetExtension($name)) { $name += '.txt' }
$base = [IO.Path]::GetFileNameWithoutExtension($name)
$ext = [IO.Path]::GetExtension($name)
$path = Join-Path $env:REC_DIR $name
$n = 2
while (Test-Path -LiteralPath $path) {
    $path = Join-Path $env:REC_DIR ('{0}-{1}{2}' -f $base, $n, $ext)
    $n++
}
$minutes = [int]$env:REC_SECONDS / 60

Write-Host "Recording into $path"
Write-Host ''
Write-Host 'Press the controls now, about a second apart. Each message shows up below.'
Write-Host "Press Enter here when you're done. It stops by itself after $minutes minutes."
Write-Host ''

$exe = Join-Path $env:REC_DIR 'tandem.exe'
$p = Start-Process -FilePath $exe -ArgumentList '--midi-monitor', $env:REC_SECONDS -NoNewWindow -PassThru -RedirectStandardOutput $path
if (-not $p) {
    Write-Host "Couldn't start tandem.exe."
    return
}
$null = $p.Handle  # keeps the exit code readable after it exits
while (-not (Test-Path -LiteralPath $path)) { Start-Sleep -Milliseconds 50 }
$stream = New-Object IO.FileStream($path, 'Open', 'Read', 'ReadWrite')
$reader = New-Object IO.StreamReader($stream, [Text.Encoding]::UTF8)
$stopped = $false
while ($true) {
    $line = $reader.ReadLine()
    if ($line -ne $null) {
        Write-Host $line
        continue
    }
    if ($p.HasExited) { break }
    if (-not [Console]::IsInputRedirected -and [Console]::KeyAvailable) {
        if ([Console]::ReadKey($true).Key -eq 'Enter') {
            $stopped = $true
            Stop-Process -Id $p.Id -ErrorAction SilentlyContinue
            $p.WaitForExit()
            break
        }
    }
    Start-Sleep -Milliseconds 50
}
while (($line = $reader.ReadLine()) -ne $null) { Write-Host $line }
$reader.Close()

Write-Host ''
if (-not $stopped -and $p.ExitCode -ne 0) {
    Write-Host 'The recording did not start: the message above says why.'
}
Write-Host "Saved: $path"
Write-Host 'Send that file to whoever is fixing the mapping.'
Start-Process explorer.exe -ArgumentList "/select,`"$path`""
