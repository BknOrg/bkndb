use crate::{
    args::*,
    backend::Backend as Storage,
    checkpoint,
    error::{Error, Result},
    files::{self, Database},
    maintenance, query,
};
use bkndb::{StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Read, ops::Bound, path::Path, time::Instant};
pub fn read_text(path: &Path, max: usize) -> Result<String> {
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_string(&mut text)?;
    if text.len() > max {
        return Err(Error::new(9, "input exceeds configured limit"));
    }
    Ok(text)
}
pub fn doctor(source: &Source) -> Result<Value> {
    match Database::open(source, false)
        .and_then(|db| maintenance::check(&db).map(|report| (db.recovery_required, report)))
    {
        Ok((false, r)) => Ok(
            json!({"healthy_within_checked_scope":true,"check":r,"recommendations":["Keep verified backups; this does not establish full engine stability"]}),
        ),
        Ok((true, r)) => Err(Error::new(
            7,
            format!(
                "source requires recovery; original preserved. Create a verified backup to a new file. Details: {r}"
            ),
        )),
        Err(e) => Err(Error::new(
            e.code,
            format!(
                "{}; preserve the source and operational files. For LSM corruption, try explicit repair to a new destination; no automatic repair was performed",
                e.message
            ),
        )),
    }
}
pub fn capabilities(path: Option<&Path>) -> Result<Value> {
    let detected = path.map(files::detect).transpose()?;
    Ok(
        json!({"backend":detected,"cli_candidate":true,"offline":true,"backends":["redb","lsm"],"repair":{"redb":false,"lsm":"validated roots/SSTs; WAL prefix salvage only"},"inspection":{"lsm":"read-only replay; lock sidecar may be created","redb":"locked source copied to private scratch"},"formats":{"lsm_read":[1,2,3],"lsm_write":3,"dump":1},"limits":{"dump_record_bytes":16*1024*1024,"input_record_bytes":32*1024*1024,"sql_bytes":16*1024*1024,"query_result_limit":1000000},"online_administration":false,"browser":false,"network_server":false}),
    )
}
pub fn debug(command: &DebugCommand) -> Result<Value> {
    let source = match command {
        DebugCommand::Header(s)
        | DebugCommand::Manifest(s)
        | DebugCommand::Wal(s)
        | DebugCommand::Sstable(s) => s,
    };
    let db = Database::open(source, false)?;
    let Storage::Lsm(b) = db.db.backend() else {
        return Err(Error::unsupported(
            "structural debug commands support LSM only",
        ));
    };
    let d = b.engine().diagnostics()?;
    Ok(match command {
        DebugCommand::Header(_) => {
            json!({"format_version":d.format_version,"selected_slot":d.selected_header_slot,"sequence":d.header_sequence,"manifest_offset":d.manifest_offset,"manifest_bytes":d.manifest_bytes})
        }
        DebugCommand::Manifest(_) => json!({"wal_start":d.wal_start,"sstables":d.sstables}),
        DebugCommand::Wal(_) => {
            json!({"start":d.wal_start,"valid_end":d.valid_wal_end,"file_bytes":d.file_bytes,"records":d.wal_records,"entries":d.wal_entries,"trailing_bytes":d.file_bytes-d.valid_wal_end})
        }
        DebugCommand::Sstable(_) => {
            json!({"sstables":d.sstables,"checks":b.verify_integrity()?.blocks_verified})
        }
    })
}
const KV_TABLE: TableSpec = TableSpec("__bkndb_cli_kv_v1");
fn prefix(namespace: &str) -> Result<Vec<u8>> {
    if namespace.is_empty()
        || namespace.len() > 64
        || !namespace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::input(
            "namespace must contain 1..64 ASCII letters, digits, '_' or '-'",
        ));
    }
    let mut p = vec![namespace.len() as u8];
    p.extend_from_slice(namespace.as_bytes());
    Ok(p)
}
fn decode_key(prefix: &[u8], text: &str) -> Result<Vec<u8>> {
    let mut out = prefix.to_vec();
    if let Some(s) = text.strip_prefix("hex:") {
        out.extend(hex::decode(s).map_err(|e| Error::input(e.to_string()))?);
    } else if let Some(s) = text.strip_prefix("utf8:") {
        out.extend(s.as_bytes());
    } else {
        return Err(Error::input(
            "keys/values require an explicit utf8: or hex: prefix",
        ));
    }
    if out.len() > 16 * 1024 * 1024 {
        return Err(Error::new(9, "KV input exceeds 16 MiB"));
    }
    Ok(out)
}
pub fn kv(command: &Kv, write: bool) -> Result<Value> {
    let (s, mutation) = match command {
        Kv::Get { source, .. } | Kv::Scan { source, .. } => (source, false),
        Kv::Put { source, .. } | Kv::Delete { source, .. } => (source, true),
    };
    let prefix = prefix(&s.namespace)?;
    if mutation {
        query::require_write(write)?;
    }
    let db = Database::open(&s.source, mutation)?;
    match command {
        Kv::Get { key, .. } => Ok(
            json!({"value_hex":db.db.backend().begin_read()?.get(KV_TABLE,&decode_key(&prefix,key)?)?.map(hex::encode)}),
        ),
        Kv::Put { key, value, .. } => {
            let key = decode_key(&prefix, key)?;
            let value = decode_key(&[], value)?;
            let mut tx = db.db.backend().begin_write()?;
            tx.put(KV_TABLE, &key, &value)?;
            checkpoint()?;
            tx.commit()?;
            Ok(json!({"stored":true}))
        }
        Kv::Delete { key, .. } => {
            let key = decode_key(&prefix, key)?;
            let mut tx = db.db.backend().begin_write()?;
            tx.delete(KV_TABLE, &key)?;
            checkpoint()?;
            tx.commit()?;
            Ok(json!({"deleted":true}))
        }
        Kv::Scan { limit, .. } => {
            query::limit_check(*limit)?;
            let tx = db.db.backend().begin_read()?;
            let mut rows = Vec::new();
            for entry in tx.scan(KV_TABLE, Bound::Included(&prefix), Bound::Unbounded)? {
                checkpoint()?;
                if rows.len() >= *limit {
                    break;
                }
                let (k, v) = entry?;
                if !k.starts_with(&prefix) {
                    break;
                }
                rows.push(
                    json!({"key_hex":hex::encode(&k[prefix.len()..]),"value_hex":hex::encode(v)}),
                );
            }
            Ok(json!(rows))
        }
    }
}
fn random(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *seed
}
fn quantiles(samples: &mut [u128]) -> Value {
    samples.sort_unstable();
    if samples.is_empty() {
        return Value::Null;
    }
    let at = |p: usize| samples[((samples.len() - 1) * p) / 100] as f64 / 1000.0;
    json!({"p50_us":at(50),"p95_us":at(95),"p99_us":at(99)})
}
pub fn workload(w: &Workload, stress: bool) -> Result<Value> {
    maintenance::create(&w.file, w.backend)?;
    let source = Source {
        file: w.file.clone(),
        backend: Some(w.backend),
    };
    let mut db = Database::open(&source, true)?;
    let table = TableSpec("cli_workload");
    let mut model = BTreeMap::new();
    let mut seed = w.seed;
    let start = Instant::now();
    let mut latencies = Vec::new();
    let mut read_latencies = Vec::new();
    let mut done = 0u64;
    let mut reopen_count = 0;
    while done < w.operations {
        checkpoint()?;
        let mut tx = db.db.backend().begin_write()?;
        let batch = 1000.min(w.operations - done);
        let rollback = stress && random(&mut seed) % 17 == 0;
        let mut pending = model.clone();
        for _ in 0..batch {
            let number = random(&mut seed);
            let key = (number % 10000).to_be_bytes().to_vec();
            if stress && number % 5 == 0 {
                tx.delete(table, &key)?;
                pending.remove(&key);
            } else {
                let value = number.to_le_bytes().repeat(4);
                tx.put(table, &key, &value)?;
                pending.insert(key, value);
            }
        }
        let commit = Instant::now();
        if rollback {
            drop(tx);
        } else {
            tx.commit()?;
            model = pending;
            latencies.push(commit.elapsed().as_nanos());
        }
        done += batch;
        if stress {
            let read = Instant::now();
            let tx = db.db.backend().begin_read()?;
            let found: BTreeMap<_, _> = tx
                .scan(table, Bound::Unbounded, Bound::Unbounded)?
                .collect::<std::result::Result<_, _>>()?;
            if found != model {
                return Err(Error::new(
                    5,
                    format!(
                        "reference model mismatch at operation {done}, seed {}",
                        w.seed
                    ),
                ));
            }
            read_latencies.push(read.elapsed().as_nanos());
            drop(tx);
            if done % 10000 == 0 {
                match db.db.backend() {
                    Storage::Lsm(b) => b.force_compact()?,
                    Storage::Redb(b) => b.force_compact()?,
                }
                drop(db);
                db = Database::open(&source, true)?;
                reopen_count += 1;
            }
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let compact = Instant::now();
    let peak = match db.db.backend() {
        Storage::Lsm(b) => {
            b.force_compact()?;
            Some(b.peak_compaction_disk_bytes())
        }
        Storage::Redb(b) => {
            b.force_compact()?;
            None
        }
    };
    let compact_seconds = compact.elapsed().as_secs_f64();
    let tx = db.db.backend().begin_read()?;
    let found: BTreeMap<_, _> = tx
        .scan(table, Bound::Unbounded, Bound::Unbounded)?
        .collect::<std::result::Result<_, _>>()?;
    if found != model {
        return Err(Error::new(5, "final workload model mismatch"));
    }
    drop(tx);
    drop(db);
    maintenance::validate(&w.file)?;
    Ok(
        json!({"version":env!("CARGO_PKG_VERSION"),"backend":w.backend,"seed":w.seed,"operations":done,"batch_operations":1000,"seconds":elapsed,"operations_per_second":done as f64/elapsed,"commit_batch_latency":quantiles(&mut latencies),"full_scan_latency":quantiles(&mut read_latencies),"reopens":reopen_count,"final_entries":model.len(),"file_bytes":std::fs::metadata(&w.file)?.len(),"final_compaction_seconds":compact_seconds,"compaction_peak_current_plus_temp_bytes":peak,"peak_rss_kib":peak_rss(),"workload":"synthetic 10000-key KV, 32-byte values; not a production performance claim"}),
    )
}
fn peak_rss() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        s.lines()
            .find(|l| l.starts_with("VmHWM:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
