# bkndb — Python bindings

Database embedded hybrid **graph + relational** untuk Python: satu file `.bkndb`, berjalan di dalam proses (seperti SQLite), transaksi ACID yang mencakup graph dan tabel sekaligus.

## Instalasi

```bash
pip install bkndb                     # wheel siap pakai untuk Windows/Linux/macOS
pip install "bkndb[pandas,networkx]"  # + integrasi pandas & NetworkX
```

Butuh Python 3.10+. Satu wheel per OS/arsitektur berlaku untuk semua versi Python 3.x (lihat [Arsitektur](#arsitektur)).

## Contoh cepat

```python
import bkndb
from bkndb import Agg, Column, TableSchema, col

with bkndb.open("knowledge.bkndb") as db:        # atau bkndb.in_memory()
    # --- Graph ---
    doc = db.create_node("Document", {"title": "Attention Is All You Need"})
    topic = db.create_node("Concept", {"name": "Transformer"})
    db.create_edge(doc, "DISCUSSES", topic)

    # --- Tabel relational (skema tersimpan di dalam database) ---
    db.create_table(TableSchema(
        "chunks",
        [
            Column("id", int),
            Column("doc", int, nullable=False),           # NOT NULL
            Column("text", str),
            Column("tokens", int, default=0),             # DEFAULT
        ],
        primary_key="id",
        auto_increment=True,
        indexes=["doc"],
    ))

    # --- Transaksi atomik lintas graph + tabel ---
    with db.transaction() as tx:
        tx.insert("chunks", {"doc": doc, "text": "…", "tokens": 512})
        tx.update_node(doc, set={"chunked": True})
    # commit otomatis di sini; rollback kalau blok melempar exception

    # --- Query ---
    chunks = db.table("chunks")
    big = chunks.select((col("doc") == doc) & (col("tokens") > 100), order_by="-tokens", limit=10)
    [stats] = db.aggregate("chunks", [Agg.count(), Agg.avg("tokens")])
    per_doc = db.aggregate("chunks", [Agg.sum("tokens")], group_by=["doc"])

    # --- Traversal graph ---
    for hit in db.traverse(doc, max_depth=2):
        print(hit.depth, db.get_node(hit.node_id).label)
```

Nilai properti/kolom cukup tipe Python biasa: `str`, `int`, `float`, `bool`, `bytes`, `None`, `datetime.datetime` (disimpan sebagai mikrodetik UTC; datetime tanpa zona waktu dianggap UTC), `uuid.UUID`, `list`, dan `dict` (kunci `str`). List dan dict boleh bersarang seperti JSON. Tipe kolom: `int`, `float`, `str`, `bool`, `bytes`, `datetime.datetime` (`"timestamp"`), `uuid.UUID` (`"uuid"`), `list`, `dict` (`"map"`). Kolom `timestamp` dan `uuid` bisa jadi primary key atau diindeks; `list`/`dict` tidak.

## Ringkasan API

**Database** — `bkndb.open(path, *, memtable_flush_bytes=None, compaction_trigger_files=None, block_size=None, compression=None)`, `bkndb.in_memory()`, `db.close()` (melepas file lock segera), `db.closed`, `db.compact()` (menulis ulang file hanya berisi data hidup: mengembalikan ruang kosong dan meng-upgrade format lama).

**Graph** — `create_node`, `create_nodes_bulk`, `get_node`, `update_node(id, set=..., unset=[...])`, `delete_node`, `create_edge`, `create_edges_bulk`, `get_edge`, `update_edge`, `delete_edge`, `neighbors(id, Direction.BOTH, edge_type=None)`, `degree`, `traverse(start, direction, max_depth, edge_types, node_label)`, `find_shortest_path` (jumlah hop paling sedikit), `find_weighted_path(start, target, weight="km")` (Dijkstra, biaya terkecil), `top_hubs`, `cascade_delete`.

**Index graph** — `nodes_by_label(label)` dan `count_nodes(label)` memakai index label. `find_nodes(label, property, value)` mencari node berdasarkan nilai properti; setelah `create_node_index(label, property)` pencarian ini menjadi lookup index (hanya nilai `int`/`str` yang diindeks). Juga `drop_node_index`, `node_indexes()`. Untuk file `.bkndb` yang dibuat versi lama, jalankan `db.rebuild_graph_indexes()` sekali — tanpa itu hasil tetap benar, hanya lebih lambat (full scan).

```python
db.create_node_index("Person", "email")
[ana] = db.find_nodes("Person", "email", "ana@example.com")
route = db.find_weighted_path(home, office, weight="minutes")   # route.cost, route.path.node_ids
```

**Tabel** — `create_table(TableSchema)`, `ensure_table(TableSchema)` (migrasi: kolom baru dengan default di-backfill, kolom dihapus dibuang, constraint divalidasi terhadap data lama — atomik), `drop_table`, `create_index`/`drop_index`, `list_tables`, `table_schema`.

**Baris** — `insert`/`insert_many` (error `DuplicateKeyError` kalau PK sudah ada; di tabel auto-increment, PK eksplisit dipakai apa adanya dan PK otomatis hanya diberikan kalau kolom PK tidak diisi), `upsert`/`upsert_many`, `get_row`, `select(table, where, order_by=, limit=, offset=, columns=)`, `count`, `update_rows(table, set, where)`, `delete_rows(table, where)`, `aggregate(table, [Agg...], where, group_by)`. `db.table("x")` memberi handle dengan method yang sama tanpa mengulang nama tabel.

**Filter** — `col("a") == 1`, `!=`, `<`, `<=`, `>`, `>=`, `.is_in([...])`, `.is_null()`, `.is_not_null()`, `.startswith("pre")`, `.between(lo, hi)`, `.contains(x)` (elemen list, substring, atau kunci dict), `.like("a%")` / `.ilike(...)`. Field bersarang bisa diakses dengan `col("meta")["author"]` atau `col("tags")[0]` (sama dengan `col("meta.author")`). Semua filter digabung dengan `&`, `|`, `~` (beri kurung pada tiap perbandingan). `where` juga boleh dict: `{"city": "Jakarta"}`. `order_by="-age"` berarti descending. Query memakai primary key atau index secara otomatis bila memungkinkan.

**Transaksi** — `with db.transaction() as tx:` memberi semua operasi graph/baris di atas. Di dalam transaksi, pembacaan melihat tulisan transaksi itu sendiri. Kalau satu operasi gagal, transaksi dibatalkan (`TransactionAbortedError`) dan harus di-rollback. Hanya satu transaksi terbuka per database; selama terbuka, menulis lewat `db` langsung akan melempar `TransactionInProgressError` (pembacaan tetap jalan).

**Bulk sync** — `db.sync_batch(nodes=[...], edges=[...], rows={"tabel": [...]})` dalam satu transaksi; baris di-*upsert*, jadi batch yang sama aman dijalankan ulang. Ujung edge boleh berupa `bkndb.NewNode(i)` untuk menunjuk node ke-`i` di `nodes` batch yang sama:

```python
db.sync_batch(
    nodes=[("File", {"path": "main.rs"}), ("Function", {"name": "main"})],
    edges=[(repo_id, "CONTAINS", NewNode(0), {}), (NewNode(0), "DEFINES", NewNode(1), {})],
)
```

**SQL** — `db.sql(query, params)` menjalankan satu statement SQL pada tabel relational, dan juga tersedia di transaksi (`tx.sql`). Parameter berupa list untuk `?`, `?N`, `$N`, atau dict untuk `:nama`. Hasilnya `QueryResult` (`.columns`, `.rows`, `.affected`, `.dicts()`, `.scalar()`, `.column(nama)`).

```python
db.sql("CREATE TABLE users (id INT PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL UNIQUE,"
       " age INT DEFAULT 0, joined TIMESTAMP, meta JSON, INDEX (age))")
db.sql("INSERT INTO users (email, age, meta) VALUES (?, ?, ?)", ["ana@x.io", 30, {"team": "core"}])
db.sql("SELECT meta.team AS team, COUNT(*) AS n, AVG(age) FROM users"
       " WHERE joined >= TIMESTAMP '2026-01-01' OR age > :min GROUP BY meta.team ORDER BY n DESC",
       {"min": 18}).dicts()
```

Yang didukung:
- **Query**: `SELECT` (kolom, path `a.b`, alias, `*`, `COUNT/SUM/AVG/MIN/MAX`, `GROUP BY`, `ORDER BY`, `LIMIT/OFFSET`).
- **Tulis**: `INSERT [OR REPLACE]`, `UPSERT`, `UPDATE`, `DELETE`.
- **Skema**: `CREATE/DROP TABLE [IF [NOT] EXISTS]`, `CREATE INDEX … ON t (c)`, `DROP INDEX ON t (c)`, `ALTER TABLE … ADD/DROP COLUMN`.
- **Kondisi WHERE**: perbandingan, `IN`, `IS NULL`, `BETWEEN`, `LIKE`/`ILIKE`, `CONTAINS`, `AND`/`OR`/`NOT`.
- **Literal**: `TIMESTAMP '…'`, `UUID '…'`, `x'…'`, `[list]`, `{map}`.
- **Belum didukung**: JOIN, subquery, dan ekspresi aritmetika.

**Query graph** — `db.graph_query(query, params)` menjalankan subset Cypher `MATCH … WHERE … RETURN …` (juga `tx.graph_query`). Parameternya `$nama` (dict) atau `$1` (list).

```python
db.graph_query(
    "MATCH (a:Person {name: $name})-[:KNOWS*1..2]->(b)-[:WORKS_AT]->(c:Company) "
    "WHERE b.age >= 30 RETURN c.name AS company, count(*) AS n, collect(b.name) AS people ORDER BY n DESC",
    {"name": "Ana"},
).dicts()
```

Yang didukung:
- **Pattern**: satu jalur node/relasi dengan label, tipe (`:A|B`), properti inline, dan arah `->`, `<-`, atau `-`. Panjang variabel `*`, `*2`, `*1..3` (maksimal 16 hop). Edge tidak dipakai dua kali dalam satu match.
- **WHERE**: perbandingan (termasuk antar-variabel), `IN`, `IS NULL`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, `LIKE`, `n:Label`.
- **RETURN**: path properti, `id()`, `label()`, `type()`, agregat `count/sum/avg/min/max/collect`, `DISTINCT`, `ORDER BY`, `SKIP/LIMIT`.
- **Bentuk hasil**: node dikembalikan sebagai `{"id", "label", "properties"}`, edge sebagai `{"id", "type", "from", "to", "properties"}`.
- **Pemilihan node awal** otomatis: `id(n) = …`, lalu properti yang punya index node, lalu index label.

**Full-text search** — `db.create_fulltext_index(table, column)` membuat index BM25 pada kolom teks, dan index itu terus diperbarui oleh setiap insert, update, dan delete. Cari dengan `db.search_text(table, column, "kata lain*", limit=10, match_all=False, where=None)`, yang mengembalikan list `ScoredRow(row, score)`. Tokenisasinya per kata (huruf/angka Unicode, tanpa stemming), jadi netral bahasa. `kata*` berarti pencarian prefix.

**Vector search** — `db.search_vector(table, column, vector, limit=10, metric="cosine"|"dot"|"euclidean", where=None)` melakukan k-NN eksak. Embedding bisa disimpan sebagai `list` angka, atau sebagai `bytes` dari `bkndb.pack_vector(vec)` (float32, 3× lebih hemat). Pencariannya scan linear, cocok sampai ratusan ribu baris, dan belum memakai index ANN.

```python
db.create_fulltext_index("articles", "body")
hits = db.search_text("articles", "body", "graph datab*", where=col("lang") == "id")
db.insert("articles", {"title": "…", "emb": bkndb.pack_vector(model.encode("…"))})
nearest = db.search_vector("articles", "emb", model.encode("query"), limit=5)
```

**Iterasi besar** — `db.iter_rows(table, where, batch_size=1000, columns=None)` mengembalikan baris secara lazy, urut primary key, per batch, sehingga memori tetap kecil untuk tabel sebesar apa pun. `for row in db.table("x")` memakai mekanisme yang sama.

**Import/export** — `db.export_jsonl(table, path, where=None)`, `db.export_csv(...)`, `db.import_jsonl(table, path, mode="upsert"|"insert")`, `db.import_csv(...)`. Import berjalan dalam satu transaksi: kalau satu baris gagal, tidak ada yang tertulis. Di JSONL, `bytes` ditulis sebagai `{"$bytes": "<base64>"}`. Di CSV, sel kosong berarti kolom tidak diisi (berlaku DEFAULT/NULL, dan PK auto-increment dibuatkan), boolean ditulis `true`/`false`, dan bytes dalam base64.

**Operasional** — `db.backup(path)` menulis salinan konsisten (sudah terkompaksi) ke file baru tanpa menghentikan baca/tulis. `db.stats()` mengembalikan jumlah node/edge/baris per tabel, plus ukuran file, jumlah segmen, WAL, dan `reclaimable_bytes` untuk database di disk. `db.verify_integrity()` membaca ulang dan memeriksa checksum semua data; kerusakan dilaporkan sebagai `CorruptionError`.

```python
with bkndb.open("app.bkndb") as db:
    print(db.stats().storage.reclaimable_bytes)
    db.verify_integrity()
    db.backup("backup/app-2026-09-29.bkndb")
    for row in db.iter_rows("events", col("kind") == "click", batch_size=5000):
        ...
```

**Integrasi** — `db.select_df(table, where, ...)` → `pandas.DataFrame` (index = primary key); `db.to_networkx(start, max_depth)` → `networkx.MultiDiGraph`.

**Error** — semua turunan `bkndb.BknDbError`: `NotFoundError`, `TableNotFoundError`, `DuplicateKeyError`, `SchemaMismatchError`, `ConstraintViolationError`, `DatabaseLockedError`, `DatabaseClosedError`, `CorruptionError`, `QueryError` (SQL/graph query tidak valid; juga turunan `ValueError`), `TransactionError` (`…InProgressError`, `…ClosedError`, `…AbortedError`), `InvalidArgumentError` (juga turunan `ValueError`, mis. integer di luar rentang 64-bit), `BackendError`, `EncodingError`, `ReservedTableNameError`.

## Development

```bash
cd bindings/python
python -m venv .venv && . .venv/bin/activate    # Windows: .venv\Scripts\activate
pip install maturin pytest
maturin develop            # compile crate Rust + generate bkndb/_native/, install editable
pytest
```

Ulangi `maturin develop` setiap kali kode Rust (`crates/`) berubah. Perubahan di file Python di `bkndb/` langsung berlaku tanpa build ulang.

Build distribusi lokal: `maturin build --release` (wheel `py3-none-<platform>`) dan `maturin sdist` (source, bisa di-`pip install` di mana saja yang punya toolchain Rust).

## Struktur paket

```
bindings/python/
├── pyproject.toml     # metadata + konfigurasi maturin (bindings = "uniffi")
├── bkndb/             # API publik — hanya dari sini yang boleh di-import
│   ├── __init__.py
│   ├── database.py    # Database, Transaction, Table
│   ├── query.py       # col(), ekspresi filter, Agg
│   ├── types.py       # Node/Edge/Row/TableSchema/... + konversi nilai
│   ├── errors.py      # hierarki exception
│   └── _native/       # PRIVAT, hasil generate maturin — jangan di-import/commit
├── tests/
└── examples/
```

## Arsitektur

Binding ini memakai UniFFI: `bkndb/_native/bkndb_ffi.py` memuat library native lewat `ctypes`, bukan lewat ABI ekstensi C CPython. Karena itu **satu wheel per OS/arsitektur berlaku untuk semua versi Python 3.x** (tag `py3-none-<platform>`), tanpa matriks `cp310`/`cp311`/…. maturin mengurus build crate Rust, generate binding (memakai `uniffi-bindgen` milik crate sendiri, jadi versinya selalu cocok) dan penandaan wheel, termasuk `manylinux` di Linux.

Transaksi eksplisit berjalan di thread native miliknya sendiri, sehingga objek transaksi aman dipakai dari thread Python mana pun.

## Rilis ke PyPI

`.github/workflows/python-wheels.yml` menjalankan, di setiap PR yang menyentuh `crates/**` atau `bindings/python/**`:
- **test** — install dari source + `pytest` di Linux/Windows/macOS × Python 3.10–3.13;
- **wheels** — Linux x86_64/aarch64 (manylinux), Windows x64, macOS x86_64/arm64, plus **sdist**;
- **smoke** — install tiap wheel di environment bersih.

Setup sekali: di PyPI tambahkan [trusted publisher](https://docs.pypi.org/trusted-publishers/) untuk repo `BknOrg/bkndb`, workflow `python-wheels.yml`, environment `pypi`; lalu buat environment `pypi` di pengaturan repo GitHub.

Rilis: naikkan `version` di `pyproject.toml`, lalu push tag `python-v<versi>` (mis. `python-v0.2.0`). Job `publish` hanya jalan setelah semua job di atas lulus, dan memeriksa bahwa tag cocok dengan versi paket.

## Lapisan stabil vs. tidak

- **Stabil (API publik):** semua yang diekspor dari `bkndb` — di-maintain tangan dan menyerap perubahan di sisi Rust.
- **Privat:** `bkndb._native.*` — berubah otomatis mengikuti `crates/bkndb-ffi`. Jangan di-import di kode aplikasi.
