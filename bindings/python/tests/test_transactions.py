import threading

import pytest

import bkndb
from bkndb import Column, Direction, TableSchema


@pytest.fixture()
def db_with_table(db: bkndb.Database) -> bkndb.Database:
    db.create_table(
        TableSchema(
            "docs",
            [Column("id", int), Column("node", int), Column("title", str, nullable=False)],
            primary_key="id",
            auto_increment=True,
            indexes=["node"],
        )
    )
    return db


def test_commit_makes_graph_and_rows_visible_together(db_with_table: bkndb.Database) -> None:
    db = db_with_table
    with db.transaction() as tx:
        node = tx.create_node("Doc", {"title": "Intro"})
        pk = tx.insert("docs", {"node": node, "title": "Intro"})
        # Read-your-own-writes inside the transaction...
        assert tx.get_node(node) is not None
        assert tx.table("docs").get(pk)["title"] == "Intro"
        # ...invisible outside until commit.
        assert db.get_node(node) is None
        assert db.count("docs") == 0
    assert not tx.active
    assert db.get_node(node).label == "Doc"
    assert db.select("docs", {"node": node})[0].pk == pk


def test_exception_rolls_back(db_with_table: bkndb.Database) -> None:
    db = db_with_table
    with pytest.raises(RuntimeError):
        with db.transaction() as tx:
            node = tx.create_node("Doc")
            raise RuntimeError("boom")
    assert db.get_node(node) is None
    # The engine is usable again afterwards.
    db.create_node("After")


def test_failed_operation_aborts_the_transaction(db_with_table: bkndb.Database) -> None:
    db = db_with_table
    tx = db.transaction()
    node = tx.create_node("Doc")
    with pytest.raises(bkndb.ConstraintViolationError):
        tx.insert("docs", {"node": node})  # title is NOT NULL
    with pytest.raises(bkndb.TransactionAbortedError):
        tx.create_node("More")
    with pytest.raises(bkndb.TransactionAbortedError):
        tx.commit()
    assert db.get_node(node) is None
    tx.rollback()  # idempotent
    with pytest.raises(bkndb.TransactionClosedError):
        tx.commit()


def test_engine_writes_are_refused_while_a_transaction_is_open(db: bkndb.Database) -> None:
    tx = db.transaction()
    try:
        with pytest.raises(bkndb.TransactionInProgressError):
            db.create_node("X")
        with pytest.raises(bkndb.TransactionInProgressError):
            db.transaction()
        with pytest.raises(bkndb.TransactionInProgressError):
            db.close()
        # Reads still work, on the last committed state.
        assert db.get_node(1) is None
    finally:
        tx.rollback()
    db.create_node("X")


def test_other_threads_can_use_the_transaction(db: bkndb.Database) -> None:
    ids = []
    with db.transaction() as tx:
        workers = [threading.Thread(target=lambda: ids.append(tx.create_node("T"))) for _ in range(4)]
        for w in workers:
            w.start()
        for w in workers:
            w.join()
    assert len({*ids}) == 4
    assert all(db.get_node(i) is not None for i in ids)


def test_graph_updates_and_traversal(db: bkndb.Database) -> None:
    a = db.create_node("File", {"path": "a.rs", "tmp": True})
    b = db.create_node("File")
    c = db.create_node("Dir")
    ab = db.create_edge(a, "IMPORTS", b, {"weight": 1})
    db.create_edge(c, "CONTAINS", a)

    db.update_node(a, set={"lines": 10}, unset=["tmp"])
    assert db.get_node(a).properties == {"path": "a.rs", "lines": 10}
    db.update_edge(ab, set={"weight": 2})
    assert db.get_edge(ab).properties == {"weight": 2}
    with pytest.raises(bkndb.NotFoundError):
        db.update_node(999, set={"x": 1})

    assert {n.edge_type for n in db.neighbors(a, Direction.BOTH)} == {"IMPORTS", "CONTAINS"}
    assert db.degree(a, Direction.OUT, "IMPORTS") == 1
    hits = db.traverse(c, max_depth=5)
    assert [(h.node_id, h.depth) for h in hits] == [(c, 0), (a, 1), (b, 2)]
    assert [h.node_id for h in db.traverse(c, max_depth=1)] == [c, a]

    assert db.delete_edge(ab) is True
    assert db.delete_edge(ab) is False
    assert db.degree(a) == 0


def test_to_networkx(db: bkndb.Database) -> None:
    nx = pytest.importorskip("networkx")
    a = db.create_node("A", {"n": 1})
    b = db.create_node("B")
    db.create_edge(a, "LINKS", b, {"w": 3})
    g = db.to_networkx(a)
    assert isinstance(g, nx.MultiDiGraph)
    assert g.nodes[a] == {"label": "A", "n": 1}
    [(u, v, data)] = list(g.edges(data=True))
    assert (u, v, data) == (a, b, {"type": "LINKS", "w": 3})


def test_close_releases_the_file_lock(tmp_path) -> None:
    path = str(tmp_path / "graph.bkndb")
    first = bkndb.open(path)
    node = first.create_node("Doc", {"title": "kept"})
    with pytest.raises(bkndb.DatabaseLockedError):
        bkndb.open(path)
    first.close()
    first.close()  # idempotent
    assert first.closed
    with pytest.raises(bkndb.DatabaseClosedError):
        first.get_node(node)
    # No reliance on garbage collection: the lock is gone right away.
    with bkndb.open(path, memtable_flush_bytes=1024) as again:
        assert again.get_node(node).properties == {"title": "kept"}
        again.compact()
        assert again.get_node(node) is not None
        assert "open" in repr(again)


def test_results_are_hashable_and_ints_are_range_checked(db: bkndb.Database) -> None:
    n = db.create_node("X", {"k": 1})
    assert len({db.get_node(n), db.get_node(n)}) == 1
    with pytest.raises(bkndb.InvalidArgumentError):
        db.create_node("X", {"big": 2**64})
    with pytest.raises(ValueError):  # InvalidArgumentError is also a ValueError
        db.get_node(-1)


def test_star_import_does_not_shadow_builtin_open() -> None:
    namespace: dict = {}
    exec("from bkndb import *", namespace)
    assert "open" not in namespace
