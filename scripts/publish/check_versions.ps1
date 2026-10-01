# Version Consistency Checker for BknDb
# Verifies that root Cargo.toml, pyproject.toml, and all sub-crate internal dependencies
# are 100% synchronized before publishing.

param (
    [string]$ExpectedVersion = ""
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$WorkspaceRoot = Resolve-Path "$ScriptDir\..\.."
Set-Location $WorkspaceRoot

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "         BknDb Version Consistency Pre-flight           " -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

# 1. Determine Target Version
$rootCargo = Get-Content "$WorkspaceRoot\Cargo.toml" -Raw
if ($rootCargo -match 'version\s*=\s*"([^"]+)"') {
    $RootVersion = $matches[1]
} else {
    Write-Error "Could not read version from root Cargo.toml"
    exit 1
}

$TargetVersion = if ($ExpectedVersion) { $ExpectedVersion } else { $RootVersion }
Write-Host "Target Version to Validate : " -NoNewline
Write-Host "v$TargetVersion`n" -ForegroundColor Yellow

$mismatches = @()

# Helper function to check a file
function Check-Version {
    param (
        [string]$FilePath,
        [string]$Pattern,
        [string]$Description
    )

    $fullPath = "$WorkspaceRoot\$FilePath"
    if (-not (Test-Path $fullPath)) {
        Write-Warning "File not found: $FilePath"
        return
    }

    $content = Get-Content $fullPath -Raw
    if ($content -match $Pattern) {
        $foundVer = $matches[1]
        if ($foundVer -eq $TargetVersion) {
            Write-Host "  [OK] " -ForegroundColor Green -NoNewline
            Write-Host "$FilePath " -NoNewline
            Write-Host "(${Description}: v$foundVer)" -ForegroundColor DarkGray
        } else {
            Write-Host "  [MISMATCH] " -ForegroundColor Red -NoNewline
            Write-Host "$FilePath" -ForegroundColor Yellow
            Write-Host "             Expected: v$TargetVersion, Found: v$foundVer ($Description)" -ForegroundColor Red
            $script:mismatches += [PSCustomObject]@{
                File = $FilePath
                Expected = $TargetVersion
                Found = $foundVer
                Description = $Description
            }
        }
    } else {
        Write-Host "  [WARNING] " -ForegroundColor Yellow -NoNewline
        Write-Host "$FilePath (Pattern '$Description' not found)" -ForegroundColor Yellow
        $script:mismatches += [PSCustomObject]@{
            File = $FilePath
            Expected = $TargetVersion
            Found = "NOT_FOUND"
            Description = $Description
        }
    }
}

# --- Checks ---
# 1. Root Cargo.toml
Check-Version -FilePath "Cargo.toml" -Pattern 'version\s*=\s*"([^"]+)"' -Description "workspace.package.version"

# 2. Python pyproject.toml
Check-Version -FilePath "bindings\python\pyproject.toml" -Pattern 'version\s*=\s*"([^"]+)"' -Description "project.version"

# 3. Main facade crate internal dependencies
Check-Version -FilePath "crates\bkndb\Cargo.toml" -Pattern 'bkndb-core\s*=\s*\{\s*version\s*=\s*"([^"]+)"' -Description "dep:bkndb-core"
Check-Version -FilePath "crates\bkndb\Cargo.toml" -Pattern 'bkndb-storage-redb\s*=\s*\{\s*version\s*=\s*"([^"]+)"' -Description "dep:bkndb-storage-redb"
Check-Version -FilePath "crates\bkndb\Cargo.toml" -Pattern 'bkndb-storage-mem\s*=\s*\{\s*version\s*=\s*"([^"]+)"' -Description "dep:bkndb-storage-mem"
Check-Version -FilePath "crates\bkndb\Cargo.toml" -Pattern 'bkndb-storage-lsm\s*=\s*\{\s*version\s*=\s*"([^"]+)"' -Description "dep:bkndb-storage-lsm"

# 4. Storage crates internal dependencies to bkndb-core
Check-Version -FilePath "crates\bkndb-storage-lsm\Cargo.toml" -Pattern 'bkndb-core\s*=\s*\{\s*version\s*=\s*"([^"]+)"' -Description "dep:bkndb-core"
Check-Version -FilePath "crates\bkndb-storage-mem\Cargo.toml" -Pattern 'bkndb-core\s*=\s*\{\s*version\s*=\s*"([^"]+)"' -Description "dep:bkndb-core"
Check-Version -FilePath "crates\bkndb-storage-redb\Cargo.toml" -Pattern 'bkndb-core\s*=\s*\{\s*version\s*=\s*"([^"]+)"' -Description "dep:bkndb-core"

Write-Host ""
if ($mismatches.Count -gt 0) {
    Write-Host "==========================================================" -ForegroundColor Red
    Write-Host "  VERSION MISMATCH DETECTED IN $($mismatches.Count) LOCATION(S)! " -ForegroundColor Red
    Write-Host "==========================================================" -ForegroundColor Red
    Write-Host "Please update the following files before publishing:" -ForegroundColor Yellow
    foreach ($m in $mismatches) {
        Write-Host "  * $($m.File) -> $($m.Description) is currently v$($m.Found) (needs v$($m.Expected))" -ForegroundColor Red
    }
    Write-Host "`nTip: You can synchronize all versions automatically with:" -ForegroundColor Cyan
    Write-Host "     .\scripts\publish\set_version.ps1 $TargetVersion" -ForegroundColor Yellow
    exit 1
} else {
    Write-Host "==========================================================" -ForegroundColor Green
    Write-Host "  ALL PROJECT VERSIONS ARE 100% SYNCHRONIZED (v$TargetVersion)!  " -ForegroundColor Green
    Write-Host "==========================================================" -ForegroundColor Green
    exit 0
}
