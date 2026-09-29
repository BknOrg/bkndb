import json

import pytest

import bkndb
from bkndb import Column, TableSchema, col

ITEMS = TableSchema(
    "items",
    [
        Column("id", int),
        Column("name", str, nullable=False),
        Column("price", float),
        Column("active", bool, default=True),
        Column("blob", bytes),
    ],
    primary_key="id",
    auto_increment=True,
    indexes=["name"],
)


def seed(db: bkndb.Database, n: int) -> None:
    db.create_table(ITEMS)
    db.insert_many("items", [{"name": f"item-{i:04d}", "price": i / 2} for i in range(n)])


def test_iter_rows_streams_in_pk_order_with_filters(db: bkndb.Database) -> None:
    seed(db, 250)
    rows = list(db.iter_rows("items", batch_size=7))
    assert [r.pk for r in rows] == list(range(1, 251))
    evens = list(db.iter_rows("items", col("price") >= 100, batch_size=10, columns=["name"]))
    assert [r.pk for r in evens] == list(range(201, 251))
    assert evens[0].values == {"name": "item-0200"}
    # An index-served filter still pages correctly.
    assert [r.pk for r in db.iter_rows("items", {"name": "item-0042"}, batch_size=1)] == [43]
    assert sum(1 for _ in db.table("items")) == 250
    with pytest.raises(bkndb.TableNotFoundError):
        next(db.iter_rows("nope"))
    with pytest.raises(ValueError):
        next(db.iter_rows("items", batch_size=0))


def test_jsonl_round_trip_preserves_types(db: bkndb.Database, tmp_path) -> None:
    db.create_table(ITEMS)
    db.insert("items", {"name": "a", "price": 1.5, "blob": b"\x00\xff", "active": False})
    db.insert("items", {"name": "b"})
    path = tmp_path / "items.jsonl"
    assert db.export_jsonl("items", path) == 2
    first = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    assert first == {"id": 1, "name": "a", "price": 1.5, "blob": {"$bytes": "AP8="}, "active": False}

    with bkndb.in_memory() as other:
        other.create_table(ITEMS)
        assert other.import_jsonl("items", path) == 2
        assert other.select("items", order_by="id") == db.select("items", order_by="id")
        assert other.import_jsonl("items", path) == 2  # upsert: idempotent
        assert other.count("items") == 2
        with pytest.raises(bkndb.DuplicateKeyError):
            other.import_jsonl("items", path, mode="insert")


def test_csv_round_trip_and_type_conversion(db: bkndb.Database, tmp_path) -> None:
    db.create_table(ITEMS)
    db.insert("items", {"name": "a, with comma", "price": 2.25, "blob": b"xyz", "active": False})
    db.insert("items", {"name": "b"})
    path = tmp_path / "items.csv"
    assert db.export_csv("items", path, where=col("id") >= 1) == 2
    lines = path.read_text(encoding="utf-8").splitlines()
    assert lines[0] == "id,name,price,active,blob"
    assert lines[1] == '1,"a, with comma",2.25,false,eHl6'
    assert lines[2] == "2,b,,true,"

    with bkndb.in_memory() as other:
        other.create_table(ITEMS)
        assert other.import_csv("items", path) == 2
        assert other.select("items", order_by="id") == db.select("items", order_by="id")

    # Hand-written CSV: no id column (auto-increment), empty cells take defaults.
    hand = tmp_path / "hand.csv"
    hand.write_text("name,price,active\nx,1,yes\ny,,\n", encoding="utf-8")
    with bkndb.in_memory() as other:
        other.create_table(ITEMS)
        assert other.import_csv("items", hand) == 2
        rows = other.select("items", order_by="id")
        assert [(r.pk, r.values["name"], r.values.get("price"), r.values["active"]) for r in rows] == [
            (1, "x", 1.0, True),
            (2, "y", None, True),
        ]


def test_failed_import_writes_nothing(db: bkndb.Database, tmp_path) -> None:
    db.create_table(ITEMS)
    bad = tmp_path / "bad.csv"
    bad.write_text("name,price\nok,1\nbroken,not-a-number\n", encoding="utf-8")
    with pytest.raises(bkndb.InvalidArgumentError, match="price"):
        db.import_csv("items", bad)
    unknown = tmp_path / "unknown.csv"
    unknown.write_text("name,colour\nok,red\n", encoding="utf-8")
    with pytest.raises(bkndb.SchemaMismatchError):
        db.import_csv("items", unknown)
    missing_required = tmp_path / "missing.jsonl"
    missing_required.write_text('{"name": "ok"}\n{"price": 3}\n', encoding="utf-8")
    with pytest.raises(bkndb.BknDbError):
        db.import_jsonl("items", missing_required, batch_size=1)
    assert db.count("items") == 0


def test_stats_backup_and_verify_on_disk(tmp_path) -> None:
    path = tmp_path / "live.bkndb"
    with bkndb.open(path, memtable_flush_bytes=4096, block_size=512, compression=True) as db:
        seed(db, 300)
        a = db.create_node("N")
        b = db.create_node("N")
        db.create_edge(a, "E", b)

        stats = db.stats()
        assert (stats.nodes, stats.edges, stats.tables) == (2, 1, {"items": 300})
        assert stats.storage is not None and stats.storage.file_bytes > 0
        report = db.verify_integrity()
        assert report.blocks_verified > 0 and report.legacy_blocks_unchecked == 0

        with db.transaction() as tx:
            tx.insert("items", {"name": "late"})
            db.backup(tmp_path / "copy.bkndb")  # uncommitted row not included
        with pytest.raises(bkndb.BackendError, match="already exists"):
            db.backup(tmp_path / "copy.bkndb")

        db.compact()
        assert db.stats().storage.reclaimable_bytes == 0

    with bkndb.open(tmp_path / "copy.bkndb") as copy:
        assert copy.stats().tables == {"items": 300}
        assert copy.count("items", col("name") == "late") == 0
        copy.verify_integrity()


def test_in_memory_operations(db: bkndb.Database, tmp_path) -> None:
    seed(db, 3)
    assert db.stats().storage is None
    assert db.verify_integrity().sstables_checked == 0
    with pytest.raises(bkndb.InvalidArgumentError):
        db.backup(tmp_path / "x.bkndb")


def test_corruption_is_reported(tmp_path) -> None:
    path = tmp_path / "c.bkndb"
    with bkndb.open(path, block_size=256) as db:
        seed(db, 200)
        db.compact()
    data = bytearray(path.read_bytes())
    data[128 + 30] ^= 0x5A  # first data block, right after the 128-byte header
    path.write_bytes(bytes(data))
    with bkndb.open(path) as db:
        with pytest.raises(bkndb.CorruptionError):
            db.verify_integrity()
        with pytest.raises(bkndb.CorruptionError):
            db.select("items")
