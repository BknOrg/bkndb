"""Contoh penggunaan bkndb di Python untuk AI Agent & GraphRAG, lewat API
publik `bkndb` — bukan modul internal `bkndb._native` mentah.
"""
import bkndb


def main() -> None:
    # 1. Buka database embedded in-memory atau file .bkndb
    # with bkndb.open("agent_memory.bkndb") as db:
    with bkndb.in_memory() as db:
        print("BknDb aktif (Embedded, In-Process, Zero Daemon)")

        # 2. Registrasi entitas AI / Dokumen (Nodes) — properti Python biasa,
        #    tidak perlu FfiPropValue.STR(...)/INT(...) manual.
        doc_id = db.create_node(
            "Document",
            {
                "title": "DeepSeek Architecture Paper",
                "token_count": 12500,
                "source": "arxiv:2401.xxxx",
            },
        )
        concept_a = db.create_node("Concept", {"name": "Multi-Head Latent Attention"})
        concept_b = db.create_node("Concept", {"name": "KV Cache Compression"})

        # 3. Buat relasi semantik (Edges)
        db.create_edge(doc_id, "DISCUSSES", concept_a)
        db.create_edge(concept_a, "OPTIMIZES", concept_b, {"impact": "High"})

        # 4. GraphRAG Traversal: cari konsep terkait dari dokumen
        neighbors = db.neighbors_out(doc_id, "DISCUSSES")
        print(f"\nDokumen #{doc_id} terhubung ke {len(neighbors)} konsep:")
        for n in neighbors:
            node = db.get_node(n.node_id)
            name = node.properties.get("name") if node else None
            print(f"  -> [DISCUSSES] Concept: {name}")

        # 5. BFS shortest path untuk multi-hop reasoning
        path = db.find_shortest_path(doc_id, concept_b, bkndb.Direction.OUT, ["DISCUSSES", "OPTIMIZES"])
        if path:
            print(f"\nJalur inferensi LLM ditemukan ({len(path.edge_ids)} hops):")
            for step in path.steps:
                print(f"  Step: Node #{step.node_id} (via edge: {step.edge_type})")

        # 6. Atomic bulk ingestion (ribuan entitas sekaligus, satu transaksi ACID)
        result = db.sync_batch(nodes=[("Entity", {"name": f"Entity_{i}"}) for i in range(10)])
        print(f"\nBerhasil ingest massal: {len(result.node_ids)} nodes dalam satu transaksi ACID.")


if __name__ == "__main__":
    main()
