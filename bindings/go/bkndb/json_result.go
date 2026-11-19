package bkndb

import (
	"bytes"
	"encoding/json"
	"errors"
	"strings"
)

// Preserve integer precision in interface-valued fields. Small numbers retain
// the SDK's existing float64 representation; large integers use json.Number.
func normalizeValue(v any) any {
	switch x := v.(type) {
	case json.Number:
		if strings.ContainsAny(x.String(), ".eE") {
			if f, e := x.Float64(); e == nil {
				return f
			}
			return x
		}
		if i, e := x.Int64(); e == nil && i >= -(1<<53) && i <= 1<<53 {
			return float64(i)
		}
		return x
	case []any:
		for i := range x {
			x[i] = normalizeValue(x[i])
		}
		return x
	case map[string]any:
		for k, v := range x {
			x[k] = normalizeValue(v)
		}
		return x
	default:
		return v
	}
}
func decodeResult(raw []byte, out any) error {
	d := json.NewDecoder(bytes.NewReader(raw))
	d.UseNumber()
	if e := d.Decode(out); e != nil {
		return e
	}
	if len(bytes.TrimSpace(raw[d.InputOffset():])) != 0 {
		return errors.New("unexpected trailing JSON data")
	}
	switch x := out.(type) {
	case *QueryResult:
		for i := range x.Rows {
			for j := range x.Rows[i] {
				x.Rows[i][j] = normalizeValue(x.Rows[i][j])
			}
		}
	case *Node:
		normalizeValue(x.Properties)
	case *Edge:
		normalizeValue(x.Properties)
	case *[]ScoredRow:
		for i := range *x {
			(*x)[i].PK = normalizeValue((*x)[i].PK)
			normalizeValue((*x)[i].Values)
		}
	}
	return nil
}
