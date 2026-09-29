import pytest

import bkndb
from bkndb import Direction, NewNode


def test_nodes_by_label_and_find_nodes(db: bkndb.Database) -> None:
    alice = db.create_node("Person", {"name": "alice", "age": 30})
    bob = db.create_node("Person", {"name": "bob", "age": 41})
    city = db.create_node("City", {"name": "Jakarta"})

    assert db.nodes_by_label("Person") == [alice, bob]
    assert db.count_nodes("City") == 1
    assert db.nodes_by_label("Nope") == []
    assert db.find_nodes("Person", "age", 41) == [bob]  # scan: not indexed yet

    assert db.create_node_index("Person", "age") is True
    assert db.create_node_index("Person", "age") is False
    assert db.node_indexes() == [("Person", "age")]
    assert db.find_nodes("Person", "age", 30) == [alice]

    db.update_node(alice, set={"age": 41})
    assert db.find_nodes("Person", "age", 41) == [alice, bob]
    db.delete_node(bob)
    assert db.find_nodes("Person", "age", 41) == [alice]
    assert db.find_nodes("City", "name", "Jakarta") == [city]

    with db.transaction() as tx:
        carol = tx.create_node("Person", {"age": 41})
        assert tx.find_nodes("Person", "age", 41) == [alice, carol]
        assert tx.nodes_by_label("Person") == [alice, carol]

    db.rebuild_graph_indexes()
    assert db.find_nodes("Person", "age", 41) == [alice, carol]
    assert db.drop_node_index("Person", "age") is True
    assert db.node_indexes() == []


def test_find_weighted_path(db: bkndb.Database) -> None:
    a, b, c = (db.create_node("Stop") for _ in range(3))
    db.create_edge(a, "BUS", b, {"minutes": 5})
    db.create_edge(b, "BUS", c, {"minutes": 5})
    db.create_edge(a, "BUS", c, {"minutes": 30})

    best = db.find_weighted_path(a, c, weight="minutes")
    assert best is not None
    assert best.cost == 10.0
    assert best.path.node_ids == [a, b, c]
    # Unweighted BFS takes the direct hop instead.
    assert db.find_shortest_path(a, c).node_ids == [a, c]
    assert db.find_weighted_path(c, a, weight="minutes") is None
    assert db.find_weighted_path(c, a, weight="minutes", direction=Direction.BOTH).cost == 10.0
    with pytest.raises(bkndb.EncodingError):
        db.find_weighted_path(a, c, weight="minutes", default_weight=-1)


def test_sync_batch_with_new_node_references(db: bkndb.Database) -> None:
    repo = db.create_node("Repo")
    result = db.sync_batch(
        nodes=[("File", {"path": "main.rs"}), ("Function", {"name": "main"})],
        edges=[
            (repo, "CONTAINS", NewNode(0), {}),
            (NewNode(0), "DEFINES", NewNode(1), {"line": 1}),
        ],
    )
    file_id, fn_id = result.node_ids
    assert [n.node_id for n in db.neighbors(repo)] == [file_id]
    [defines] = db.neighbors(file_id, edge_type="DEFINES")
    assert defines.node_id == fn_id
    assert db.get_edge(defines.edge_id).properties == {"line": 1}

    with pytest.raises(bkndb.EncodingError):
        db.sync_batch(nodes=[("File", {})], edges=[(NewNode(0), "X", NewNode(3), {})])
    assert db.count_nodes("File") == 1  # the failed batch left nothing behind
