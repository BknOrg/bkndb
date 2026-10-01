# Synchronize Version Across Entire BknDb Repository
# Updates root Cargo.toml, pyproject.toml, and all sub-crate internal dependencies.

param (
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$NewVersion
)

$ErrorActionPreference = "Stop"

# Strip optional leading 'v'
$NewVersion = $NewVersion.TrimStart('v')

if ($NewVersion -notmatch '^\d+\.\d+\.\d+(-[a-zA-Z0-9.]+)?$') {
    Write-Error "Invalid semantic version format: '$NewVersion'. Expected format: X.Y.Z (e.g. 0.2.1, 0.3.0)"
    exit 1
}

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$WorkspaceRoot = Resolve-Path "$ScriptDir\..\.."
Set-Location $WorkspaceRoot

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "         BknDb Version Synchronizer                      " -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "Bumping all components to version: " -NoNewline
Write-Host "v$NewVersion`n" -ForegroundColor Yellow

# Helper to replace regex in file
function Update-FileContent {
    param (
        [string]$RelativePath,
        [scriptblock]$Transform
    )
    $fullPath = "$WorkspaceRoot\$RelativePath"
    if (-not (Test-Path $fullPath)) {
        Write-Warning "File not found: $RelativePath"
        return
    }
    $raw = Get-Content $fullPath -Raw
    $newContent = & $Transform $raw
    if ($raw -ne $newContent) {
        Set-Content -Path $fullPath -Value $newContent -NoNewline
        Write-Host "  [UPDATED] " -ForegroundColor Green -NoNewline
        Write-Host "$RelativePath"
    } else {
        Write-Host "  [NO CHANGE] " -ForegroundColor DarkGray -NoNewline
        Write-Host "$RelativePath"
    }
}

# 1. Cargo.toml (root package version)
Update-FileContent -RelativePath "Cargo.toml" -Transform {
    param($content)
    # Replace package version at the top [workspace.package]
    $content -replace '(?m)(^version\s*=\s*")[^"]+(")', "`${1}$NewVersion`${2}"
}

# 2. bindings/python/pyproject.toml
Update-FileContent -RelativePath "bindings\python\pyproject.toml" -Transform {
    param($content)
    $content -replace '(?m)(^version\s*=\s*")[^"]+(")', "`${1}$NewVersion`${2}"
}

# 3. crates/bkndb/Cargo.toml
Update-FileContent -RelativePath "crates\bkndb\Cargo.toml" -Transform {
    param($content)
    $res = $content -replace '(?m)(bkndb-core\s*=\s*\{\s*version\s*=\s*")[^"]+(")', "`${1}$NewVersion`${2}"
    $res = $res -replace '(?m)(bkndb-storage-redb\s*=\s*\{\s*version\s*=\s*")[^"]+(")', "`${1}$NewVersion`${2}"
    $res = $res -replace '(?m)(bkndb-storage-mem\s*=\s*\{\s*version\s*=\s*")[^"]+(")', "`${1}$NewVersion`${2}"
    $res = $res -replace '(?m)(bkndb-storage-lsm\s*=\s*\{\s*version\s*=\s*")[^"]+(")', "`${1}$NewVersion`${2}"
    return $res
}

# 4. Storage crates (dep on bkndb-core)
$storageCrates = @(
    "crates\bkndb-storage-lsm\Cargo.toml",
    "crates\bkndb-storage-mem\Cargo.toml",
    "crates\bkndb-storage-redb\Cargo.toml"
)
foreach ($sc in $storageCrates) {
    Update-FileContent -RelativePath $sc -Transform {
        param($content)
        $content -replace '(?m)(bkndb-core\s*=\s*\{\s*version\s*=\s*")[^"]+(")', "`${1}$NewVersion`${2}"
    }
}

Write-Host "`nUpdating Cargo.lock to match new workspace version..." -ForegroundColor Cyan
try {
    cargo check --workspace --quiet
    Write-Host "  [OK] Cargo.lock updated successfully" -ForegroundColor Green
} catch {
    Write-Warning "Could not run 'cargo check --workspace'. Please verify Cargo.lock manually."
}

Write-Host ""
# Run validation pre-flight
& "$ScriptDir\check_versions.ps1" -ExpectedVersion $NewVersion
