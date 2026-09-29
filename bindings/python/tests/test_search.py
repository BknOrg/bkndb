import math

import pytest

import bkndb
from bkndb import Column, TableSchema, col

ARTICLES = TableSchema(
    "articles",
    [
        Column("id", int),
        Column("title", str),
        Column("body", str),
        Column("lang", str),
        Column("emb", bytes),
    ],
    primary_key="id",
)

DATA = [
    (1, "Rust", "Ownership and borrowing make Rust memory safe.", "en", [1.0, 0.0, 0.0]),
    (2, "Graphs", "A graph database stores nodes and edges; graph queries follow edges.", "en", [0.0, 1.0, 0.0]),
    (3, "Graf", "Basis data graf menyimpan simpul dan sisi.", "id", [0.0, 0.9, 0.2]),
    (4, "Both", "Building a graph database in Rust.", "en", [0.7, 0.7, 0.0]),
]


def seed(db: bkndb.Database) -> None:
    db.create_table(ARTICLES)
    db.insert_many(
        "articles",
        [{"id": i, "title": t, "body": b, "lang": lang, "emb": bkndb.pack_vector(e)} for i, t, b, lang, e in DATA],
    )


def pks(hits):
    return [h.row.pk for h in hits]


def test_fulltext_search(db: bkndb.Database) -> None:
    seed(db)
    assert db.create_fulltext_index("articles", "body") is True
    assert db.create_fulltext_index("articles", "body") is False
    assert db.fulltext_indexes("articles") == ["body"]

    assert pks(db.search_text("articles", "body", "graph")) == [2, 4]
    assert pks(db.search_text("articles", "body", "rust graph", match_all=True)) == [4]
    assert pks(db.search_text("articles", "body", "borrow*")) == [1]
    assert pks(db.search_text("articles", "body", "GRAF")) == [3]
    assert pks(db.search_text("articles", "body", "graph", where=col("id") > 2)) == [4]
    assert pks(db.search_text("articles", "body", "graph", limit=1)) == [2]
    top = db.search_text("articles", "body", "edges")[0]
    assert top.row.values["title"] == "Graphs" and top.score > 0

    # Index follows writes and transactions.
    db.update_rows("articles", {"body": "Nothing to see."}, {"id": 4})
    assert pks(db.search_text("articles", "body", "rust")) == [1]
    with db.transaction() as tx:
        tx.insert("articles", {"id": 5, "body": "rust in a transaction"})
    assert pks(db.search_text("articles", "body", "transaction")) == [5]
    db.delete_rows("articles", {"id": 5})
    assert db.search_text("articles", "body", "transaction") == []

    with pytest.raises(bkndb.QueryError):
        db.search_text("articles", "title", "rust")  # no index there
    assert db.drop_fulltext_index("articles", "body") is True


def test_vector_search(db: bkndb.Database) -> None:
    seed(db)
    near = db.search_vector("articles", "emb", [0.0, 1.0, 0.1], limit=2)
    assert pks(near) == [2, 3]
    assert math.isclose(near[0].score, 1 / math.sqrt(1.01), rel_tol=1e-5)
    assert pks(db.search_vector("articles", "emb", [0.0, 0.0, 1.0], limit=1, metric="euclidean")) == [3]
    assert pks(db.search_vector("articles", "emb", [2.0, 0.1, 0.0], limit=2, metric="dot")) == [1, 4]
    assert pks(db.search_vector("articles", "emb", [0.0, 1.0, 0.0], where={"lang": "id"})) == [3]

    with pytest.raises(ValueError):
        db.search_vector("articles", "emb", [1.0], metric="manhattan")
    with pytest.raises(bkndb.QueryError):
        db.search_vector("articles", "emb", [1.0, 0.0])  # wrong dimension


def test_vector_index(db: bkndb.Database) -> None:
    import random

    rng = random.Random(7)
    db.create_table(TableSchema("v", [Column("id", int), Column("grp", str), Column("emb", bytes)], primary_key="id"))
    vectors = {i: [rng.uniform(-1, 1) for _ in range(8)] for i in range(1, 401)}
    db.insert_many("v", [{"id": i, "grp": "ab"[i % 2], "emb": bkndb.pack_vector(v)} for i, v in vectors.items()])

    assert db.create_vector_index("v", "emb", m=8, ef_construction=64) is True
    assert db.create_vector_index("v", "emb") is False
    (info,) = db.vector_indexes("v")
    assert (info.column, info.metric, info.m, info.ef_construction, info.dimensions, info.vectors) == ("emb", "cosine", 8, 64, 8, 400)

    q = [rng.uniform(-1, 1) for _ in range(8)]
    approx = db.search_vector("v", "emb", q, limit=10)
    exact = db.search_vector("v", "emb", q, limit=10, exact=True)
    assert len(set(pks(approx)) & set(pks(exact))) >= 9
    assert pks(db.search_vector("v", "emb", vectors[123], limit=1, ef_search=128)) == [123]
    assert all(h.row.values["grp"] == "a" for h in db.search_vector("v", "emb", q, where={"grp": "a"}))

    # Maintained by writes, rolled back with transactions.
    db.delete_rows("v", {"id": 123})
    assert 123 not in pks(db.search_vector("v", "emb", vectors[123], limit=5))
    with pytest.raises(RuntimeError):
        with db.transaction() as tx:
            tx.insert("v", {"id": 999, "grp": "a", "emb": bkndb.pack_vector(q)})
            raise RuntimeError("roll back")
    assert db.vector_indexes("v")[0].vectors == 399

    with pytest.raises(bkndb.QueryError):
        db.insert("v", {"id": 1000, "emb": bkndb.pack_vector([1.0, 2.0])})  # wrong dimension
    with pytest.raises(bkndb.SchemaMismatchError):
        db.create_vector_index("v", "grp")
    with pytest.raises(ValueError):
        db.create_vector_index("v", "emb", metric="manhattan")
    assert db.drop_vector_index("v", "emb") is True
    assert db.vector_indexes("v") == []


def test_pack_vector() -> None:
    packed = bkndb.pack_vector([1.0, -2.5])
    assert packed == b"\x00\x00\x80\x3f\x00\x00\x20\xc0"
    assert bkndb.pack_vector(range(3)) == bkndb.pack_vector([0.0, 1.0, 2.0])
