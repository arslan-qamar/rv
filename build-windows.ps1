$ErrorActionPreference = 'Stop'
Set-Location "$PSScriptRoot\host"
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Cargo build failed' }

$MakeNsis = Get-Command makensis -ErrorAction SilentlyContinue
if ($MakeNsis) {
    $MakeNsisPath = $MakeNsis.Source
} else {
    $MakeNsisPath = @(
        "${env:ProgramFiles(x86)}\NSIS\makensis.exe"
        "$env:ProgramFiles\NSIS\makensis.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
}
if (-not $MakeNsisPath) {
    throw 'NSIS makensis.exe was not found. Install NSIS or add it to PATH.'
}

Set-Location "$PSScriptRoot\installer"
& $MakeNsisPath RVHost.nsi
if ($LASTEXITCODE -ne 0) { throw 'NSIS build failed' }
Write-Host "Installer: $PSScriptRoot\installer\RVHostSetup.exe"
