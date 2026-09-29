//! Graph label/property indexes, weighted paths and the MATCH query language.
use super::*;

/// Label and property indexes: lookups, maintenance on every node write,
/// and the fallback + rebuild path for files created before the indexes
/// existed.
pub fn graph_index_suite<B: StorageBackend>(backend: B) {
    use std::sync::Arc;

    use crate::graph::{Direction, GraphDb};
    use crate::value::{PropValue, Properties};

    fn props(pairs: &[(&str, PropValue)]) -> Properties {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    let backend = Arc::new(backend);
    let g = GraphDb::from_arc(backend.clone());
    let alice = g.create_node("Person", props(&[("name", "alice".into()), ("age", 30.into())])).unwrap();
    let bob = g.create_node("Person", props(&[("name", "bob".into()), ("age", 30.into())])).unwrap();
    let bulk = g
        .create_nodes_bulk([
            ("Person", props(&[("name", "carol".into()), ("age", 41.into())])),
            ("City", props(&[("name", "Jakarta".into())])),
        ])
        .unwrap();
    let (carol, jakarta) = (bulk[0], bulk[1]);
    g.create_edge(alice, "LIVES_IN", jakarta, Properties::new()).unwrap();
    g.create_edge(bob, "LIVES_IN", jakarta, Properties::new()).unwrap();

    assert_eq!(g.nodes_by_label("Person").unwrap(), vec![alice, bob, carol]);
    assert_eq!(g.nodes_by_label("City").unwrap(), vec![jakarta]);
    assert!(g.nodes_by_label("Pers").unwrap().is_empty(), "labels match exactly, not by prefix");

    // Without a property index: label scan + filter.
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![alice, bob]);
    assert!(g.find_nodes("City", "age", &30.into()).unwrap().is_empty());

    // With one: backfilled on creation, then maintained by writes.
    assert!(g.create_property_index("Person", "age").unwrap());
    assert!(!g.create_property_index("Person", "age").unwrap());
    assert_eq!(g.property_indexes().unwrap(), vec![("Person".to_string(), "age".to_string())]);
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![alice, bob]);
    g.update_node_properties(bob, |p| {
        p.insert("age".into(), 31.into());
    })
    .unwrap();
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![alice]);
    assert_eq!(g.find_nodes("Person", "age", &31.into()).unwrap(), vec![bob]);
    let dave = g.create_node("Person", props(&[("age", 30.into())])).unwrap();
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![alice, dave]);
    g.delete_node(alice).unwrap();
    assert_eq!(g.find_nodes("Person", "age", &30.into()).unwrap(), vec![dave]);
    assert_eq!(g.nodes_by_label("Person").unwrap(), vec![bob, carol, dave]);
    // Values the index can't hold still work (scan fallback).
    let eve = g.create_node("Person", props(&[("age", PropValue::Float(30.5))])).unwrap();
    assert_eq!(g.find_nodes("Person", "age", &PropValue::Float(30.5)).unwrap(), vec![eve]);

    // Batch writes maintain the indexes too, and see their own writes.
    g.write_tx(|tx| {
        let mut b = tx.graph();
        let frank = b.create_node("Person", props(&[("age", 30.into())]))?;
        assert_eq!(b.find_nodes("Person", "age", &30.into())?, vec![dave, frank]);
        b.delete_node(dave)?;
        assert_eq!(b.find_nodes("Person", "age", &30.into())?, vec![frank]);
        Ok(())
    })
    .unwrap();

    // top_hubs restricted to a label.
    let hubs = g.top_hubs(5, Direction::In, Some("City")).unwrap();
    assert_eq!(hubs, vec![(jakarta, 1)], "alice's edge went with her");

    assert!(g.drop_property_index("Person", "age").unwrap());
    assert!(g.property_indexes().unwrap().is_empty());
    assert_eq!(g.find_nodes("Person", "age", &31.into()).unwrap(), vec![bob]);

    // Simulate a file from before the label index existed: no marker and
    // no entries. Lookups must stay correct (full scan), and a rebuild
    // restores the index.
    {
        let mut w = backend.begin_write().unwrap();
        w.delete(TableSpec("meta"), b"graph:label_index").unwrap();
        for (k, _) in w.range(TableSpec("node_labels"), Bound::Unbounded, Bound::Unbounded).unwrap() {
            w.delete(TableSpec("node_labels"), &k).unwrap();
        }
        w.commit().unwrap();
    }
    let people = g.nodes_by_label("Person").unwrap();
    assert!(people.contains(&bob) && people.contains(&carol) && !people.contains(&alice));
    g.rebuild_indexes().unwrap();
    assert_eq!(g.nodes_by_label("Person").unwrap(), people);
    let r = backend.begin_read().unwrap();
    assert!(!r.range(TableSpec("node_labels"), Bound::Unbounded, Bound::Unbounded).unwrap().is_empty());
}

