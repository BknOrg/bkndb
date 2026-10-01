# Automated Publishing Script for BknDb to Crates.io
# Enforces exact dependency-tier order with verification & index propagation delays.

param (
    [switch]$DryRun,
    [switch]$AllowDirty,
    [int]$WaitSeconds = 45
)

$ErrorActionPreference = "Stop"

# Navigate to workspace root
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$WorkspaceRoot = Resolve-Path "$ScriptDir\..\.."
Set-Location $WorkspaceRoot

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "       BknDb Crates.io Automated Publisher               " -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

# Read current workspace version from Cargo.toml
$cargoTomlContent = Get-Content "$WorkspaceRoot\Cargo.toml" -Raw
if ($cargoTomlContent -match 'version\s*=\s*"([^"]+)"') {
    $Version = $matches[1]
    Write-Host "Detected Workspace Version: " -NoNewline
    Write-Host "v$Version" -ForegroundColor Yellow
} else {
    Write-Error "Could not detect version in Cargo.toml"
    exit 1
}

if ($DryRun) {
    Write-Host "[DRY-RUN MODE] Commands will validate packaging without uploading to crates.io." -ForegroundColor Magenta
}

# 0. Pre-flight Version Consistency Check
Write-Host "`n[Step 0/3] Checking version consistency across repository..." -ForegroundColor Cyan
& "$ScriptDir\check_versions.ps1" -ExpectedVersion $Version
if ($LASTEXITCODE -ne 0) {
    Write-Error "Version check failed. Aborting publishing."
    exit 1
}

function Publish-Crate {
    param (
        [string]$CrateName,
        [string]$Description
    )

    Write-Host "`n--> Publishing crate: " -NoNewline
    Write-Host "$CrateName" -ForegroundColor Green -NoNewline
    Write-Host " ($Description)..."

    $args = @("publish", "-p", $CrateName)
    if ($DryRun) {
        $args += "--dry-run"
    }
    if ($AllowDirty) {
        $args += "--allow-dirty"
    }

    Write-Host "Running: cargo $($args -join ' ')" -ForegroundColor DarkGray
    & cargo @args

    if ($LASTEXITCODE -ne 0) {
        Write-Error "Failed to publish $CrateName. Exiting."
        exit $LASTEXITCODE
    }

    Write-Host "[OK] Successfully processed $CrateName." -ForegroundColor Green
}

function Wait-Propagation {
    param ([int]$Seconds)
    if ($DryRun) {
        Write-Host "Dry run: skipping index propagation wait." -ForegroundColor DarkGray
        return
    }

    Write-Host "`nWaiting $Seconds seconds for crates.io index propagation..." -ForegroundColor Yellow
    for ($i = $Seconds; $i -gt 0; $i--) {
        Write-Host -NoNewline "`rRemaining: $i s  "
        Start-Sleep -Seconds 1
    }
    Write-Host "`rIndex propagation wait complete!       " -ForegroundColor Green
}

# --- TIER 1: Core Primitives ---
Write-Host "`n[Tier 1/3] Publishing Core Foundation" -ForegroundColor Cyan
Publish-Crate -CrateName "bkndb-core" -Description "Core primitives and interfaces"

Wait-Propagation -Seconds $WaitSeconds

# --- TIER 2: Storage Engines ---
Write-Host "`n[Tier 2/3] Publishing Storage Backends" -ForegroundColor Cyan
Publish-Crate -CrateName "bkndb-storage-mem" -Description "In-memory backend"
Publish-Crate -CrateName "bkndb-storage-lsm" -Description "LSM-Tree persistent storage (.bkndb)"
Publish-Crate -CrateName "bkndb-storage-redb" -Description "Optional redb backend"

Wait-Propagation -Seconds $WaitSeconds

# --- TIER 3: Main Facade ---
Write-Host "`n[Tier 3/3] Publishing Main Facade Crate" -ForegroundColor Cyan
Publish-Crate -CrateName "bkndb" -Description "Top-level embedded database engine"

Write-Host "`n==========================================================" -ForegroundColor Green
Write-Host "   ALL CRATES PUBLISHED SUCCESSFULLY TO CRATES.IO!        " -ForegroundColor Green
Write-Host "==========================================================" -ForegroundColor Green
Write-Host "Verified crates at: https://crates.io/crates/bkndb" -ForegroundColor Cyan
