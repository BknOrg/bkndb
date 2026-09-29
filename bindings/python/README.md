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

Nilai properti/kolom cukup tipe Python biasa: `str`, `int`, `float`, `bool`, `bytes`, `None`.

## Ringkasan API

**Database** — `bkndb.open(path, *, memtable_flush_bytes=None, compaction_trigger_files=None)`, `bkndb.in_memory()`, `db.close()` (melepas file lock segera), `db.closed`, `db.compact()`.

**Graph** — `create_node`, `create_nodes_bulk`, `get_node`, `update_node(id, set=..., unset=[...])`, `delete_node`, `create_edge`, `create_edges_bulk`, `get_edge`, `update_edge`, `delete_edge`, `neighbors(id, Direction.BOTH, edge_type=None)`, `degree`, `traverse(start, direction, max_depth, edge_types, node_label)`, `find_shortest_path`, `top_hubs`, `cascade_delete`.

**Tabel** — `create_table(TableSchema)`, `ensure_table(TableSchema)` (migrasi: kolom baru dengan default di-backfill, kolom dihapus dibuang, constraint divalidasi terhadap data lama — atomik), `drop_table`, `create_index`/`drop_index`, `list_tables`, `table_schema`.

**Baris** — `insert`/`insert_many` (error `DuplicateKeyError` kalau PK sudah ada), `upsert`/`upsert_many`, `get_row`, `select(table, where, order_by=, limit=, offset=, columns=)`, `count`, `update_rows(table, set, where)`, `delete_rows(table, where)`, `aggregate(table, [Agg...], where, group_by)`. `db.table("x")` memberi handle dengan method yang sama tanpa mengulang nama tabel.

**Filter** — `col("a") == 1`, `!=`, `<`, `<=`, `>`, `>=`, `.is_in([...])`, `.is_null()`, `.is_not_null()`, `.startswith("pre")`, `.between(lo, hi)`, digabung dengan `&`, `|`, `~` (beri kurung pada tiap perbandingan). `where` juga boleh dict: `{"city": "Jakarta"}`. `order_by="-age"` berarti descending. Query memakai primary key atau index secara otomatis bila memungkinkan.

**Transaksi** — `with db.transaction() as tx:` memberi semua operasi graph/baris di atas. Di dalam transaksi, pembacaan melihat tulisan transaksi itu sendiri. Kalau satu operasi gagal, transaksi dibatalkan (`TransactionAbortedError`) dan harus di-rollback. Hanya satu transaksi terbuka per database; selama terbuka, menulis lewat `db` langsung akan melempar `TransactionInProgressError` (pembacaan tetap jalan).

**Bulk sync** — `db.sync_batch(nodes=[...], edges=[...], rows={"tabel": [...]})` dalam satu transaksi; baris di-*upsert*, jadi batch yang sama aman dijalankan ulang.

**Integrasi** — `db.select_df(table, where, ...)` → `pandas.DataFrame` (index = primary key); `db.to_networkx(start, max_depth)` → `networkx.MultiDiGraph`.

**Error** — semua turunan `bkndb.BknDbError`: `NotFoundError`, `TableNotFoundError`, `DuplicateKeyError`, `SchemaMismatchError`, `ConstraintViolationError`, `DatabaseLockedError`, `DatabaseClosedError`, `TransactionError` (`…InProgressError`, `…ClosedError`, `…AbortedError`), `InvalidArgumentError` (juga turunan `ValueError`, mis. integer di luar rentang 64-bit), `BackendError`, `EncodingError`, `ReservedTableNameError`.

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

Setup sekali: di PyPI tambahkan [trusted publisher](https://docs.pypi.org/trusted-publishers/) untuk repo `BknOrg/bkn-db`, workflow `python-wheels.yml`, environment `pypi`; lalu buat environment `pypi` di pengaturan repo GitHub.

Rilis: naikkan `version` di `pyproject.toml`, lalu push tag `python-v<versi>` (mis. `python-v0.2.0`). Job `publish` hanya jalan setelah semua job di atas lulus, dan memeriksa bahwa tag cocok dengan versi paket.

## Lapisan stabil vs. tidak

- **Stabil (API publik):** semua yang diekspor dari `bkndb` — di-maintain tangan dan menyerap perubahan di sisi Rust.
- **Privat:** `bkndb._native.*` — berubah otomatis mengikuti `crates/bkndb-ffi`. Jangan di-import di kode aplikasi.
