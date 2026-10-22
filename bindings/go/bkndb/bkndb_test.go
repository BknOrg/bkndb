package bkndb_test

import (
	"path/filepath"
	"testing"

	"github.com/BknOrg/bkndb/bindings/go/bkndb"
)

func TestOpenAndClose(t *testing.T) {
	// 1. InMemory
	db, err := bkndb.InMemory()
	if err != nil {
		t.Fatalf("failed to create in-memory db: %v", err)
	}
	if db.IsClosed() {
		t.Fatal("expected db to be open")
	}
	if err := db.Close(); err != nil {
		t.Fatalf("failed to close in-memory db: %v", err)
	}
	if !db.IsClosed() {
		t.Fatal("expected db to be closed")
	}

	// 2. On-disk in temp directory
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "test.bkndb")

	diskDB, err := bkndb.Open(dbPath)
	if err != nil {
		t.Fatalf("failed to open on-disk db: %v", err)
	}
	if diskDB.IsClosed() {
		t.Fatal("expected disk db to be open")
	}

	stats, err := diskDB.Stats()
	if err != nil {
		t.Fatalf("failed to get stats: %v", err)
	}
	if stats.Nodes != 0 || stats.Edges != 0 {
		t.Fatalf("expected empty db, got nodes=%d, edges=%d", stats.Nodes, stats.Edges)
	}

	if err := diskDB.Compact(); err != nil {
		t.Fatalf("failed to compact db: %v", err)
	}

	if err := diskDB.Close(); err != nil {
		t.Fatalf("failed to close on-disk db: %v", err)
	}
}

func TestSQLOperations(t *testing.T) {
	db, err := bkndb.InMemory()
	if err != nil {
		t.Fatalf("failed to create db: %v", err)
	}
	defer db.Close()

	// DDL
	_, err = db.Exec("CREATE TABLE agents (id INT PRIMARY KEY AUTOINCREMENT, name TEXT, role TEXT, rank INT)")
	if err != nil {
		t.Fatalf("failed to create table: %v", err)
	}

	// Positional insert
	affected, err := db.Exec("INSERT INTO agents (name, role, rank) VALUES ('Hermes', 'planner', 1), ('Athena', 'critic', 2)")
	if err != nil {
		t.Fatalf("failed to insert rows: %v", err)
	}
	if affected != 2 {
		t.Fatalf("expected 2 affected rows, got %d", affected)
	}

	// Query
	res, err := db.SQL("SELECT id, name, role, rank FROM agents ORDER BY id ASC")
	if err != nil {
		t.Fatalf("failed to query rows: %v", err)
	}
	if len(res.Rows) != 2 {
		t.Fatalf("expected 2 rows, got %d", len(res.Rows))
	}
	if res.Columns[1] != "name" || res.Columns[2] != "role" {
		t.Fatalf("unexpected columns: %v", res.Columns)
	}

	// Check first row values
	row0 := res.Rows[0]
	if row0[1] != "Hermes" || row0[2] != "planner" {
		t.Fatalf("unexpected row 0 data: %v", row0)
	}
}

func TestGraphOperations(t *testing.T) {
	db, err := bkndb.InMemory()
	if err != nil {
		t.Fatalf("failed to create db: %v", err)
	}
	defer db.Close()

	// Create nodes
	agentID, err := db.CreateNode("Agent", map[string]any{
		"name":    "HermesLight",
		"version": "1.0",
	})
	if err != nil {
		t.Fatalf("failed to create agent node: %v", err)
	}

	toolID, err := db.CreateNode("Tool", map[string]any{
		"name": "web_search",
		"type": "builtin",
	})
	if err != nil {
		t.Fatalf("failed to create tool node: %v", err)
	}

	// Retrieve node
	node, err := db.GetNode(agentID)
	if err != nil {
		t.Fatalf("failed to get node: %v", err)
	}
	if node == nil || node.Label != "Agent" || node.Properties["name"] != "HermesLight" {
		t.Fatalf("unexpected node content: %+v", node)
	}

	// Create edge
	edgeID, err := db.CreateEdge(agentID, "USES_TOOL", toolID, map[string]any{
		"priority": 1,
	})
	if err != nil {
		t.Fatalf("failed to create edge: %v", err)
	}

	edge, err := db.GetEdge(edgeID)
	if err != nil {
		t.Fatalf("failed to get edge: %v", err)
	}
	if edge == nil || edge.EdgeType != "USES_TOOL" || edge.From != agentID || edge.To != toolID {
		t.Fatalf("unexpected edge content: %+v", edge)
	}

	// Traversal
	neighbors, err := db.Neighbors(agentID, bkndb.DirectionOut)
	if err != nil {
		t.Fatalf("failed to get neighbors: %v", err)
	}
	if len(neighbors) != 1 || neighbors[0].NodeID != toolID || neighbors[0].EdgeType != "USES_TOOL" {
		t.Fatalf("unexpected neighbors: %+v", neighbors)
	}

	// Graph query (MATCH ... RETURN)
	gRes, err := db.GraphQuery("MATCH (a:Agent)-[r:USES_TOOL]->(t:Tool) RETURN a.name, t.name")
	if err != nil {
		t.Fatalf("failed graph query: %v", err)
	}
	if len(gRes.Rows) != 1 {
		t.Fatalf("expected 1 match row, got %d", len(gRes.Rows))
	}

	// Update node
	err = db.UpdateNode(agentID, map[string]any{"status": "active"}, "version")
	if err != nil {
		t.Fatalf("failed to update node: %v", err)
	}
	updatedNode, _ := db.GetNode(agentID)
	if updatedNode.Properties["status"] != "active" || updatedNode.Properties["version"] != nil {
		t.Fatalf("unexpected updated properties: %+v", updatedNode.Properties)
	}

	// Delete edge & node
	deleted, err := db.DeleteEdge(edgeID)
	if err != nil || !deleted {
		t.Fatalf("failed to delete edge: %v", err)
	}
	if err := db.DeleteNode(agentID); err != nil {
		t.Fatalf("failed to delete node: %v", err)
	}
}

