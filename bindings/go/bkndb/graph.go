package bkndb

import (
	"encoding/json"
	"errors"
)

// CreateNode creates a graph node with a label and optional properties.
func (db *DB) CreateNode(label string, props map[string]any) (uint64, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return 0, errors.New("database is closed")
	}

	var propsJSON string
	if len(props) > 0 {
		b, err := json.Marshal(props)
		if err != nil {
			return 0, err
		}
		propsJSON = string(b)
	}
	return defaultDriver.createNode(db.handle, label, propsJSON)
}

// GetNode retrieves a graph node by its numeric ID. Returns nil, nil if not found.
func (db *DB) GetNode(id uint64) (*Node, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}

	raw, err := defaultDriver.getNode(db.handle, id)
	if err != nil {
		return nil, err
	}
	if raw == "" || raw == "null" {
		return nil, nil
	}
	var node Node
	if err := json.Unmarshal([]byte(raw), &node); err != nil {
		return nil, err
	}
	return &node, nil
}

// DeleteNode deletes a node and all incident edges atomically.
func (db *DB) DeleteNode(id uint64) error {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return errors.New("database is closed")
	}
	return defaultDriver.deleteNode(db.handle, id)
}

// UpdateNode adds/updates properties in setProps and deletes keys in unsetKeys.
func (db *DB) UpdateNode(id uint64, setProps map[string]any, unsetKeys ...string) error {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return errors.New("database is closed")
	}

	var setJSON, unsetJSON string
	if len(setProps) > 0 {
		b, err := json.Marshal(setProps)
		if err != nil {
			return err
		}
		setJSON = string(b)
	}
	if len(unsetKeys) > 0 {
		b, err := json.Marshal(unsetKeys)
		if err != nil {
			return err
		}
		unsetJSON = string(b)
	}
	return defaultDriver.updateNode(db.handle, id, setJSON, unsetJSON)
}

// CreateEdge creates a directed edge between two existing nodes.
func (db *DB) CreateEdge(from uint64, edgeType string, to uint64, props map[string]any) (uint64, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return 0, errors.New("database is closed")
	}

	var propsJSON string
	if len(props) > 0 {
		b, err := json.Marshal(props)
		if err != nil {
			return 0, err
		}
		propsJSON = string(b)
	}
	return defaultDriver.createEdge(db.handle, from, edgeType, to, propsJSON)
}

// GetEdge retrieves an edge by its numeric ID. Returns nil, nil if not found.
func (db *DB) GetEdge(id uint64) (*Edge, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}

	raw, err := defaultDriver.getEdge(db.handle, id)
	if err != nil {
		return nil, err
	}
	if raw == "" || raw == "null" {
		return nil, nil
	}
	var edge Edge
	if err := json.Unmarshal([]byte(raw), &edge); err != nil {
		return nil, err
	}
	return &edge, nil
}

// DeleteEdge deletes an edge by its ID. Returns whether the edge existed.
func (db *DB) DeleteEdge(id uint64) (bool, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return false, errors.New("database is closed")
	}
	return defaultDriver.deleteEdge(db.handle, id)
}

// Neighbors returns adjacent nodes connected to nodeID in direction dir.
// An optional edgeType filter can be provided.
func (db *DB) Neighbors(nodeID uint64, dir Direction, edgeType ...string) ([]Neighbor, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}

	var filterType string
	if len(edgeType) > 0 {
		filterType = edgeType[0]
	}
	raw, err := defaultDriver.neighbors(db.handle, nodeID, int(dir), filterType)
	if err != nil {
		return nil, err
	}
	var list []Neighbor
	if err := json.Unmarshal([]byte(raw), &list); err != nil {
		return nil, err
	}
	return list, nil
}

// GraphQuery runs a graph MATCH ... RETURN query against a database snapshot.
func (db *DB) GraphQuery(query string, params ...any) (*QueryResult, error) {
	db.mu.RLock()
	defer db.mu.RUnlock()
	if db.closed || db.handle == 0 {
		return nil, errors.New("database is closed")
	}

	var paramsJSON string
	if len(params) == 1 {
		if m, ok := params[0].(map[string]any); ok {
			b, err := json.Marshal(m)
			if err != nil {
				return nil, err
			}
			paramsJSON = string(b)
		} else {
			b, err := json.Marshal(params)
			if err != nil {
				return nil, err
			}
			paramsJSON = string(b)
		}
	} else if len(params) > 1 {
		b, err := json.Marshal(params)
		if err != nil {
			return nil, err
		}
		paramsJSON = string(b)
	}

	raw, err := defaultDriver.graphQuery(db.handle, query, paramsJSON)
	if err != nil {
		return nil, err
	}
	var res QueryResult
	if err := json.Unmarshal([]byte(raw), &res); err != nil {
		return nil, err
	}
	return &res, nil
}
