# Master Release & Publishing Orchestrator for BknDb
# Coordinates publishing to both Crates.io and PyPI in one seamless workflow.

param (
    [switch]$DryRun,
    [switch]$SkipTests,
    [switch]$CratesOnly,
    [switch]$PyPIOnly,
    [ValidateSet("GitHubTag", "Local")]
    [string]$PyPIMode = "GitHubTag"
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$WorkspaceRoot = Resolve-Path "$ScriptDir\..\.."
Set-Location $WorkspaceRoot

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "         BknDb Unified Release & Publisher               " -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

# 1. Check workspace version
$cargoTomlContent = Get-Content "$WorkspaceRoot\Cargo.toml" -Raw
if ($cargoTomlContent -match 'version\s*=\s*"([^"]+)"') {
    $RustVersion = $matches[1]
} else {
    Write-Error "Could not read Rust version."
    exit 1
}

$pyprojectContent = Get-Content "$WorkspaceRoot\bindings\python\pyproject.toml" -Raw
if ($pyprojectContent -match 'version\s*=\s*"([^"]+)"') {
    $PyVersion = $matches[1]
} else {
    Write-Error "Could not read Python version."
    exit 1
}

Write-Host "Rust Workspace Version   : v$RustVersion" -ForegroundColor Yellow
Write-Host "Python Package Version   : v$PyVersion" -ForegroundColor Yellow

# Pre-flight: Check that all versions match across the entire workspace
Write-Host "`nChecking repository-wide version consistency..." -ForegroundColor Cyan
& "$ScriptDir\check_versions.ps1" -ExpectedVersion $RustVersion
if ($LASTEXITCODE -ne 0) {
    Write-Error "Version check failed. Aborting unified release."
    exit 1
}

# 2. Run Pre-flight Tests
if (-not $SkipTests -and -not $DryRun) {
    Write-Host "`n[Step 1/3] Running Cargo Workspace Tests..." -ForegroundColor Cyan
    & cargo test --workspace
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Cargo tests failed! Aborting release."
        exit $LASTEXITCODE
    }
    Write-Host "[OK] All Cargo tests passed!" -ForegroundColor Green
} else {
    Write-Host "`n[Step 1/3] Pre-flight tests skipped." -ForegroundColor DarkGray
}

# 3. Publish to Crates.io
if (-not $PyPIOnly) {
    Write-Host "`n[Step 2/3] Publishing to Crates.io..." -ForegroundColor Cyan
    $cratesArgs = @{}
    if ($DryRun) { $cratesArgs["DryRun"] = $true }
    
    & "$ScriptDir\publish_crates.ps1" @cratesArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Crates.io publishing failed! Aborting."
        exit $LASTEXITCODE
    }
} else {
    Write-Host "`n[Step 2/3] Crates.io publishing skipped (-PyPIOnly)." -ForegroundColor DarkGray
}

# 4. Publish to PyPI
if (-not $CratesOnly) {
    Write-Host "`n[Step 3/3] Publishing to PyPI..." -ForegroundColor Cyan
    $pypiArgs = @{ Mode = $PyPIMode }
    if ($DryRun) { $pypiArgs["DryRun"] = $true }

    & "$ScriptDir\publish_pypi.ps1" @pypiArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Error "PyPI publishing failed!"
        exit $LASTEXITCODE
    }
} else {
    Write-Host "`n[Step 3/3] PyPI publishing skipped (-CratesOnly)." -ForegroundColor DarkGray
}

Write-Host "`n==========================================================" -ForegroundColor Green
Write-Host "         RELEASE PIPELINE COMPLETED SUCCESSFULLY!         " -ForegroundColor Green
Write-Host "==========================================================" -ForegroundColor Green
