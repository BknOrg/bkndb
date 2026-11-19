package bkndb

import (
	"errors"
	"strings"
	"unicode/utf8"
)

// Raw C-ABI text is UTF-8 and NUL-terminated. Check it in the shared facade
// so both drivers reject invalid text instead of executing a truncated prefix.
// JSON-encoded property/parameter values can still contain escaped NUL bytes.
func validateText(values ...string) error {
	for _, s := range values {
		if strings.IndexByte(s, 0) >= 0 {
			return errors.New("text argument contains a NUL byte")
		}
		if !utf8.ValidString(s) {
			return errors.New("text argument is not valid UTF-8")
		}
	}
	return nil
}
