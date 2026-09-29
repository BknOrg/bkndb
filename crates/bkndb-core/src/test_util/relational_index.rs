//! Secondary indexes and graph + relational hybrid lookups.
use super::*;

/// Conformance suite for secondary index prefix searching, range scanning,
/// index cleanup on update/delete, and hybrid graph-relational joins.
#[cfg(all(feature = "graph", feature = "relational"))]
pub fn relational_indexing_and_hybrid_suite<B: StorageBackend>(backend: B) {
    use crate::BknError;
    use crate::db::Db;
    use crate::graph::{Direction, NodeId, Properties as GraphProperties};
    use crate::value::{PropValue, Properties};

    let db = Db::new(backend);

    // 1. Setup graph nodes & relational rows atomically in write_tx
    let (f1, f2, f3) = db
        .write_tx(|tx| {
            let n1 = tx.graph().create_node("File", GraphProperties::new())?;
            let n2 = tx.graph().create_node("File", GraphProperties::new())?;
            let n3 = tx.graph().create_node("File", GraphProperties::new())?;

            // Edges: n1 -> n2 -> n3 (calls)
            tx.graph()
                .create_edge(n1, "calls", n2, GraphProperties::new())?;
            tx.graph()
                .create_edge(n2, "calls", n3, GraphProperties::new())?;

            // Direct PK relational table (FILES)
            let mut r1 = Properties::new();
            r1.insert(
                "path".to_string(),
                PropValue::Str("src/main.rs".to_string()),
            );
            r1.insert("size".to_string(), PropValue::Int(150));
            tx.relational()
                .table(&HYBRID_FILES)
                .insert_with_pk(PropValue::Int(n1.0 as i64), r1)?;

            let mut r2 = Properties::new();
            r2.insert("path".to_string(), PropValue::Str("src/lib.rs".to_string()));
            r2.insert("size".to_string(), PropValue::Int(300));
            tx.relational()
                .table(&HYBRID_FILES)
                .insert_with_pk(PropValue::Int(n2.0 as i64), r2)?;

            let mut r3 = Properties::new();
            r3.insert(
                "path".to_string(),
                PropValue::Str("tests/integration.rs".to_string()),
            );
            r3.insert("size".to_string(), PropValue::Int(500));
            tx.relational()
                .table(&HYBRID_FILES)
                .insert_with_pk(PropValue::Int(n3.0 as i64), r3)?;

            // Foreign Key relational table (SYMBOLS)
            let mut s1 = Properties::new();
            s1.insert("file_node_id".to_string(), PropValue::Int(n1.0 as i64));
            s1.insert("name".to_string(), PropValue::Str("parse_ast".to_string()));
            tx.relational().table(&HYBRID_SYMBOLS).insert(s1)?;

            let mut s2 = Properties::new();
            s2.insert("file_node_id".to_string(), PropValue::Int(n1.0 as i64));
            s2.insert("name".to_string(), PropValue::Str("parse_expr".to_string()));
            tx.relational().table(&HYBRID_SYMBOLS).insert(s2)?;

            let mut s3 = Properties::new();
            s3.insert("file_node_id".to_string(), PropValue::Int(n2.0 as i64));
            s3.insert("name".to_string(), PropValue::Str("eval".to_string()));
            tx.relational().table(&HYBRID_SYMBOLS).insert(s3)?;

            Ok::<_, BknError>((n1, n2, n3))
        })
        .unwrap();

    // 2. Secondary Index Prefix Search
    db.read_tx(|tx| {
        let table = tx.relational().table(&HYBRID_FILES);

        // Prefix "src/" matches src/main.rs and src/lib.rs
        let src_files = table.select_prefix("path", "src/")?;
        assert_eq!(src_files.len(), 2);

        // Prefix "tests/" matches tests/integration.rs
        let test_files = table.select_prefix("path", "tests/")?;
        assert_eq!(test_files.len(), 1);
        assert_eq!(
            test_files[0].values.get("path"),
            Some(&PropValue::Str("tests/integration.rs".to_string()))
        );

        // Prefix "nonexistent" matches 0
        let non_files = table.select_prefix("path", "nonexistent")?;
        assert_eq!(non_files.len(), 0);

        // Empty prefix matches all
        let all_files = table.select_prefix("path", "")?;
        assert_eq!(all_files.len(), 3);

        // Prefix on symbols: "parse_" matches 2 symbols
        let sym_table = tx.relational().table(&HYBRID_SYMBOLS);
        let parse_syms = sym_table.select_prefix("name", "parse_")?;
        assert_eq!(parse_syms.len(), 2);

        // SelectQuery where_prefix with limit
        let query_files = db
            .relational()
            .table(&HYBRID_FILES)
            .select()
            .where_prefix("path", "src/")
            .limit(1)
            .run()?;
        assert_eq!(query_files.len(), 1);

        Ok::<_, BknError>(())
    })
    .unwrap();

    // 3. Secondary Index Range Scan
    db.read_tx(|tx| {
        let table = tx.relational().table(&HYBRID_FILES);
        let ranged = table.select_range(
            "size",
            &Bound::Included(PropValue::Int(200)),
            &Bound::Included(PropValue::Int(600)),
        )?;
        assert_eq!(ranged.len(), 2); // 300 and 500
        Ok::<_, BknError>(())
    })
    .unwrap();

    // 4. Index Maintenance: Update and Delete cleans index
    db.write_tx(|tx| {
        let mut rel = tx.relational();
        let mut table = rel.table(&HYBRID_FILES);
        // Change path of f1
        table.update(&PropValue::Int(f1.0 as i64), |props| {
            props.insert(
                "path".to_string(),
                PropValue::Str("src/renamed_main.rs".to_string()),
            );
        })?;
        Ok::<_, BknError>(())
    })
    .unwrap();

    db.read_tx(|tx| {
        let table = tx.relational().table(&HYBRID_FILES);
        // Old prefix "src/main" should now return 0
        let old = table.select_prefix("path", "src/main")?;
        assert_eq!(old.len(), 0);

        // New prefix "src/renamed" should return 1
        let new = table.select_prefix("path", "src/renamed")?;
        assert_eq!(new.len(), 1);
        Ok::<_, BknError>(())
    })
    .unwrap();

    // 5. Hybrid Direct PK Join (join_nodes_with_table)
    db.read_tx(|tx| {
        let joined = tx.join_nodes_with_table(&[f1, f2, NodeId(8888)], &HYBRID_FILES)?;
        assert_eq!(joined.len(), 3);

        // f1
        assert_eq!(joined[0].id, f1);
        assert!(joined[0].node.is_some());
        assert_eq!(joined[0].node.as_ref().unwrap().label, "File");
        assert_eq!(
            joined[0].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("src/renamed_main.rs".to_string()))
        );

        // f2
        assert_eq!(joined[1].id, f2);
        assert!(joined[1].node.is_some());
        assert_eq!(
            joined[1].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("src/lib.rs".to_string()))
        );

        // invalid NodeId(8888)
        assert_eq!(joined[2].id, NodeId(8888));
        assert!(joined[2].node.is_none());
        assert!(joined[2].row.is_none());

        Ok::<_, BknError>(())
    })
    .unwrap();

    // 6. Hybrid Foreign Key Join (join_nodes_by_column)
    db.read_tx(|tx| {
        let joined = tx.join_nodes_by_column(&[f1, f2, f3], &HYBRID_SYMBOLS, "file_node_id")?;
        assert_eq!(joined.len(), 3);

        // f1 has 2 symbols
        assert_eq!(joined[0].id, f1);
        assert_eq!(joined[0].rows.len(), 2);

        // f2 has 1 symbol
        assert_eq!(joined[1].id, f2);
        assert_eq!(joined[1].rows.len(), 1);
        assert_eq!(
            joined[1].rows[0].values.get("name"),
            Some(&PropValue::Str("eval".to_string()))
        );

        // f3 has 0 symbols
        assert_eq!(joined[2].id, f3);
        assert_eq!(joined[2].rows.len(), 0);

        Ok::<_, BknError>(())
    })
    .unwrap();

    // 7. Reverse Join (join_rows_with_nodes)
    db.read_tx(|tx| {
        let sym_table = tx.relational().table(&HYBRID_SYMBOLS);
        let parse_rows = sym_table.select_prefix("name", "parse_")?;
        assert_eq!(parse_rows.len(), 2);

        let reverse_joined =
            tx.join_rows_with_nodes(&parse_rows, &HYBRID_SYMBOLS, "file_node_id")?;
        assert_eq!(reverse_joined.len(), 2);
        for (_row, node_opt) in reverse_joined {
            let node = node_opt.expect("must resolve to f1 node");
            assert_eq!(node.label, "File");
        }

        Ok::<_, BknError>(())
    })
    .unwrap();

    // 8. Graph Traversal + Hybrid Join
    db.read_tx(|tx| {
        let path = tx
            .graph()
            .find_shortest_path(f1, f3, Direction::Out, None)?
            .expect("path exists");
        let nodes = path.nodes();
        assert_eq!(nodes, vec![f1, f2, f3]);

        let joined_path = tx.join_nodes_with_table(&nodes, &HYBRID_FILES)?;
        assert_eq!(joined_path.len(), 3);
        assert_eq!(
            joined_path[0].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("src/renamed_main.rs".to_string()))
        );
        assert_eq!(
            joined_path[1].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("src/lib.rs".to_string()))
        );
        assert_eq!(
            joined_path[2].row.as_ref().unwrap().values.get("path"),
            Some(&PropValue::Str("tests/integration.rs".to_string()))
        );

        Ok::<_, BknError>(())
    })
    .unwrap();
}
