# Build dist\LanShare.exe in a clean venv, then smoke-test the exe for real.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File build.ps1
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

$venv = ".venv-build"
$py = Join-Path $venv "Scripts\python.exe"
if (-not (Test-Path $py)) {
    python -m venv $venv
    if ($LASTEXITCODE -ne 0) { throw "failed to create venv" }
}

& $py -m pip install --disable-pip-version-check -q -r requirements-build.txt -i https://pypi.tuna.tsinghua.edu.cn/simple
if ($LASTEXITCODE -ne 0) {
    & $py -m pip install --disable-pip-version-check -q -r requirements-build.txt
    if ($LASTEXITCODE -ne 0) { throw "pip install failed" }
}

& $py -m unittest discover -s tests -t .
if ($LASTEXITCODE -ne 0) { throw "unit tests failed, not building" }

& $py -m PyInstaller --noconfirm --clean --onefile --windowed --name LanShare --icon lanshare\web\icon.ico --hidden-import pystray._win32 --add-data "lanshare\web;lanshare\web" run.py
if ($LASTEXITCODE -ne 0) { throw "PyInstaller failed" }

& $py tools\smoke_test.py dist\LanShare.exe
if ($LASTEXITCODE -ne 0) { throw "exe smoke test failed" }

Write-Host "Done: dist\LanShare.exe"
