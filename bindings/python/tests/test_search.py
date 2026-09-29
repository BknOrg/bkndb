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


def test_pack_vector() -> None:
    packed = bkndb.pack_vector([1.0, -2.5])
    assert packed == b"\x00\x00\x80\x3f\x00\x00\x20\xc0"
    assert bkndb.pack_vector(range(3)) == bkndb.pack_vector([0.0, 1.0, 2.0])
