# BknDb Python Bindings

Embedded Hybrid Graph & Relational Database Engine for Python (via UniFFI).

## Fitur Utama untuk Python & AI/ML
1. **Embedded / In-Process:** Berjalan langsung di dalam proses Python Anda (seperti SQLite). Tanpa server terpisah, tanpa Docker, tanpa daemon background.
2. **GraphRAG & Knowledge Retrieval:** Traversal relasi multi-hop (BFS Shortest Path, Top Hubs Centrality) dalam mikrodetik untuk memperkaya prompt LLM.
3. **High Throughput Bulk Ingest:** Mampu meng-ingest lebih dari 500,000 nodes/edges per detik ke format single-file `.bkndb`.
4. **Single-file Database (`.bkndb`):** Mudah didistribusikan bersama model AI atau workspace data.

## Struktur File
- `bkndb_ffi.py`: Binding Python yang di-generate via UniFFI.
- `bkndb_ffi.dll`: Compiled dynamic library (Windows x64). *(Untuk Linux gunakan `libbkndb_ffi.so`, untuk macOS gunakan `libbkndb_ffi.dylib`)*.
- `example.py`: Contoh kode penggunaan untuk AI Agent & GraphRAG.

## Contoh Penggunaan Cepat

```python
from bkndb_ffi import BknDbEngine, FfiPropValue, FfiDirection

# Buka database embedded
db = BknDbEngine.open("my_knowledge.bkndb")
# atau in-memory:
# db = BknDbEngine.in_memory()

# Buat entitas & relasi
n1 = db.create_node("Prompt", {"text": FfiPropValue.Str("Analisis codebase")})
n2 = db.create_node("Tool", {"name": FfiPropValue.Str("Linter")})
db.create_edge(n1, n2, "USES", {})

# Traversal graf cepat
neighbors = db.neighbors_out(n1)
print(neighbors)
```