/// Dijkstra over edge weights.
pub fn graph_weighted_path_suite<B: StorageBackend>(backend: B) {
    use crate::graph::{Direction, GraphDb};
    use crate::value::{PropValue, Properties};

    fn w(v: PropValue) -> Properties {
        [("cost".to_string(), v)].into_iter().collect()
    }

    let g = GraphDb::new(backend);
    let [a, b, c, d] = ["a", "b", "c", "d"].map(|n| g.create_node(n, Properties::new()).unwrap());
    let ab = g.create_edge(a, "ROAD", b, w(1.into())).unwrap();
    let bc = g.create_edge(b, "ROAD", c, w(PropValue::Float(1.5))).unwrap();
    g.create_edge(a, "ROAD", c, w(5.into())).unwrap();
    g.create_edge(c, "FERRY", d, Properties::new()).unwrap(); // no weight: default

    // BFS picks the direct edge; Dijkstra the cheaper detour.
    assert_eq!(g.find_shortest_path(a, c, Direction::Out, None).unwrap().unwrap().nodes(), vec![a, c]);
    let best = g.find_weighted_path(a, c, Direction::Out, None, "cost", 1.0).unwrap().unwrap();
    assert_eq!(best.path.nodes(), vec![a, b, c]);
    assert_eq!(best.path.edges(), vec![ab, bc]);
    assert_eq!(best.cost, 2.5);

    let to_d = g.find_weighted_path(a, d, Direction::Out, None, "cost", 10.0).unwrap().unwrap();
    assert_eq!(to_d.cost, 12.5);
    assert!(g.find_weighted_path(a, d, Direction::Out, Some(&["ROAD"]), "cost", 1.0).unwrap().is_none());
    assert!(g.find_weighted_path(d, a, Direction::Out, None, "cost", 1.0).unwrap().is_none());
    assert_eq!(g.find_weighted_path(d, a, Direction::Both, None, "cost", 1.0).unwrap().unwrap().cost, 3.5);
    assert_eq!(g.find_weighted_path(a, a, Direction::Out, None, "cost", 1.0).unwrap().unwrap().cost, 0.0);

    let e = g.create_node("e", Properties::new()).unwrap();
    g.create_edge(d, "ROAD", e, w((-1).into())).unwrap();
    assert!(g.find_weighted_path(a, e, Direction::Out, None, "cost", 1.0).is_err(), "negative weights are rejected");
    assert!(g.find_weighted_path(a, c, Direction::Out, None, "cost", -1.0).is_err());
}

