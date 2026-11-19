//! Deterministic native/WASM interchange fixture, exercised by scripts/go/wasm.py.
use bkndb_core::{Db, StorageBackend, StorageWriteTx, TableSpec, graph::NodeId};
use bkndb_storage_lsm::LsmStorageBackend;
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let db = Db::new(LsmStorageBackend::open(&args[2]).unwrap());
    let high = 9_007_199_254_741_037u64;
    if args[1] == "hold" {
        use std::io::Write;
        println!("READY");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        return;
    }
    if args[1] == "seed" {
        let mut tx = db.backend().begin_write().unwrap();
        tx.put(TableSpec("meta"), b"next_node_id", &high.to_be_bytes())
            .unwrap();
        tx.commit().unwrap();
        let id = db
            .graph()
            .create_node("Native", Default::default())
            .unwrap();
        assert_eq!(id.0, high);
        db.relational()
            .sql(
                "CREATE TABLE exchange (id INT PRIMARY KEY, text TEXT, embedding LIST)",
                (),
            )
            .unwrap();
        db.relational()
            .sql(
                "INSERT INTO exchange VALUES (1, 'native hello', [1.0,0.0])",
                (),
            )
            .unwrap();
        db.relational()
            .create_fulltext_index("exchange", "text")
            .unwrap();
        db.relational()
            .create_vector_index(
                "exchange",
                "embedding",
                bkndb_core::relational::VectorIndexOptions {
                    metric: bkndb_core::relational::VectorMetric::Cosine,
                    m: 16,
                    ef_construction: 200,
                },
            )
            .unwrap();
        db.backend().force_compact().unwrap();
    } else {
        assert_eq!(
            db.graph().get_node(NodeId(high)).unwrap().unwrap().label,
            "Native"
        );
        assert_eq!(
            db.graph()
                .get_node(NodeId(high + 1))
                .unwrap()
                .unwrap()
                .label,
            "WASM"
        );
        assert_eq!(
            db.relational()
                .sql("SELECT * FROM exchange ORDER BY id", ())
                .unwrap()
                .rows
                .len(),
            2
        );
        assert_eq!(
            db.relational()
                .search_vector(
                    "exchange",
                    "embedding",
                    &[1.0, 0.0],
                    2,
                    bkndb_core::relational::VectorMetric::Cosine,
                    None
                )
                .unwrap()
                .len(),
            2
        );
        db.backend().verify_integrity().unwrap();
    }
}
