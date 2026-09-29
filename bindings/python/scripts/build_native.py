#!/usr/bin/env python3
"""Builds `bkndb-ffi` for the current platform and refreshes
`bkndb/_native/` with the freshly-generated Python bindings and the
compiled shared library.

Run this after any change to `crates/bkndb-ffi` (or its `bkndb-core`/
`bkndb` dependencies) before testing or building a wheel locally. CI runs
the same script once per target platform to produce each platform's wheel.

Usage:
    python scripts/build_native.py [--release | --debug]
"""
from __future__ import annotations

import argparse
import platform
import shutil
import subprocess
import sys
from pathlib import Path

BINDINGS_PYTHON_DIR = Path(__file__).resolve().parent.parent
WORKSPACE_ROOT = BINDINGS_PYTHON_DIR.parent.parent
NATIVE_DIR = BINDINGS_PYTHON_DIR / "bkndb" / "_native"


def native_lib_filename() -> str:
    """The compiled library's filename, per platform — must match exactly
    what `bkndb/_native/bkndb_ffi.py`'s generated `_uniffi_load_indirect()`
    looks for (it loads `bkndb_ffi.{dll,so,dylib}` next to itself)."""
    system = platform.system()
    if system == "Windows":
        return "bkndb_ffi.dll"
    if system == "Darwin":
        return "libbkndb_ffi.dylib"
    return "libbkndb_ffi.so"


def cargo_target_filename() -> str:
    # Cargo's own cdylib output naming is identical to what the loader
    # expects on every platform this project targets, so this is the same
    # string as `native_lib_filename()` today — kept as a separate function
    # in case a future target (e.g. a cross-compile triple) needs the two
    # to diverge.
    return native_lib_filename()


def run(cmd: list[str], **kwargs) -> None:
    print(f"$ {' '.join(cmd)}")
    subprocess.run(cmd, check=True, cwd=WORKSPACE_ROOT, **kwargs)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--debug",
        action="store_true",
        help="Build the Rust crate in debug mode instead of release (faster, larger, unoptimized).",
    )
    args = parser.parse_args()
    profile_flag = [] if args.debug else ["--release"]
    profile_dir = "debug" if args.debug else "release"

    run(["cargo", "build", "-p", "bkndb-ffi", *profile_flag])

    built_lib = WORKSPACE_ROOT / "target" / profile_dir / cargo_target_filename()
    if not built_lib.exists():
        sys.exit(f"expected build output not found: {built_lib}")

    NATIVE_DIR.mkdir(parents=True, exist_ok=True)

    run(
        [
            "cargo",
            "run",
            "-p",
            "bkndb-ffi",
            "--bin",
            "uniffi-bindgen",
            *profile_flag,
            "--",
            "generate",
            "--library",
            str(built_lib),
            "--language",
            "python",
            "--out-dir",
            str(NATIVE_DIR),
        ]
    )

    dest_lib = NATIVE_DIR / native_lib_filename()
    shutil.copy2(built_lib, dest_lib)
    print(f"Copied {built_lib} -> {dest_lib}")
    print(f"Refreshed {NATIVE_DIR / 'bkndb_ffi.py'}")


if __name__ == "__main__":
    main()
