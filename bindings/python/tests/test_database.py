import pytest

import bkndb


def test_create_and_get_node(db: bkndb.Database) -> None:
    node_id = db.create_node("Document", {"title": "Attention Is All You Need", "pages": 15})
    node = db.get_node(node_id)
    assert node is not None
    assert node.label == "Document"
    assert node.properties == {"title": "Attention Is All You Need", "pages": 15}


def test_get_missing_node_returns_none(db: bkndb.Database) -> None:
    assert db.get_node(999_999) is None


def test_create_and_get_edge(db: bkndb.Database) -> None:
    a = db.create_node("Concept", {"name": "A"})
    b = db.create_node("Concept", {"name": "B"})
    edge_id = db.create_edge(a, "RELATES_TO", b, {"weight": 0.5})

    edge = db.get_edge(edge_id)
    assert edge is not None
    assert (edge.from_node, edge.edge_type, edge.to_node) == (a, "RELATES_TO", b)
    assert edge.properties == {"weight": 0.5}


def test_create_edge_with_missing_node_raises_not_found(db: bkndb.Database) -> None:
    a = db.create_node("Concept", {})
    with pytest.raises(bkndb.NotFoundError):
        db.create_edge(a, "RELATES_TO", 999_999, {})


def test_neighbors_out_and_in(db: bkndb.Database) -> None:
    a = db.create_node("Fn", {})
    b = db.create_node("Fn", {})
    edge_id = db.create_edge(a, "calls", b, {})

    out = db.neighbors_out(a, "calls")
    assert out == [bkndb.Neighbor(node_id=b, edge_id=edge_id)]

    inbound = db.neighbors_in(b, "calls")
    assert inbound == [bkndb.Neighbor(node_id=a, edge_id=edge_id)]


def test_delete_node_cascades_edges(db: bkndb.Database) -> None:
    a = db.create_node("Fn", {})
    b = db.create_node("Fn", {})
    db.create_edge(a, "calls", b, {})

    db.delete_node(a)

    assert db.get_node(a) is None
    assert db.neighbors_in(b, "calls") == []


def test_find_shortest_path(db: bkndb.Database) -> None:
    a = db.create_node("Fn", {})
    b = db.create_node("Fn", {})
    c = db.create_node("Fn", {})
    db.create_edge(a, "calls", b, {})
    db.create_edge(b, "calls", c, {})

    path = db.find_shortest_path(a, c, bkndb.Direction.OUT, ["calls"])
    assert path is not None
    assert path.node_ids == [a, b, c]


def test_find_shortest_path_none_when_unreachable(db: bkndb.Database) -> None:
    a = db.create_node("Fn", {})
    b = db.create_node("Fn", {})
    assert db.find_shortest_path(a, b) is None


def test_top_hubs(db: bkndb.Database) -> None:
    hub = db.create_node("Fn", {})
    for _ in range(3):
        leaf = db.create_node("Fn", {})
        db.create_edge(hub, "calls", leaf, {})

    hubs = db.top_hubs(1, bkndb.Direction.OUT)
    assert len(hubs) == 1
    assert hubs[0].node_id == hub
    assert hubs[0].degree == 3


def test_cascade_delete(db: bkndb.Database) -> None:
    root = db.create_node("Dir", {})
    child = db.create_node("File", {})
    db.create_edge(root, "contains", child, {})

    deleted = db.cascade_delete(root, "contains")
    assert set(deleted) == {root, child}
    assert db.get_node(root) is None
    assert db.get_node(child) is None


def test_bulk_creation_is_atomic_in_shape(db: bkndb.Database) -> None:
    ids = db.create_nodes_bulk([("Entity", {"name": f"e{i}"}) for i in range(5)])
    assert len(ids) == 5
    for node_id in ids:
        assert db.get_node(node_id) is not None


def test_sync_batch(db: bkndb.Database) -> None:
    result = db.sync_batch(nodes=[("Entity", {"name": "a"}), ("Entity", {"name": "b"})])
    assert len(result.node_ids) == 2
    for node_id in result.node_ids:
        assert db.get_node(node_id) is not None


def test_closed_database_raises(db: bkndb.Database) -> None:
    db.close()
    with pytest.raises(bkndb.BknDbError):
        db.create_node("X", {})


def test_property_round_trip_covers_every_scalar_type(db: bkndb.Database) -> None:
    node_id = db.create_node(
        "Kitchen",
        {
            "s": "hello",
            "i": 42,
            "f": 3.5,
            "b": True,
            "n": None,
            "bytes": b"\x00\x01",
        },
    )
    node = db.get_node(node_id)
    assert node is not None
    assert node.properties == {
        "s": "hello",
        "i": 42,
        "f": 3.5,
        "b": True,
        "n": None,
        "bytes": b"\x00\x01",
    }


def test_file_database_persists_and_rejects_second_open(tmp_path) -> None:
    path = str(tmp_path / "graph.bkndb")
    first = bkndb.open(path)
    node_id = first.create_node("Doc", {"title": "kept"})
    with pytest.raises(bkndb.DatabaseLockedError):
        bkndb.open(path)
    first.close()
    del first

    with bkndb.open(path) as reopened:
        node = reopened.get_node(node_id)
        assert node is not None
        assert node.properties["title"] == "kept"
