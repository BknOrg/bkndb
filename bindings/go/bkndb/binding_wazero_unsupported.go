//go:build bkndb_wasm && !((linux && !android && (amd64 || arm64)) || (darwin && !ios && (amd64 || arm64)) || (windows && amd64))

package bkndb

var _ = BknDb_WASM_requires_linux_amd64_arm64_darwin_amd64_arm64_or_windows_amd64
