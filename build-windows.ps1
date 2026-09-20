$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot\host"
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed' }
Set-Location "$PSScriptRoot\installer"
makensis RVHost.nsi
if ($LASTEXITCODE -ne 0) { throw 'NSIS build failed' }
Write-Host "Installer: $PSScriptRoot\installer\RVHostSetup.exe"