/// `MATCH` pattern queries over the graph.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn graph_query_suite<B: StorageBackend>(backend: B) {
    use std::collections::BTreeMap;

    use crate::graph::{GraphDb, Properties};
    use crate::lang::Params;
    use crate::value::PropValue;
    use crate::BknError;

    let db = GraphDb::new(backend);
    let person = |name: &str, age: i64| {
        let mut p = Properties::new();
        p.insert("name".into(), name.into());
        p.insert("age".into(), age.into());
        db.create_node("Person", p).unwrap()
    };
    let company = |name: &str| {
        let mut p = Properties::new();
        p.insert("name".into(), name.into());
        db.create_node("Company", p).unwrap()
    };
    let edge = |a, t: &str, b, since: Option<i64>| {
        let mut p = Properties::new();
        if let Some(s) = since {
            p.insert("since".into(), s.into());
        }
        db.create_edge(a, t, b, p).unwrap()
    };
    let (ana, budi, citra, dewi) = (person("ana", 30), person("budi", 25), person("citra", 41), person("dewi", 19));
    let (acme, globex) = (company("Acme"), company("Globex"));
    edge(ana, "KNOWS", budi, Some(2020));
    edge(budi, "KNOWS", citra, Some(2021));
    edge(citra, "KNOWS", ana, None);
    edge(ana, "KNOWS", dewi, Some(2020));
    edge(ana, "WORKS_AT", acme, None);
    edge(budi, "WORKS_AT", acme, None);
    edge(citra, "WORKS_AT", globex, None);

    let q = |text: &str, params: Params| db.query(text, params).unwrap_or_else(|e| panic!("{text}: {e}"));
    let col0 = |text: &str, params: Params| -> Vec<PropValue> { q(text, params).rows.into_iter().map(|r| r[0].clone()).collect() };
    let strs = |v: &[&str]| v.iter().map(|s| PropValue::from(*s)).collect::<Vec<_>>();
    let none = Params::none;

    for pass in ["scan", "indexed"] {
        if pass == "indexed" {
            db.create_property_index("Person", "name").unwrap();
        }
        // Directions.
        assert_eq!(col0("MATCH (a:Person {name: 'ana'})-[:KNOWS]->(b) RETURN b.name ORDER BY b.name", none()), strs(&["budi", "dewi"]), "{pass}");
        assert_eq!(col0("MATCH (b:Person {name: $n})<-[:KNOWS]-(a) RETURN a.name", Params::named([("n", "budi")])), strs(&["ana"]));
        assert_eq!(
            col0("MATCH (a:Person)-[:KNOWS]-(b) WHERE a.name = 'ana' RETURN b.name ORDER BY b.name", none()),
            strs(&["budi", "citra", "dewi"])
        );
        assert_eq!(col0("MATCH (a {name: 'dewi'})<--(b) RETURN b.name", none()), strs(&["ana"]));
        assert_eq!(col0("MATCH (a {name: 'dewi'})--(b) RETURN b.name", none()), strs(&["ana"]));
    }

    // Variable-length paths (edges never reused, so the 3-hop cycle back to
    // ana is found but not 4+ hops).
    assert_eq!(col0("MATCH (a {name: 'ana'})-[:KNOWS*2]->(c) RETURN c.name", none()), strs(&["citra"]));
    assert_eq!(
        col0("MATCH (a {name: 'ana'})-[:KNOWS*1..3]->(c) RETURN DISTINCT c.name ORDER BY c.name", none()),
        strs(&["ana", "budi", "citra", "dewi"])
    );
    let path = q("MATCH (a {name: 'ana'})-[p:KNOWS*..2]->(c {name: 'citra'}) RETURN p", none());
    let PropValue::List(edges) = &path.rows[0][0] else { panic!("{path:?}") };
    assert_eq!(edges.len(), 2);

    // Longer patterns, cross-variable WHERE, labels, cycles.
    let coworkers = q(
        "MATCH (a:Person)-[:WORKS_AT]->(c:Company)<-[:WORKS_AT]-(b:Person) WHERE a.name < b.name RETURN a.name, b.name, c.name",
        none(),
    );
    assert_eq!(coworkers.columns, vec!["a.name", "b.name", "c.name"]);
    assert_eq!(coworkers.rows, vec![strs(&["ana", "budi", "Acme"])]);
    assert_eq!(
        col0("MATCH (a)-[:KNOWS]->(b)-[:KNOWS]->(c)-[:KNOWS]->(a) RETURN a.name ORDER BY a.name", none()),
        strs(&["ana", "budi", "citra"])
    );
    assert_eq!(col0("MATCH (a {name: 'ana'})-->(x) WHERE x:Company RETURN x.name", none()), strs(&["Acme"]));
    assert_eq!(col0("MATCH (a {name: 'ana'})-[:WORKS_AT|KNOWS]->(x) RETURN count(*)", none()), vec![PropValue::Int(3)]);

    // Aggregation.
    let per_company = q(
        "MATCH (p:Person)-[:WORKS_AT]->(c:Company) RETURN c.name AS company, count(*) AS n, collect(p.name) AS people, avg(p.age) AS age ORDER BY n DESC",
        none(),
    );
    assert_eq!(per_company.columns, vec!["company", "n", "people", "age"]);
    assert_eq!(
        per_company.rows,
        vec![
            vec!["Acme".into(), 2.into(), PropValue::List(strs(&["ana", "budi"])), 27.5.into()],
            vec!["Globex".into(), 1.into(), PropValue::List(strs(&["citra"])), 41.0.into()],
        ]
    );
    assert_eq!(q("MATCH (p:Person {name: 'zed'}) RETURN count(*)", none()).rows, vec![vec![PropValue::Int(0)]]);
    assert_eq!(
        q("MATCH (p:Person) RETURN sum(p.age), min(p.age), max(p.name)", none()).rows,
        vec![vec![115.into(), 19.into(), "dewi".into()]]
    );

    // Functions, relationship variables, whole entities.
    let rel = q("MATCH (a)-[r:KNOWS {since: 2020}]->(b) RETURN type(r), r.since, a.name, b.name ORDER BY b.name", none());
    assert_eq!(rel.rows, vec![
        vec!["KNOWS".into(), 2020.into(), "ana".into(), "budi".into()],
        vec!["KNOWS".into(), 2020.into(), "ana".into(), "dewi".into()],
    ]);
    let whole = q("MATCH (n) WHERE id(n) = $id RETURN n, label(n)", Params::named([("id", dewi.0 as i64)]));
    let mut expected = BTreeMap::new();
    expected.insert("id".to_string(), PropValue::Int(dewi.0 as i64));
    expected.insert("label".to_string(), "Person".into());
    let mut props = BTreeMap::new();
    props.insert("name".to_string(), "dewi".into());
    props.insert("age".to_string(), 19.into());
    expected.insert("properties".to_string(), PropValue::Map(props));
    assert_eq!(whole.rows, vec![vec![PropValue::Map(expected), "Person".into()]]);
    let star = q("MATCH (a {name: 'budi'})-[r:WORKS_AT]->(c) RETURN *", none());
    assert_eq!(star.columns, vec!["a", "c", "r"]);

    // Predicates, paging.
    assert_eq!(col0("MATCH (p:Person) WHERE p.name STARTS WITH 'c' OR p.name ENDS WITH 'wi' RETURN p.name ORDER BY p.name", none()), strs(&["citra", "dewi"]));
    assert_eq!(col0("MATCH (p:Person) WHERE p.name IN ['ana', 'zed'] AND NOT p.age < 20 RETURN p.name", none()), strs(&["ana"]));
    assert_eq!(col0("MATCH (p:Person) WHERE p.city IS NULL AND p.name CONTAINS 'itr' RETURN p.name", none()), strs(&["citra"]));
    assert_eq!(col0("MATCH (p:Person) WHERE p.name ILIKE 'B%' RETURN p.name", none()), strs(&["budi"]));
    assert_eq!(col0("MATCH (p:Person) RETURN p.name ORDER BY p.age DESC SKIP 1 LIMIT 2", none()), strs(&["ana", "budi"]));
    assert_eq!(q("MATCH (p:Person) RETURN p.name LIMIT 2", none()).rows.len(), 2);
    assert_eq!(col0("MATCH (p:Person) WHERE p.age > $min RETURN p.name ORDER BY p.name", Params::named([("min", 29)])), strs(&["ana", "citra"]));

    // Errors.
    for (text, needle) in [
        ("CREATE (n)", "expected MATCH"),
        ("MATCH (a) RETURN b", "'b' is not defined"),
        ("MATCH (a)-[r]->(b) RETURN label(r)", "not a node"),
        ("MATCH (a)-[r*]->(b) RETURN r.since", "variable-length"),
        ("MATCH (a)-[*1..99]->(b) RETURN a", "hop range"),
        ("MATCH (a), (b) RETURN a", "single path pattern"),
        ("MATCH (a) RETURN count(*) ORDER BY a.name", "must name a RETURN column"),
    ] {
        let err = db.query(text, ()).unwrap_err();
        assert!(matches!(err, BknError::InvalidQuery(_)), "{text}: {err}");
        assert!(err.to_string().contains(needle), "{text}: {err}");
    }

    // Inside a write batch, queries see the batch's own writes.
    db.write_tx(|tx| {
        let mut g = tx.graph();
        let mut p = Properties::new();
        p.insert("name".into(), "eka".into());
        let eka = g.create_node("Person", p)?;
        g.create_edge(eka, "KNOWS", ana, Properties::new())?;
        let r = g.query("MATCH (e {name: 'eka'})-[:KNOWS]->(x) RETURN x.name", ())?;
        assert_eq!(r.rows, vec![vec![PropValue::from("ana")]]);
        Ok(())
    })
    .unwrap();
    let _ = globex;
}
