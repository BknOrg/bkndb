//! Maintenance: forced compaction, backup, statistics and integrity checks.
use super::*;

impl LsmStorageBackend {
    /// Forces an immediate full compaction regardless of
    /// `compaction_trigger_files`. Unlike the internal auto-compact call
    /// from `flush()` (which inherits `writer_lock` from the enclosing
    /// write-tx guard), this is an external entry point and must acquire
    /// the lock itself.
    ///
    /// Flushes the active memtable first: `compact_all` rebuilds the file
    /// from SSTables alone, starting a fresh empty WAL region, so any commit
    /// still living only in the memtable/WAL would otherwise be lost on the
    /// next reopen.
    pub fn force_compact(&self) -> Result<(), BknError> {
        let _guard = self.writer_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.flush()?;
        self.compact_all(true)
    }

    pub fn sstable_count(&self) -> usize {
        self.state.read().unwrap_or_else(|e| e.into_inner()).sstables.len()
    }

    /// Path of the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes a consistent, compacted copy of the database — everything
    /// committed when the call starts — to a new file at `dest`, which opens
    /// like any other `.bkndb` file.
    ///
    /// Online: it reads from a snapshot, so readers and writers carry on
    /// meanwhile (commits made after the call started just aren't in the
    /// copy). The copy is written to `dest` + `.backup.tmp`, fsynced, then
    /// renamed into place, so a crash never leaves a half-written file
    /// under `dest`. Fails if `dest` already exists.
    pub fn backup_to(&self, dest: impl AsRef<Path>) -> Result<(), BknError> {
        let dest = dest.as_ref();
        if dest.exists() {
            return Err(BknError::Backend(format!("backup target '{}' already exists", dest.display())));
        }
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(io_err)?;
        }
        let snapshot = self.snapshot();
        let expected = snapshot.active_memtable.len() as u64
            + snapshot.immutable_memtables.iter().map(|m| m.len() as u64).sum::<u64>()
            + snapshot.sstables.iter().map(|s| s.estimated_entries()).sum::<u64>();
        let sources = scan_sources(&snapshot, None, &Bound::Unbounded, &Bound::Unbounded);

        let tmp = with_suffix(dest, ".backup.tmp");
        drop(write_fresh_container(&tmp, 1, MergeIter::new(sources), expected as usize, self.write_options())?);
        fs::rename(&tmp, dest).map_err(io_err)?;
        sync_parent_dir(dest)
    }

    /// Current size figures for the file and the in-memory write buffer.
    pub fn stats(&self) -> Result<LsmStats, BknError> {
        // Lock order matches `flush`: container, then state.
        let cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
        let file_bytes = cw.file.metadata().map_err(io_err)?.len();
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        let (memtable_entries, memtable_bytes) = std::iter::once(&state.active_memtable)
            .chain(&state.immutable_memtables)
            .fold((0, 0), |(n, b), m| (n + m.len(), b + memtable_byte_size(m)));
        let sstable_bytes: u64 = state.sstables.iter().map(|s| s.blob_len).sum();
        let wal_bytes = file_bytes.saturating_sub(state.wal_region_start);
        Ok(LsmStats {
            file_bytes,
            sstable_count: state.sstables.len(),
            legacy_sstable_count: state.sstables.iter().filter(|s| s.is_legacy()).count(),
            sstable_bytes,
            sstable_entries: state.sstables.iter().map(|s| s.estimated_entries()).sum(),
            memtable_entries,
            memtable_bytes,
            wal_bytes,
            reclaimable_bytes: file_bytes.saturating_sub(container::BODY_START + sstable_bytes + state.manifest_len + wal_bytes),
        })
    }

    /// Re-reads every stored byte and checks it: the header's manifest
    /// checksum, every SSTable block's checksum and encoding, and every WAL
    /// frame. Returns what was checked, or [`BknError::Corruption`] naming
    /// the first bad structure. SSTables are checked from a snapshot, so
    /// only the (short) WAL re-read briefly holds up commits.
    pub fn verify_integrity(&self) -> Result<IntegrityReport, BknError> {
        let snapshot = self.snapshot();
        let mut report = IntegrityReport::default();
        for sst in &snapshot.sstables {
            let counts = sst.verify()?;
            report.sstables_checked += 1;
            report.blocks_verified += counts.blocks_verified;
            report.legacy_blocks_unchecked += counts.blocks_unchecked;
            report.entries += counts.entries;
        }

        let cw = self.container.lock().unwrap_or_else(|e| e.into_inner());
        let slot = container::read_header(&cw.file)?.ok_or_else(|| BknError::Corruption("database header is missing".to_string()))?;
        Manifest::decode(&container::read_manifest(&cw.file, &slot)?).map_err(manifest_corrupt)?;
        let wal_start = self.state.read().unwrap_or_else(|e| e.into_inner()).wal_region_start;
        let (records, valid_end) = wal::replay_from(&cw.file, wal_start)?;
        let file_len = cw.file.metadata().map_err(io_err)?.len();
        if valid_end != file_len {
            return Err(BknError::Corruption(format!(
                "write-ahead log has {} trailing bytes that are not a valid frame",
                file_len - valid_end
            )));
        }
        report.wal_records = records.len() as u64;
        Ok(report)
    }
}
