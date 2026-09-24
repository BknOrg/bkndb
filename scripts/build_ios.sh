#!/usr/bin/env bash
set -euo pipefail

# iOS Build and XCFramework packaging script for BknDb UniFFI
# Produces static library libbkndb_ffi.a and packages into BknDb.xcframework

PROFILE="${1:-release}"
OUT_DIR="${2:-bindings/ios}"
CARGO_FLAGS=""

if [ "$PROFILE" = "release" ]; then
    CARGO_FLAGS="--release"
fi

echo "=== Building bkndb-ffi for iOS targets ($PROFILE) ==="

# iOS targets: Device (arm64) and Simulator (arm64 + x86_64)
TARGET_DEVICE="aarch64-apple-ios"
TARGET_SIM_ARM="aarch64-apple-ios-sim"
TARGET_SIM_X86="x86_64-apple-ios"

rustup target add "$TARGET_DEVICE" "$TARGET_SIM_ARM" "$TARGET_SIM_X86" || true

echo "Building for device ($TARGET_DEVICE)..."
cargo build -p bkndb-ffi --target "$TARGET_DEVICE" $CARGO_FLAGS

echo "Building for simulator arm64 ($TARGET_SIM_ARM)..."
cargo build -p bkndb-ffi --target "$TARGET_SIM_ARM" $CARGO_FLAGS

echo "Building for simulator x86_64 ($TARGET_SIM_X86)..."
cargo build -p bkndb-ffi --target "$TARGET_SIM_X86" $CARGO_FLAGS

# Lipod simulator static library
SIM_FAT_DIR="target/ios-sim-fat/$PROFILE"
mkdir -p "$SIM_FAT_DIR"
lipo -create \
    "target/$TARGET_SIM_ARM/$PROFILE/libbkndb_ffi.a" \
    "target/$TARGET_SIM_X86/$PROFILE/libbkndb_ffi.a" \
    -output "$SIM_FAT_DIR/libbkndb_ffi.a"

# Package XCFramework
XCFRAMEWORK_DIR="$OUT_DIR/BknDb.xcframework"
rm -rf "$XCFRAMEWORK_DIR"
mkdir -p "$OUT_DIR"

xcodebuild -create-xcframework \
    -library "target/$TARGET_DEVICE/$PROFILE/libbkndb_ffi.a" \
    -headers "bindings/swift" \
    -library "$SIM_FAT_DIR/libbkndb_ffi.a" \
    -headers "bindings/swift" \
    -output "$XCFRAMEWORK_DIR"

echo "=== iOS XCFramework successfully packaged at $XCFRAMEWORK_DIR ==="
