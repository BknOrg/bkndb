import datetime
import uuid

import pytest

import bkndb

UTC = datetime.timezone.utc


def test_sql_round_trip(db: bkndb.Database) -> None:
    db.sql(
        "CREATE TABLE users (id INT PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, age INT DEFAULT 0,"
        " joined TIMESTAMP, token UUID, meta JSON)"
    )
    token = uuid.uuid4()
    when = datetime.datetime(2026, 1, 2, 3, 4, 5, tzinfo=UTC)
    ins = db.sql(
        "INSERT INTO users (name, age, joined, token, meta) VALUES (?, ?, ?, ?, ?), ('Budi', 25, NULL, NULL, NULL)",
        ["Ana", 30, when, token, {"team": "core", "langs": ["rust", "py"]}],
    )
    assert ins.affected == 2 and ins.column("id") == [1, 2]

    res = db.sql("SELECT name, joined, token, meta.team AS team FROM users WHERE age >= :min ORDER BY name", {"min": 25})
    assert res.columns == ("name", "joined", "token", "team")
    assert res.dicts() == [
        {"name": "Ana", "joined": when, "token": token, "team": "core"},
        {"name": "Budi", "joined": None, "token": None, "team": None},
    ]
    assert list(res)[1][0] == "Budi" and len(res) == 2
    assert db.sql("SELECT COUNT(*) FROM users WHERE meta.langs CONTAINS 'rust'").scalar() == 1
    assert db.sql("SELECT name FROM users WHERE joined > TIMESTAMP '2026-01-01T00:00:00Z'").scalar() == "Ana"
    assert db.sql("UPDATE users SET age = ? WHERE name = 'Budi'", [26]).affected == 1
    assert db.sql("UPDATE users SET age = 1 WHERE name = 'nobody'").affected == 0

    # SQL and the Python API see the same tables.
    assert db.count("users") == 2
    assert db.table_schema("users").column("token").type == "uuid"

    with pytest.raises(bkndb.QueryError, match="expected"):
        db.sql("SELEC 1")
    with pytest.raises(ValueError):  # QueryError is also a ValueError
        db.sql("SELECT * FROM users WHERE age = ?")
    with pytest.raises(TypeError):
        db.sql("SELECT * FROM users WHERE name = ?", "Ana")


def test_sql_in_transactions(db: bkndb.Database) -> None:
    db.sql("CREATE TABLE t (id INT PRIMARY KEY, v TEXT)")
    with db.transaction() as tx:
        tx.sql("INSERT INTO t VALUES (1, 'a')")
        assert tx.sql("SELECT COUNT(*) FROM t").scalar() == 1
        assert db.sql("SELECT COUNT(*) FROM t").scalar() == 0  # not committed yet
    assert db.sql("SELECT v FROM t").scalar() == "a"

    with pytest.raises(bkndb.DuplicateKeyError):
        with db.transaction() as tx:
            tx.sql("INSERT INTO t VALUES (2, 'b')")
            tx.sql("INSERT INTO t VALUES (1, 'dup')")
    assert db.sql("SELECT COUNT(*) FROM t").scalar() == 1


def test_graph_query(db: bkndb.Database) -> None:
    ana = db.create_node("Person", {"name": "Ana", "age": 30})
    budi = db.create_node("Person", {"name": "Budi", "age": 25})
    acme = db.create_node("Company", {"name": "Acme"})
    db.create_edge(ana, "KNOWS", budi, {"since": 2020})
    db.create_edge(ana, "WORKS_AT", acme)
    db.create_edge(budi, "WORKS_AT", acme)

    r = db.graph_query("MATCH (a:Person {name: $n})-[k:KNOWS]->(b) RETURN b.name, k.since", {"n": "Ana"})
    assert r.rows == [("Budi", 2020)]
    coworkers = db.graph_query(
        "MATCH (p:Person)-[:WORKS_AT]->(c:Company) RETURN c.name AS company, collect(p.name) AS people"
    )
    assert coworkers.dicts() == [{"company": "Acme", "people": ["Ana", "Budi"]}]
    node = db.graph_query("MATCH (n) WHERE id(n) = $1 RETURN n", [acme]).scalar()
    assert node == {"id": acme, "label": "Company", "properties": {"name": "Acme"}}

    with db.transaction() as tx:
        cici = tx.create_node("Person", {"name": "Cici"})
        tx.create_edge(budi, "KNOWS", cici)
        names = tx.graph_query("MATCH ({name: 'Ana'})-[:KNOWS*2]->(x) RETURN x.name").column("x.name")
        assert names == ["Cici"]

    with pytest.raises(bkndb.QueryError, match="MATCH"):
        db.graph_query("CREATE (n)")
