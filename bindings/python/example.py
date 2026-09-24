"""
Contoh penggunaan BknDb di Python untuk AI Agent & GraphRAG:
- Membuat database persistent (.bkndb) atau in-memory
- Ingesti entitas AI (Prompt, Chunk dokumen, Entitas Graph, Ekstraksi relasi)
- Traversal multi-hop untuk konteks retrieval LLM
"""

from bkndb_ffi import (
    BknDbEngine,
    FfiPropValue,
    FfiNodeInput,
    FfiEdgeInput,
    FfiDirection,
    FfiSyncBatch,
)

def main():
    # 1. Buka database embedded in-memory atau file .bkndb
    # db = BknDbEngine.open("agent_memory.bkndb")
    db = BknDbEngine.in_memory()
    print("BknDb Engine aktif (Embedded, In-Process, Zero Daemon)")

    # 2. Registrasi entitas AI / Dokumen (Nodes)
    doc_node_id = db.create_node(
        "Document",
        {
            "title": FfiPropValue.Str("DeepSeek Architecture Paper"),
            "token_count": FfiPropValue.Int(12500),
            "source": FfiPropValue.Str("arxiv:2401.xxxx"),
        }
    )

    concept_a = db.create_node(
        "Concept",
        {"name": FfiPropValue.Str("Multi-Head Latent Attention")}
    )

    concept_b = db.create_node(
        "Concept",
        {"name": FfiPropValue.Str("KV Cache Compression")}
    )

    # 3. Buat relasi semantik (Edges)
    db.create_edge(doc_node_id, concept_a, "DISCUSSES", {})
    db.create_edge(concept_a, concept_b, "OPTIMIZES", {"impact": FfiPropValue.Str("High")})

    # 4. GraphRAG Traversal: Cari konsep terkait dari dokumen
    neighbors = db.neighbors_out(doc_node_id)
    print(f"\nDokumen #{doc_node_id} terhubung ke {len(neighbors)} konsep:")
    for n in neighbors:
        node = db.get_node(n.target_node_id)
        name = node.props.get("name")
        print(f"  -> [{n.edge_type}] Concept: {name.value if name else 'Unknown'}")

    # 5. BFS Shortest Path untuk Multi-hop Reasoning
    path = db.find_shortest_path(doc_node_id, concept_b, FfiDirection.OUT, ["DISCUSSES", "OPTIMIZES"])
    if path:
        print(f"\nJalur inferensi LLM ditemukan ({path.distance} hops):")
        for step in path.steps:
            print(f"  Step: Node #{step.from_node_id} --({step.edge_type})--> Node #{step.to_node_id}")

    # 6. Atomic Bulk Ingestion (10,000+ entitas sekaligus untuk data scraping / RAG pipeline)
    batch = FfiSyncBatch(
        nodes=[
            FfiNodeInput(label="Entity", props={"name": FfiPropValue.Str(f"Entity_{i}")})
            for i in range(10)
        ],
        edges=[],
    )
    result = db.sync_batch(batch)
    print(f"\nBerhasil ingest massal: {len(result.node_ids)} nodes dalam satu transaksi ACID.")

if __name__ == "__main__":
    main()
