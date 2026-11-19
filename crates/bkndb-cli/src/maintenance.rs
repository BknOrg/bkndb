use crate::{
    args::{Backend as Kind, Source},
    backend::Backend,
    checkpoint,
    error::{Error, Result},
    files::{self, Database, Stage},
};
use serde_json::{Value, json};
use std::path::Path;

pub fn info(db: &Database) -> Value {
    json!({"backend":db.kind,"source":db.path,"file_bytes":db.source_bytes,"inspection":"offline; source data preserved",
        "source_recovery_required":db.recovery_required,
        "format_version":match db.db.backend(){Backend::Lsm(b)=>json!(b.engine().format_version()),_=>json!("redb upstream")},
        "stability":"validation candidate; not a stable release"})
}
pub fn stats(db: &Database) -> Result<Value> {
    let logical = db.db.stats()?;
    let storage = match db.db.backend() {
        Backend::Lsm(b) => {
            let s = b.stats()?;
            json!({"file_bytes":s.file_bytes,"sstable_count":s.sstable_count,"sstable_bytes":s.sstable_bytes,"memtable_bytes":s.memtable_bytes,"wal_bytes":s.wal_bytes,"reclaimable_bytes":s.reclaimable_bytes})
        }
        Backend::Redb(b) => {
            let s = b.stats()?;
            json!({"file_bytes":s.file_bytes,"table_count":s.table_count,"entries":s.entries,"stored_bytes":s.stored_bytes,"reclaimable_bytes":s.reclaimable_bytes})
        }
    };
    Ok(
        json!({"backend":db.kind,"storage":storage,"logical":{"nodes":logical.nodes,"edges":logical.edges,"tables":logical.tables},"source_recovery_required":db.recovery_required}),
    )
}
pub fn check(db: &Database) -> Result<Value> {
    checkpoint()?;
    let physical = match db.db.backend() {
        Backend::Redb(b) => {
            let r = b.verify_integrity()?;
            json!({"tables_checked":r.sstables_checked,"pages_verified":r.blocks_verified,"entries":r.entries})
        }
        Backend::Lsm(b) => {
            let r = b.verify_integrity()?;
            json!({"sstables_checked":r.sstables_checked,"blocks_verified":r.blocks_verified,"legacy_blocks_unchecked":r.legacy_blocks_unchecked,"wal_records":r.wal_records,"entries":r.entries})
        }
    };
    let graph = db.db.graph().verify_integrity()?;
    let tables = db.db.relational().list_tables()?;
    let mut rows = 0usize;
    for schema in &tables {
        checkpoint()?;
        db.db.relational().scan_rows::<Error>(schema.name(), |_| {
            checkpoint()?;
            rows += 1;
            Ok(())
        })?;
        db.db.relational().fulltext_indexes(schema.name())?;
        db.db.relational().vector_indexes(schema.name())?;
    }
    Ok(
        json!({"physical":physical,"logical":{"graph":graph,"tables_checked":tables.len(),"rows_decoded":rows},
        "scope":"physical checksums/encoding, graph records/adjacency/ID counters, relational decoding/schema validation and index registries; not exhaustive semantic validation of every derived index",
        "source_recovery_required":db.recovery_required}),
    )
}
pub fn validate(path: &Path) -> Result<()> {
    let db = Database::open(&files::source(path), false)?;
    check(&db)?;
    if db.recovery_required {
        return Err(Error::new(
            7,
            "generated output unexpectedly requires recovery",
        ));
    }
    Ok(())
}
pub fn duplicate(source: &Source, output: &Path, to: Option<Kind>) -> Result<Value> {
    let source = Database::open(source, false)?;
    // Do not call upstream Redb backup's fixed temporary path. This staging
    // directory belongs exclusively to this invocation.
    check(&source)?;
    let stage = Stage::new(output)?;
    let target = to.unwrap_or(source.kind);
    match (source.db.backend(), target) {
        (Backend::Lsm(b), Kind::Lsm) => b.backup_to(&stage.path)?,
        (Backend::Redb(b), Kind::Redb) => {
            let mut f = std::fs::File::open(b.path())?;
            files::copy_file(&mut f, &stage.path)?;
            // Close/reopen the private copy to normalize a dirty snapshot.
            drop(bkndb::RedbStorageBackend::open_existing(&stage.path)?);
        }
        (Backend::Redb(b), Kind::Lsm) => bkndb::migration::redb_to_lsm(b, &stage.path)?,
        (Backend::Lsm(b), Kind::Redb) => bkndb::migration::lsm_to_redb(b, &stage.path)?,
    }
    validate(&stage.path)?;
    stage.publish()?;
    Ok(
        json!({"output":output,"backend":target,"verified":true,"source_recovery_required":source.recovery_required}),
    )
}
pub fn create(path: &Path, kind: Kind) -> Result<Value> {
    let stage = Stage::new(path)?;
    files::create(&stage.path, kind)?;
    validate(&stage.path)?;
    stage.publish()?;
    Ok(json!({"output":path,"backend":kind}))
}
pub fn repair(source: &Source, output: &Path) -> Result<Value> {
    let kind = files::detect(&source.file)?;
    if source.backend.is_some_and(|b| b != kind) {
        return Err(Error::input("--backend mismatch"));
    }
    if kind != Kind::Lsm {
        return Err(Error::unsupported(
            "salvage repair is implemented only for LSM; Redb repair is not available",
        ));
    }
    let stage = Stage::new(output)?;
    let report = bkndb::LsmStorageBackend::repair_to(&source.file, &stage.path)?;
    validate(&stage.path)?;
    stage.publish()?;
    let result = json!({"output":output,"source_bytes":report.source_bytes,"recovered_wal_records":report.recovered_wal_records,"discarded_wal_bytes":report.discarded_wal_bytes,"source_preserved":true});
    if report.discarded_wal_bytes > 0 {
        return Err(Error::new(
            10,
            "repair published recovered data; some WAL bytes were discarded",
        )
        .with_details(result));
    }
    Ok(result)
}
pub fn compact(source: &Source) -> Result<Value> {
    let db = Database::open(source, true)?;
    check(&db)?;
    match db.db.backend() {
        Backend::Redb(b) => b.force_compact()?,
        Backend::Lsm(b) => b.force_compact()?,
    }
    check(&db)?;
    Ok(json!({"compacted":true,"backend":db.kind}))
}
