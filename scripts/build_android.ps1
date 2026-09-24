# Android Cross-Compilation Script for BknDb UniFFI
# Compliant with Android 15+ 16KB Page Size Alignment

param (
    [string]$Target = "aarch64-linux-android",
    [string]$Profile = "release",
    [string]$OutDir = "bindings/android/jniLibs"
)

Write-Host "=== Building bkndb-ffi for Android ($Target, $Profile) ===" -ForegroundColor Cyan

# Ensure 16KB page size alignment for Android 15+ compliance
$env:RUSTFLAGS = "-C link-arg=-Wl,-z,max-page-size=16384"

# Check if target is installed
$installedTargets = rustup target list --installed
if ($installedTargets -notcontains $Target) {
    Write-Host "Target $Target not found. Installing via rustup..." -ForegroundColor Yellow
    rustup target add $Target
}

# Build cargo command
$cargoArgs = @("build", "-p", "bkndb-ffi", "--target", $Target)
if ($Profile -eq "release") {
    $cargoArgs += "--release"
}

Write-Host "Running: cargo $($cargoArgs -join ' ')" -ForegroundColor Green
cargo @cargoArgs

if ($LASTEXITCODE -ne 0) {
    Write-Error "Cargo build failed for $Target."
    exit $LASTEXITCODE
}

# Determine ABI subfolder
$abi = switch ($Target) {
    "aarch64-linux-android"   { "arm64-v8a" }
    "armv7-linux-androideabi" { "armeabi-v7a" }
    "x86_64-linux-android"    { "x86_64" }
    "i686-linux-android"      { "x86" }
    default                   { $Target }
}

$sourceSo = "target\$Target\$Profile\libbkndb_ffi.so"
$destDir = "$OutDir\$abi"

if (Test-Path $sourceSo) {
    New-Item -ItemType Directory -Force -Path $destDir | Out-Null
    Copy-Item $sourceSo -Destination "$destDir\libbkndb_ffi.so" -Force
    Write-Host "Copied $sourceSo -> $destDir\libbkndb_ffi.so" -ForegroundColor Green
    Write-Host "Android build completed successfully with 16KB page size compliance!" -ForegroundColor Cyan
} else {
    Write-Warning "Built binary not found at $sourceSo"
}
