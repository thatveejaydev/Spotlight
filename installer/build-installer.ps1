$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$HelperDir = Join-Path $RepoRoot "helper-app"
$CargoToml = Join-Path $HelperDir "Cargo.toml"
$IssPath = Join-Path $PSScriptRoot "premiere-spotlight.iss"

# Read package version from helper-app/Cargo.toml
$versionMatch = Select-String -Path $CargoToml -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $versionMatch) {
    throw "Could not determine package version from $CargoToml"
}
$Version = $versionMatch.Matches[0].Groups[1].Value

# Locate Inno Setup compiler (ISCC.exe)
$IsccCandidates = @(
    (Get-Command iscc -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -ErrorAction SilentlyContinue),
    (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"),
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) }

$Iscc = $IsccCandidates | Select-Object -First 1
if (-not $Iscc) {
    throw "Inno Setup 6 (ISCC.exe) was not found. Install it via: winget install --id JRSoftware.InnoSetup -e"
}

Write-Host "Building premiere-spotlight.exe (release)..." -ForegroundColor Cyan
Push-Location $HelperDir
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

Write-Host "Building installer v$Version with $Iscc..." -ForegroundColor Cyan
& $Iscc "/DMyAppVersion=$Version" $IssPath
if ($LASTEXITCODE -ne 0) {
    throw "ISCC.exe failed with exit code $LASTEXITCODE"
}

$InstallerPath = Join-Path $RepoRoot "dist\PremiereSpotlightSetup-$Version.exe"
if (-not (Test-Path $InstallerPath)) {
    throw "Expected installer was not produced at $InstallerPath"
}

Write-Host "Installer created: $InstallerPath" -ForegroundColor Green
