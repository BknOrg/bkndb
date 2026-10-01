# Automated Publishing Script for BknDb to PyPI
# Supports two modes:
# 1. 'GitHubTag' (Recommended): Tags git with 'python-vX.Y.Z' and pushes to GitHub.
#    The GitHub Actions CI/CD automatically builds cross-platform wheels (Linux, Windows, macOS)
#    and publishes to PyPI via Trusted Publishing with 0 manual token management.
# 2. 'Local': Builds sdist & current platform wheel locally using maturin and uploads directly.

param (
    [ValidateSet("GitHubTag", "Local")]
    [string]$Mode = "GitHubTag",
    [switch]$DryRun,
    [switch]$TestPyPI,
    [string]$Username = "__token__",
    [string]$Password = ""
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$WorkspaceRoot = Resolve-Path "$ScriptDir\..\.."
$PythonDir = "$WorkspaceRoot\bindings\python"

Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "         BknDb PyPI Automated Publisher                  " -ForegroundColor Cyan
Write-Host "==========================================================" -ForegroundColor Cyan

# Read current python package version from pyproject.toml
$pyprojectContent = Get-Content "$PythonDir\pyproject.toml" -Raw
if ($pyprojectContent -match 'version\s*=\s*"([^"]+)"') {
    $Version = $matches[1]
    Write-Host "Detected Python Package Version: " -NoNewline
    Write-Host "v$Version" -ForegroundColor Yellow
} else {
    Write-Error "Could not detect version in bindings/python/pyproject.toml"
    exit 1
}

Write-Host "Selected Publishing Mode: " -NoNewline
Write-Host "$Mode" -ForegroundColor Magenta

# 0. Pre-flight Version Consistency Check
Write-Host "`nChecking version consistency across repository..." -ForegroundColor Cyan
& "$ScriptDir\check_versions.ps1" -ExpectedVersion $Version
if ($LASTEXITCODE -ne 0) {
    Write-Error "Version check failed. Aborting PyPI publishing."
    exit 1
}

if ($Mode -eq "GitHubTag") {
    $TagName = "python-v$Version"
    Write-Host "`n[GitHub Tag Release Flow]" -ForegroundColor Cyan
    Write-Host "This triggers .github/workflows/python-wheels.yml to compile native wheels for:"
    Write-Host "  * Windows x64" -ForegroundColor Gray
    Write-Host "  * Linux x86_64 & aarch64 (manylinux)" -ForegroundColor Gray
    Write-Host "  * macOS Intel & Apple Silicon (arm64)" -ForegroundColor Gray
    Write-Host "  * Universal Source Distribution (sdist)" -ForegroundColor Gray

    if ($DryRun) {
        Write-Host "`n[DRY-RUN] Would create and push tag: $TagName" -ForegroundColor Magenta
        exit 0
    }

    Set-Location $WorkspaceRoot

    # Check if tag already exists
    $existingTags = git tag --list $TagName
    if ($existingTags -contains $TagName) {
        Write-Warning "Git tag '$TagName' already exists locally."
        $confirm = Read-Host "Do you want to force recreate and push tag '$TagName'? (y/N)"
        if ($confirm -ne 'y' -and $confirm -ne 'Y') {
            Write-Host "Aborted by user."
            exit 0
        }
        git tag -d $TagName
        git push origin ":refs/tags/$TagName" 2>$null
    }

    Write-Host "`nCreating Git Tag: $TagName" -ForegroundColor Green
    git tag -a $TagName -m "Release Python bindings v$Version to PyPI"
    
    Write-Host "Pushing Tag to GitHub..." -ForegroundColor Green
    git push origin $TagName

    Write-Host "`n[SUCCESS] Tag $TagName pushed to GitHub!" -ForegroundColor Green
    Write-Host "Track the automated PyPI build & publish progress at:" -ForegroundColor Cyan
    Write-Host "https://github.com/BknOrg/bkndb/actions" -ForegroundColor Yellow
    exit 0
}

if ($Mode -eq "Local") {
    Write-Host "`n[Local Build & Upload Flow]" -ForegroundColor Cyan
    Set-Location $PythonDir

    # Check if maturin is installed
    $maturinCheck = Get-Command maturin -ErrorAction SilentlyContinue
    if (-not $maturinCheck) {
        Write-Host "Maturin not found on PATH. Attempting to run via 'python -m maturin'..." -ForegroundColor Yellow
        $maturinCmd = "python"
        $maturinPrefix = @("-m", "maturin")
    } else {
        $maturinCmd = "maturin"
        $maturinPrefix = @()
    }

    if ($DryRun) {
        Write-Host "`n[DRY-RUN] Building local wheel and sdist with maturin (no upload)..." -ForegroundColor Magenta
        $buildArgs = $maturinPrefix + @("build", "--release", "--sdist", "--out", "dist")
        & $maturinCmd @buildArgs
        Write-Host "`n[OK] Built artifacts successfully in $PythonDir\dist" -ForegroundColor Green
        exit 0
    }

    Write-Host "`nPublishing directly to PyPI using maturin..." -ForegroundColor Cyan
    $publishArgs = $maturinPrefix + @("publish")
    
    if ($TestPyPI) {
        Write-Host "Target Registry: TestPyPI" -ForegroundColor Yellow
        $publishArgs += @("--repository", "testpypi")
    } else {
        Write-Host "Target Registry: Production PyPI" -ForegroundColor Green
    }

    if ($Password) {
        $publishArgs += @("-u", $Username, "-p", $Password)
    }

    Write-Host "Executing: $maturinCmd $($publishArgs -join ' ')" -ForegroundColor DarkGray
    & $maturinCmd @publishArgs

    if ($LASTEXITCODE -ne 0) {
        Write-Error "Maturin publish failed."
        exit $LASTEXITCODE
    }

    Write-Host "`n[SUCCESS] Published Python package v$Version to PyPI!" -ForegroundColor Green
    Write-Host "Inspect at: https://pypi.org/project/bkndb/$Version/" -ForegroundColor Cyan
}
