import datetime
import uuid

import pytest

import bkndb
from bkndb import Column, TableSchema, col

UTC = datetime.timezone.utc
EVENTS = TableSchema(
    "events",
    [
        Column("id", uuid.UUID),
        Column("at", datetime.datetime),
        Column("tags", list),
        Column("meta", dict),
        Column("title", str),
    ],
    primary_key="id",
    indexes=["at"],
)

IDS = [uuid.UUID(int=i) for i in range(1, 4)]


def seed(db: bkndb.Database) -> None:
    db.create_table(EVENTS)
    db.insert_many(
        "events",
        [
            {"id": IDS[2], "at": datetime.datetime(2026, 3, 1, tzinfo=UTC), "tags": ["rust", "db"],
             "meta": {"author": "ana", "score": 7, "extra": {"n": [1, 2.5, None]}}, "title": "Graph Databases"},
            {"id": IDS[0], "at": datetime.datetime(2026, 1, 1, tzinfo=UTC), "tags": ["python"],
             "meta": {"author": "budi", "score": 9}, "title": "Intro to Python"},
            {"id": IDS[1], "at": datetime.datetime(2026, 2, 1, 12, 30, 0, 123456, tzinfo=UTC), "tags": ("rust",),
             "meta": {"author": "ana", "score": 1}, "title": "rust tips"},
        ],
    )


def pks(rows):
    return [r.pk for r in rows]


def test_values_round_trip(db: bkndb.Database) -> None:
    seed(db)
    row = db.get_row("events", IDS[1])
    assert row is not None
    assert row.values["at"] == datetime.datetime(2026, 2, 1, 12, 30, 0, 123456, tzinfo=UTC)
    assert row.values["tags"] == ["rust"]  # tuples come back as lists
    assert db.get_row("events", IDS[2]).values["meta"]["extra"] == {"n": [1, 2.5, None]}
    assert db.table_schema("events").column("at").type == "timestamp"

    # Naive datetimes are taken as UTC.
    node = db.create_node("T", {"when": datetime.datetime(2000, 1, 1), "id": IDS[0], "nested": {"a": [b"x"]}})
    props = db.get_node(node).properties
    assert props == {"when": datetime.datetime(2000, 1, 1, tzinfo=UTC), "id": IDS[0], "nested": {"a": [b"x"]}}

    with pytest.raises(TypeError):
        db.create_node("T", {"bad": {1: "non-str key"}})
    with pytest.raises(bkndb.SchemaMismatchError):
        db.insert("events", {"id": uuid.uuid4(), "at": 5})


def test_filters_on_new_types(db: bkndb.Database) -> None:
    seed(db)
    assert pks(db.select("events")) == IDS  # uuid pk order
    feb = datetime.datetime(2026, 2, 1, tzinfo=UTC)
    assert pks(db.select("events", col("at") >= feb, order_by="at")) == [IDS[1], IDS[2]]
    assert pks(db.select("events", col("tags").contains("rust"))) == [IDS[1], IDS[2]]
    assert pks(db.select("events", col("meta")["author"] == "ana")) == [IDS[1], IDS[2]]
    assert pks(db.select("events", col("tags")[0] == "python")) == [IDS[0]]
    assert pks(db.select("events", order_by="-meta.score")) == [IDS[0], IDS[2], IDS[1]]
    assert pks(db.select("events", col("title").like("%tips"))) == [IDS[1]]
    assert pks(db.select("events", col("title").ilike("GRAPH%"))) == [IDS[2]]
    assert pks(db.select("events", {"id": IDS[0]})) == [IDS[0]]
    assert db.count("events", col("meta").contains("score")) == 3


def test_import_export_new_types(db: bkndb.Database, tmp_path) -> None:
    seed(db)
    for fmt in ("jsonl", "csv"):
        path = tmp_path / f"events.{fmt}"
        assert getattr(db, f"export_{fmt}")("events", path) == 3
        with bkndb.in_memory() as other:
            other.create_table(EVENTS)
            assert getattr(other, f"import_{fmt}")("events", path) == 3
            assert other.select("events") == db.select("events"), fmt
    line = (tmp_path / "events.jsonl").read_text(encoding="utf-8").splitlines()[0]
    assert '"$uuid": "00000000-0000-0000-0000-000000000001"' in line
    assert '"$datetime": "2026-01-01T00:00:00Z"' in line