func TestFulltextAndVectorSearch(t *testing.T) {
	db, err := bkndb.InMemory()
	if err != nil {
		t.Fatalf("failed to create db: %v", err)
	}
	defer db.Close()

	// Table with text and embedding
	_, err = db.Exec("CREATE TABLE memory (id INT PRIMARY KEY, content TEXT, emb LIST)")
	if err != nil {
		t.Fatalf("failed to create table: %v", err)
	}

	_, err = db.Exec("INSERT INTO memory (id, content, emb) VALUES (1, 'hermes agent persistent memory retrieval', [1.0, 0.0])")
	if err != nil {
		t.Fatalf("failed to insert row 1: %v", err)
	}
	_, err = db.Exec("INSERT INTO memory (id, content, emb) VALUES (2, 'unrelated weather forecast today', [0.0, 1.0])")
	if err != nil {
		t.Fatalf("failed to insert row 2: %v", err)
	}

	// 1. Full-text search (BM25)
	if err := db.CreateFulltextIndex("memory", "content"); err != nil {
		t.Fatalf("failed to create fulltext index: %v", err)
	}
	hits, err := db.SearchText("memory", "content", "hermes", 10)
	if err != nil {
		t.Fatalf("failed to search text: %v", err)
	}
	if len(hits) == 0 {
		t.Fatal("expected at least 1 BM25 hit")
	}

	// 2. Vector search (Cosine)
	if err := db.CreateVectorIndex("memory", "emb", bkndb.MetricCosine, 8, 32); err != nil {
		t.Fatalf("failed to create vector index: %v", err)
	}
	vecHits, err := db.SearchVector("memory", "emb", []float32{1.0, 0.1}, 1, bkndb.MetricCosine)
	if err != nil {
		t.Fatalf("failed to search vector: %v", err)
	}
	if len(vecHits) != 1 {
		t.Fatalf("expected 1 vector hit, got %d", len(vecHits))
	}
}

func TestTransactions(t *testing.T) {
	db, err := bkndb.InMemory()
	if err != nil {
		t.Fatalf("failed to create db: %v", err)
	}
	defer db.Close()

	_, err = db.Exec("CREATE TABLE items (id INT PRIMARY KEY, title TEXT)")
	if err != nil {
		t.Fatalf("failed to create table: %v", err)
	}

	// Rollback test
	tx, err := db.Begin()
	if err != nil {
		t.Fatalf("failed to begin transaction: %v", err)
	}
	_, err = tx.Exec("INSERT INTO items (id, title) VALUES (1, 'Ephemeral')")
	if err != nil {
		t.Fatalf("tx insert failed: %v", err)
	}
	if err := tx.Rollback(); err != nil {
		t.Fatalf("tx rollback failed: %v", err)
	}

	res, err := db.SQL("SELECT COUNT(*) FROM items")
	if err != nil {
		t.Fatalf("query failed: %v", err)
	}
	if count, ok := res.Rows[0][0].(float64); !ok || count != 0 {
		t.Fatalf("expected count 0 after rollback, got %v", res.Rows[0][0])
	}

	// Commit test
	tx2, err := db.Begin()
	if err != nil {
		t.Fatalf("failed to begin tx2: %v", err)
	}
	_, err = tx2.Exec("INSERT INTO items (id, title) VALUES (2, 'Persistent')")
	if err != nil {
		t.Fatalf("tx2 insert failed: %v", err)
	}
	if err := tx2.Commit(); err != nil {
		t.Fatalf("tx2 commit failed: %v", err)
	}

	res2, err := db.SQL("SELECT COUNT(*) FROM items")
	if err != nil {
		t.Fatalf("query failed: %v", err)
	}
	if count, ok := res2.Rows[0][0].(float64); !ok || count != 1 {
		t.Fatalf("expected count 1 after commit, got %v", res2.Rows[0][0])
	}
}
