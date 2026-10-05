# Build dist\LanShare.exe (v2, Rust) and smoke-test the release exe for real.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File build.ps1
# Steps: wasm -> copy into web assets -> clippy -> cargo test -> node tests -> release build -> node smoke on the release exe.
# Keep this file ASCII-only: Windows PowerShell 5.1 reads BOM-less scripts in the local code page.
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

function Step($name, [scriptblock]$body) {
    Write-Host "==> $name"
    & $body
    if ($LASTEXITCODE -ne 0) { throw "$name failed (exit $LASTEXITCODE)" }
}

# Release binaries embed the source paths of dependencies (for panic messages). They live under the
# cargo home, inside the user profile, so they would publish the user name: map them to "/cargo".
# CARGO_ENCODED_RUSTFLAGS (not RUSTFLAGS) so a path with spaces stays one argument.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$cargoHome=/cargo"

$targets = rustup target list --installed
if ($targets -notcontains "wasm32-unknown-unknown") {
    throw "missing wasm target: run 'rustup target add wasm32-unknown-unknown' first"
}

Step "build wasm" { cargo build -p lanshare-wasm --release --target wasm32-unknown-unknown }
Copy-Item "target\wasm32-unknown-unknown\release\lanshare_wasm.wasm" "crates\lanshare\web\lanshare.wasm" -Force

Step "clippy" { cargo clippy --workspace --all-targets -- -D warnings }
Step "cargo test" { cargo test --workspace }
Step "node tests (debug exe)" { node --test "web-tests/*.test.mjs" }
Step "release build" { cargo build -p lanshare --release }

New-Item -ItemType Directory -Force -Path dist | Out-Null
Copy-Item "target\release\LanShare.exe" "dist\LanShare.exe" -Force
$exe = (Resolve-Path "dist\LanShare.exe").Path

$env:LANSHARE_EXE = $exe
try {
    Step "smoke test (release exe)" { node --test "web-tests/interop.test.mjs" }
} finally {
    Remove-Item Env:\LANSHARE_EXE
}

$sizeKB = [math]::Round((Get-Item $exe).Length / 1KB)
$wasmKB = [math]::Round((Get-Item "crates\lanshare\web\lanshare.wasm").Length / 1KB, 1)
Write-Host "Done: dist\LanShare.exe ($sizeKB KB, wasm $wasmKB KB)"
