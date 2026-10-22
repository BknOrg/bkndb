package main

import (
	"fmt"
	"log"
	"os"

	"github.com/BknOrg/bkndb/bindings/go/bkndb"
)

func main() {
	dbPath := "hermes_agent.bkndb"
	defer os.Remove(dbPath)

	fmt.Println("=== Initializing BknDb for Lightweight Hermes Agent ===")

	// 1. Open local persistent embedded database
	db, err := bkndb.Open(dbPath)
	if err != nil {
		log.Fatalf("Failed to open bkndb: %v", err)
	}
	defer db.Close()

	// 2. Setup Session & Message Tables for Agent Memory
	_, err = db.Exec(`
		CREATE TABLE sessions (
			session_id TEXT PRIMARY KEY,
			title TEXT,
			created_at INT
		)
	`)
	if err != nil {
		log.Fatalf("Failed to create sessions table: %v", err)
	}

	_, err = db.Exec(`
		CREATE TABLE messages (
			id INT PRIMARY KEY AUTOINCREMENT,
			session_id TEXT,
			role TEXT,
			content TEXT
		)
	`)
	if err != nil {
		log.Fatalf("Failed to create messages table: %v", err)
	}

	// 3. Insert chat turns
	_, err = db.Exec("INSERT INTO sessions (session_id, title, created_at) VALUES ('sess_01', 'System Optimization', 1700000000)")
	if err != nil {
		log.Fatalf("Failed to insert session: %v", err)
	}

	_, err = db.Exec(`
		INSERT INTO messages (session_id, role, content) VALUES 
		('sess_01', 'user', 'Bagaimana cara membuat hermes agent lebih ringan?'),
		('sess_01', 'assistant', 'Gunakan arsitektur single binary dalam bahasa Go dengan storage embedded BknDb.'),
		('sess_01', 'user', 'Apakah BknDb mendukung vector search dan fulltext search?'),
		('sess_01', 'assistant', 'Ya, BknDb mendukung BM25 fulltext search dan HNSW vector search dalam satu file.')
	`)
	if err != nil {
		log.Fatalf("Failed to insert messages: %v", err)
	}

	// 4. Full-text search (BM25) over agent messages
	if err := db.CreateFulltextIndex("messages", "content"); err != nil {
		log.Fatalf("Failed to create fulltext index: %v", err)
	}

	hits, err := db.SearchText("messages", "content", "vector", 5)
	if err != nil {
		log.Fatalf("Search failed: %v", err)
	}
	fmt.Printf("\n[BM25 Recall] Ditemukan %d pesan relevan untuk keyword 'vector':\n", len(hits))
	for _, h := range hits {
		fmt.Printf(" - (%s): %s [Score: %.2f]\n", h.Values["role"], h.Values["content"], h.Score)
	}

	// 5. Knowledge Graph for Agent Skills & Tools
	agentID, _ := db.CreateNode("Agent", map[string]any{"name": "Hermes-Light", "runtime": "Go"})
	tool1ID, _ := db.CreateNode("Tool", map[string]any{"name": "web_search", "provider": "searxng"})
	tool2ID, _ := db.CreateNode("Tool", map[string]any{"name": "terminal", "provider": "native_exec"})

	_, _ = db.CreateEdge(agentID, "HAS_TOOL", tool1ID, map[string]any{"priority": 1})
	_, _ = db.CreateEdge(agentID, "HAS_TOOL", tool2ID, map[string]any{"priority": 2})

	neighbors, err := db.Neighbors(agentID, bkndb.DirectionOut)
	if err != nil {
		log.Fatalf("Neighbors traversal failed: %v", err)
	}
	fmt.Printf("\n[Knowledge Graph] Agent terhubung dengan %d tools:\n", len(neighbors))
	for _, n := range neighbors {
		toolNode, _ := db.GetNode(n.NodeID)
		fmt.Printf(" - Tool: %s (Provider: %s)\n", toolNode.Properties["name"], toolNode.Properties["provider"])
	}

	fmt.Println("\n=== Semua fitur BknDb berhasil diverifikasi untuk Hermes Agent! ===")
}
