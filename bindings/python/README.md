# bkndb — Python bindings

Embedded hybrid graph + relational database engine for Python (via UniFFI).

## Fitur Utama untuk Python & AI/ML

1. **Embedded / In-Process:** Berjalan langsung di dalam proses Python Anda (seperti SQLite). Tanpa server terpisah, tanpa Docker, tanpa daemon background.
2. **GraphRAG & Knowledge Retrieval:** Traversal relasi multi-hop (BFS Shortest Path, Top Hubs Centrality) untuk memperkaya prompt LLM.
3. **High Throughput Bulk Ingest:** Batch atomik untuk node/edge dalam jumlah besar ke format single-file `.bkndb`.
4. **Single-file Database (`.bkndb`):** Mudah didistribusikan bersama model AI atau workspace data.
5. **Cross-platform:** satu wheel per OS/arsitektur (Windows, Linux, macOS × x86_64/arm64), berlaku untuk semua versi Python 3.9+ — lihat [Arsitektur](#arsitektur--kenapa-cross-platform-tanpa-matriks-cpxx) di bawah.

## Instalasi

```bash
pip install bkndb
```

## Struktur Paket

```
bindings/python/
├── pyproject.toml          # metadata paket + build backend (setuptools)
├── setup.py                # hanya untuk memaksa wheel tag platform-specific
├── bkndb/                  # API publik — HANYA di sini yang boleh di-import
│   ├── __init__.py
│   ├── database.py         # class Database, wrapper Pythonic atas BknDbEngine
│   ├── errors.py           # BknDbError & subclass-nya
│   ├── types.py            # Node/Edge/Path/... dataclass + konversi PropValue
│   ├── py.typed
│   └── _native/             # PRIVAT — hasil generate uniffi, jangan diimpor langsung
│       ├── bkndb_ffi.py
│       └── bkndb_ffi.dll / libbkndb_ffi.so / libbkndb_ffi.dylib
├── scripts/
│   └── build_native.py     # build ulang crate Rust + regenerate _native/
├── tests/
│   └── test_database.py
└── examples/
    └── basic_usage.py
```

## Contoh Penggunaan Cepat

```python
import bkndb

with bkndb.open("my_knowledge.bkndb") as db:
    # atau: with bkndb.in_memory() as db:
    n1 = db.create_node("Prompt", {"text": "Analisis codebase"})
    n2 = db.create_node("Tool", {"name": "Linter"})
    db.create_edge(n1, "USES", n2)

    for neighbor in db.neighbors_out(n1, "USES"):
        target = db.get_node(neighbor.node_id)
        print("Connected to:", target.label, target.properties)
```

Properti node/edge cukup pakai tipe Python biasa (`str`/`int`/`float`/`bool`/`bytes`/`None`) — tidak perlu `FfiPropValue.STR(...)` manual; konversi ke/dari tipe internal ditangani `bkndb.types`.

Error dari sisi Rust muncul sebagai exception Python asli: `bkndb.NotFoundError`, `bkndb.BackendError`, `bkndb.TableNotFoundError`, `bkndb.EncodingError`, `bkndb.ReservedTableNameError` — semuanya turunan `bkndb.BknDbError`.

## Development

```bash
# 1. Build crate Rust + regenerate bkndb/_native/ untuk platform saat ini
python scripts/build_native.py

# 2. Install paket dalam mode editable + dependency test
pip install -e ".[test]"

# 3. Jalankan test
pytest
```

### Build wheel asli & uji di venv bersih (sebelum publish)

```bash
python -m pip install build
python -m build --wheel
# harus menghasilkan bkndb-<versi>-py3-none-<platform>.whl (bukan cpXYZ-cpXYZ-...)

python -m venv /tmp/wheel-check
/tmp/wheel-check/bin/pip install dist/bkndb-*.whl   # Scripts\pip.exe di Windows
/tmp/wheel-check/bin/python -c "import bkndb; db = bkndb.in_memory(); print(db.create_node('x', {}))"
```

## Arsitektur — kenapa cross-platform tanpa matriks `cp3x`

Binding ini pakai gaya UniFFI "Python murni": `bkndb/_native/bkndb_ffi.py` memuat library native lewat `ctypes` saat runtime, bukan lewat ABI ekstensi C CPython (beda dengan PyO3). Konsekuensinya: **satu wheel per OS/arsitektur sudah cukup untuk semua versi Python 3.9+** — tidak perlu build matrix `cp39`/`cp310`/`cp311`/....

`setup.py` di root paket ini memaksa wheel bertanda `py3-none-<platform>` (mis. `py3-none-win_amd64`) lewat dua override:
1. `BinaryDistribution.has_ext_modules() -> True` — supaya `bdist_wheel` memilih tag platform (`win_amd64`, dst), bukan `any`.
2. Override `bdist_wheel.get_tag()` — tanpa ini, `bdist_wheel` tetap menempelkan tag Python+ABI milik interpreter yang menjalankan build (mis. `cp312-cp312`), padahal tidak ada C extension yang benar-benar dikompilasi di sini (filenya sudah disiapkan lebih dulu oleh `scripts/build_native.py`). Override ini memaksa bagian python/abi jadi `py3`/`none`, menyisakan hanya tag platform.

Sudah diverifikasi lokal: `python -m build --wheel` di Windows menghasilkan `bkndb-0.1.0-py3-none-win_amd64.whl`, dan wheel itu bisa `pip install` + `import bkndb` di venv bersih tanpa source tree sama sekali.

## Publish ke PyPI

### 1. Build satu wheel per platform (perlu CI — tidak bisa dari satu mesin)

Rust dikompilasi native per OS/arsitektur, jadi wheel Linux/macOS **tidak bisa** dibuat dari Windows (atau sebaliknya). `.github/workflows/python-wheels.yml` sudah disiapkan: matrix 4 runner (`windows-latest`, `ubuntu-latest`, `macos-13` Intel, `macos-14` Apple Silicon), masing-masing menjalankan `scripts/build_native.py` → `python -m build --wheel` → smoke-test install di venv bersih → upload sebagai artifact.

Trigger: otomatis di setiap PR yang menyentuh `bindings/python/**` atau `crates/bkndb-ffi/**` (build + smoke-test saja, tidak publish), atau manual lewat tab **Actions → Run workflow**.

### 2. Setup akun PyPI + Trusted Publishing (sekali saja, manual di web PyPI)

Workflow publish-nya pakai [Trusted Publishing](https://docs.pypi.org/trusted-publishers/) (OIDC), **bukan** API token yang disimpan sebagai secret — lebih aman, tidak ada token yang bisa bocor. Langkah satu kali:

1. Buat akun di [pypi.org](https://pypi.org) (dan [test.pypi.org](https://test.pypi.org) untuk uji coba dulu).
2. Di halaman project PyPI (`https://pypi.org/manage/project/bkndb/settings/publishing/` — atau "pending publisher" kalau project belum pernah dipublish), tambahkan trusted publisher: repo `BknOrg/bkn-db`, workflow `python-wheels.yml`, environment `pypi`.
3. Di GitHub repo settings → Environments, buat environment bernama `pypi` (boleh tambahkan required reviewer untuk approval manual sebelum publish jalan).

### 3. Rilis

```bash
# Naikkan versi dulu di pyproject.toml — PyPI menolak upload ulang versi yang sama
git tag python-v0.1.0
git push origin python-v0.1.0
```

Push tag `python-v*` memicu job `publish` di workflow: menunggu keempat wheel selesai build+smoke-test, lalu upload semuanya ke PyPI sekaligus. Disarankan uji ke TestPyPI dulu (ubah target di workflow atau publish manual dengan `twine upload --repository testpypi dist/*`) sebelum tag rilis nyata.

## Lapisan yang stabil vs. yang tidak

- **Stabil (API publik):** `bkndb.Database`, `bkndb.open`/`bkndb.in_memory`, semua exception, semua dataclass di `bkndb.types`. Ini yang di-maintain tangan dan yang menyerap perubahan di sisi Rust.
- **Tidak stabil (privat):** `bkndb._native.*` — berubah bentuk otomatis setiap `crates/bkndb-ffi` berubah. Jangan pernah import ini di kode aplikasi.
