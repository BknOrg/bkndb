package bkndb

// Direction specifies traversal direction along graph edges.
type Direction int

const (
	// DirectionOut traverses edges originating from the node.
	DirectionOut Direction = 0
	// DirectionIn traverses edges pointing to the node.
	DirectionIn Direction = 1
	// DirectionBoth traverses edges in either direction.
	DirectionBoth Direction = 2
)

// VectorMetric specifies the distance metric for vector embeddings.
type VectorMetric string

const (
	// MetricCosine uses cosine similarity (highest score first).
	MetricCosine VectorMetric = "cosine"
	// MetricEuclidean uses Euclidean distance (lowest score first).
	MetricEuclidean VectorMetric = "euclidean"
	// MetricDot uses dot product similarity (highest score first).
	MetricDot VectorMetric = "dot"
)

// Node represents a graph node.
type Node struct {
	ID         uint64         `json:"id"`
	Label      string         `json:"label"`
	Properties map[string]any `json:"properties"`
}

// Edge represents a directed relationship between two nodes.
type Edge struct {
	ID         uint64         `json:"id"`
	From       uint64         `json:"from"`
	To         uint64         `json:"to"`
	EdgeType   string         `json:"edge_type"`
	Properties map[string]any `json:"properties"`
}

// Neighbor represents a connected adjacent node and edge.
type Neighbor struct {
	NodeID   uint64 `json:"node_id"`
	EdgeID   uint64 `json:"edge_id"`
	EdgeType string `json:"edge_type"`
}

// ScoredRow represents a search hit with its BM25 or vector similarity score.
type ScoredRow struct {
	PK     any            `json:"pk"`
	Values map[string]any `json:"values"`
	Score  float64        `json:"score"`
}

// QueryResult represents the tabular result of a SQL or graph query.
type QueryResult struct {
	Columns  []string `json:"columns"`
	Rows     [][]any  `json:"rows"`
	Affected uint64   `json:"affected"`
}

// TableCount represents the row count of one table.
type TableCount struct {
	Table string `json:"table"`
	Rows  uint64 `json:"rows"`
}

// StorageStats contains physical storage statistics for on-disk databases.
type StorageStats struct {
	FileBytes uint64 `json:"file_bytes"`
}

// DbStats contains high-level logical and physical metrics for the database.
type DbStats struct {
	Nodes   uint64        `json:"nodes"`
	Edges   uint64        `json:"edges"`
	Tables  []TableCount  `json:"tables"`
	Storage *StorageStats `json:"storage"`
}
