import pytest

import bkndb
from bkndb import Agg, Column, TableSchema, col

PEOPLE = TableSchema(
    "people",
    [
        Column("id", int),
        Column("email", str, nullable=False, unique=True),
        Column("city", str),
        Column("age", int, default=0),
        Column("score", float),
    ],
    primary_key="id",
    auto_increment=True,
    indexes=["city"],
)


@pytest.fixture()
def people(db: bkndb.Database) -> bkndb.Table:
    assert db.create_table(PEOPLE) is True
    t = db.table("people")
    t.insert_many(
        [
            {"email": "a@x", "city": "Jakarta", "age": 30, "score": 1.5},
            {"email": "b@x", "city": "Bandung", "age": 25},
            {"email": "c@x", "city": "Jakarta", "age": 41, "score": 3.0},
        ]
    )
    return t


def test_schema_roundtrip_and_catalog(db: bkndb.Database, people: bkndb.Table) -> None:
    assert db.create_table(PEOPLE) is False  # identical definition: no-op
    [schema] = db.list_tables()
    assert schema.name == "people"
    assert schema.column("email").unique
    assert set(schema.indexes) == {"city", "email"}  # UNIQUE implies an index
    assert db.table_schema("nope") is None


def test_insert_get_defaults_and_constraints(db: bkndb.Database, people: bkndb.Table) -> None:
    pk = people.insert({"email": "d@x"})
    row = people.get(pk)
    assert row is not None and row.pk == pk
    assert row["age"] == 0  # DEFAULT
    assert row.get("city") is None
    with pytest.raises(bkndb.ConstraintViolationError):
        people.insert({"email": "a@x"})  # UNIQUE
    with pytest.raises(bkndb.ConstraintViolationError):
        people.insert({"city": "Solo"})  # NOT NULL email
    with pytest.raises(bkndb.SchemaMismatchError):
        people.insert({"email": "e@x", "age": "old"})
    with pytest.raises(bkndb.TableNotFoundError):
        db.insert("missing", {})

    db.create_table(TableSchema("tags", [Column("name", str)], primary_key="name"))
    db.insert("tags", {"name": "x"})
    with pytest.raises(bkndb.DuplicateKeyError):
        db.insert("tags", {"name": "x"})


def test_select_filters_order_paging_projection(people: bkndb.Table) -> None:
    rows = people.select((col("city") == "Jakarta") & (col("age") >= 35))
    assert [r["email"] for r in rows] == ["c@x"]
    assert [r["email"] for r in people.select({"city": "Bandung"})] == ["b@x"]
    assert [r["email"] for r in people.select(col("city").is_in(["Bandung", "Jakarta"]), order_by="-age")] == [
        "c@x",
        "a@x",
        "b@x",
    ]
    assert [r["age"] for r in people.select(order_by="age", offset=1, limit=1)] == [30]
    assert [r["email"] for r in people.select(col("score").is_null())] == ["b@x"]
    assert [r["email"] for r in people.select(~(col("city") == "Jakarta"))] == ["b@x"]
    assert [r["email"] for r in people.select(col("email").startswith("a") | col("age").between(40, 50), order_by="id")] == [
        "a@x",
        "c@x",
    ]
    projected = people.select(col("email") == "a@x", columns=["age"])
    assert projected[0].values == {"age": 30}
    assert people.count(col("city") == "Jakarta") == 2
    assert len(people) == 3
    assert {r.pk for r in people} == {1, 2, 3}
    with pytest.raises(bkndb.SchemaMismatchError):
        people.select(col("colour") == "red")


def test_filter_expressions_refuse_python_boolean_operators() -> None:
    with pytest.raises(TypeError):
        (col("a") == 1) and (col("b") == 2)  # noqa: B015


def test_update_delete_and_aggregates(db: bkndb.Database, people: bkndb.Table) -> None:
    [overall] = db.aggregate("people", [Agg.count(), Agg.sum("age"), Agg.avg("age"), Agg.max("score"), Agg.count("score")])
    assert overall.group == ()
    assert overall.values == (3, 96, 32.0, 3.0, 2)
    by_city = db.aggregate("people", [Agg.count(), Agg.min("age")], group_by=["city"])
    assert [(g.group, g.values) for g in by_city] == [(("Bandung",), (1, 25)), (("Jakarta",), (2, 30))]

    assert people.update({"city": "Depok"}, col("city") == "Jakarta") == 2
    assert people.count({"city": "Depok"}) == 2
    assert people.delete(col("age") < 35) == 2
    assert [r["email"] for r in people.select()] == ["c@x"]


def test_migration_and_indexes(db: bkndb.Database, people: bkndb.Table) -> None:
    v2 = TableSchema(
        "people",
        [*PEOPLE.columns, Column("country", str, default="ID")],
        primary_key="id",
        auto_increment=True,
        indexes=["city", "country"],
    )
    db.ensure_table(v2)
    assert all(r["country"] == "ID" for r in people.select())
    assert people.count(col("country") == "ID") == 3
    db.drop_index("people", "country")
    assert "country" not in db.table_schema("people").indexes
    db.create_index("people", "age")
    assert "age" in db.table_schema("people").indexes
    assert db.drop_table("people") is True
    assert db.list_tables() == []


def test_upsert_and_sync_batch_rows(db: bkndb.Database) -> None:
    db.create_table(TableSchema("files", [Column("path", str), Column("lang", str)], primary_key="path", indexes=["lang"]))
    db.upsert("files", {"path": "a.rs", "lang": "rust"})
    db.upsert("files", {"path": "a.rs", "lang": "python"})
    assert db.count("files") == 1
    assert db.get_row("files", "a.rs")["lang"] == "python"

    rows = {"files": [{"path": "b.go", "lang": "go"}, {"path": "a.rs", "lang": "rust"}]}
    res = db.sync_batch(rows=rows)
    assert res.row_pks == {"files": ["b.go", "a.rs"]}
    db.sync_batch(rows=rows)  # re-applying is safe (upsert)
    assert db.count("files") == 2
    assert db.count("files", {"lang": "rust"}) == 1


def test_select_df(db: bkndb.Database, people: bkndb.Table) -> None:
    pd = pytest.importorskip("pandas")
    frame = db.select_df("people", order_by="id")
    assert isinstance(frame, pd.DataFrame)
    assert frame.index.name == "id"
    assert list(frame["email"]) == ["a@x", "b@x", "c@x"]
