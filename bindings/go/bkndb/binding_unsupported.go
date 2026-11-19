//go:build !bkndb_wasm && (!cgo || !((linux && !android && (amd64 || arm64)) || (darwin && !ios && (amd64 || arm64)) || (windows && amd64)))

package bkndb

// Intentionally fail at compile time instead of leaving defaultDriver nil.
var _ = BknDb_requires_CGO_ENABLED_1_and_linux_amd64_arm64_darwin_amd64_arm64_or_windows_amd64
