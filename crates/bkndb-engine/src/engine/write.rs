//! Commit path: WAL append, memtable flush and compaction.
use super::*;

impl LsmStorageBackend {
    pub(super) fn write_options(&self) -> WriteOptions {
        WriteOptions {
            block_size: self.options.block_size_bytes,
            compress: self.options.compression,
        }
    }

    pub(super) fn snapshot(&self) -> Result<ReadSnapshot, BknError> {
        let state = self
            .state
            .read()
            .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
        Ok(ReadSnapshot {
            active_memtable: state.active_memtable.clone(),
            immutable_memtables: state.immutable_memtables.clone(),
            sstables: state.sstables.clone(),
        })
    }

    pub(super) fn commit_pending(&self, pending: Pending) -> Result<(), BknError> {
        self.ensure_writable()?;
        if pending.is_empty() {
            return Ok(());
        }

        // Never acknowledge a memtable that recovery would refuse to restore.
        // This matters for many tiny keys with a large flush threshold: their
        // allocation overhead is much larger than their encoded WAL payload.
        {
            let state = self
                .state
                .read()
                .map_err(|_| BknError::RecoveryRequired("state poisoned".into()))?;
            let size = projected_recovery_bytes(&state.active_memtable, &pending)?;
            if size > 256 * 1024 * 1024 {
                return Err(BknError::ResourceLimit(
                    "transaction would exceed the recovery memtable limit; checkpoint first".into(),
                ));
            }
        }

        let wal_bytes;
        let record = WalRecord {
            entries: pending
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        {
            let cw = self
                .container
                .lock()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
            if let Err(e) = wal::append(&cw.file, &record) {
                if matches!(e, BknError::CommitUnknown(_)) {
                    self.fail(e.clone());
                }
                return Err(e);
            }
            let end = cw
                .file
                .metadata()
                .map_err(|e| {
                    let err = BknError::CommitUnknown(e.to_string());
                    self.fail(err.clone());
                    err
                })?
                .len();
            let start = self
                .state
                .read()
                .map_err(|_| BknError::RecoveryRequired("state poisoned".into()))?
                .wal_region_start;
            wal_bytes = end.saturating_sub(start);
        }

        let needs_flush;
        {
            let mut state = self
                .state
                .write()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
            {
                // `Arc::make_mut`: cheap in-place mutation unless a reader
                // snapshot is currently outstanding, in which case it
                // clones first — clone-on-write, not clone-always.
                let map = Arc::make_mut(&mut state.active_memtable);
                for (key, value) in pending {
                    match value {
                        Some(bytes) => {
                            map.insert(key, LsmValue::Value(bytes));
                        }
                        None => {
                            map.insert(key, LsmValue::Tombstone);
                        }
                    }
                }
            }
            needs_flush = memtable_byte_size(&state.active_memtable)
                >= self.options.memtable_flush_bytes
                || wal_bytes >= self.options.memtable_flush_bytes as u64;
        }

        if needs_flush {
            if let Err(error) = self.flush() {
                // WAL commit is already acknowledged. Preserve that outcome and
                // stop further operations until recovery reconciles maintenance.
                self.fail(error);
            }
        }
        Ok(())
    }

    /// Appends the flushed memtable as a new SSTable blob and a new
    /// manifest blob at the container file's current end, one `sync_data`
    /// covering both, then a header write pointing at the new manifest —
    /// that header write is the actual commit point for the flush. Old WAL
    /// bytes before the new `wal_region_start` are left as dead space,
    /// reclaimed only by the next compaction (see `compact_all`), per the
    /// confirmed "compaction is also the vacuum point" design.
    pub(super) fn flush(&self) -> Result<(), BknError> {
        let memtable_to_flush;
        let generation;
        {
            let mut state = self
                .state
                .write()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
            if state.active_memtable.is_empty() {
                return Ok(());
            }
            memtable_to_flush = state.active_memtable.clone();
            state.immutable_memtables.push(memtable_to_flush.clone());
            state.active_memtable = Arc::new(Memtable::new());
            generation = state.next_sstable_id;
            state.next_sstable_id = state
                .next_sstable_id
                .checked_add(1)
                .ok_or_else(|| BknError::ResourceLimit("generation exhausted".into()))?;
        }

        let new_handle;
        let new_wal_region_start;
        let new_manifest_len;
        {
            let mut cw = self
                .container
                .lock()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;

            wal::fence(&cw.file)?;
            let entries = memtable_to_flush
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.clone())));
            let handle = SstableHandle::write(
                &cw.file,
                generation,
                entries,
                memtable_to_flush.len(),
                self.write_options(),
            )?;

            let (mut sstable_refs, next_sstable_id) = {
                let state = self
                    .state
                    .read()
                    .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
                let refs: Vec<SstableRef> = state
                    .sstables
                    .iter()
                    .map(|s| SstableRef {
                        generation: s.generation,
                        offset: s.base_offset,
                        length: s.blob_len,
                    })
                    .collect();
                (refs, state.next_sstable_id)
            };
            sstable_refs.push(SstableRef {
                generation: handle.generation,
                offset: handle.base_offset,
                length: handle.blob_len,
            });

            let manifest_offset = handle.base_offset + handle.blob_len;
            let placeholder = Manifest {
                next_sstable_id,
                sstables: sstable_refs,
                wal_region_start: 0,
            };
            let len = placeholder.encode()?.len() as u64;
            let manifest = Manifest {
                wal_region_start: manifest_offset + len,
                ..placeholder
            };
            let bytes = manifest.encode()?;
            container::write_blob_at(&cw.file, manifest_offset, &bytes)?;
            cw.file.sync_data().map_err(io_err)?;
            let (seq, slot) = container::write_header(
                &cw.file,
                cw.header_seq,
                cw.header_slot,
                manifest_offset,
                &bytes,
            )?;
            // Both roots must describe this checkpoint before another commit:
            // otherwise bitrot of the newer root could hide later WAL commits
            // behind the older checkpoint's maintenance fence.
            let (seq, slot) =
                container::write_header(&cw.file, seq, slot, manifest_offset, &bytes)?;
            cw.header_seq = seq;
            cw.header_slot = slot;

            new_handle = handle;
            new_wal_region_start = manifest.wal_region_start;
            new_manifest_len = bytes.len() as u64;
        }

        let should_compact;
        {
            let mut state = self
                .state
                .write()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
            state.sstables.push(Arc::new(new_handle));
            if let Some(pos) = state
                .immutable_memtables
                .iter()
                .position(|m| Arc::ptr_eq(m, &memtable_to_flush))
            {
                state.immutable_memtables.remove(pos);
            }
            state.wal_region_start = new_wal_region_start;
            state.manifest_len = new_manifest_len;
            should_compact = state.sstables.len() >= self.options.compaction_trigger_files;
        }

        if should_compact {
            self.compact_all(false)?;
        }
        Ok(())
    }

    /// Merges every current SSTable generation into one new SSTable blob,
    /// writing it into a **brand-new file** (header + merged blob + a
    /// manifest referencing just that one SSTable + an empty WAL region)
    /// rather than appending to the live file, then atomically renaming
    /// that new file over the live path. This is also the vacuum step: the
    /// new file only contains live data, so it's smaller than the file it
    /// replaces whenever there was dead space to reclaim — and it's written
    /// in the current SSTable format, which upgrades legacy files.
    ///
    /// The merge streams: each input SSTable is read one block at a time
    /// and the output is written block by block, so memory stays flat no
    /// matter how large the database is.
    ///
    /// Because this always merges *all* generations at once (size-tiered,
    /// "compact everything past N files" rather than a partial/leveled
    /// merge), no older generation is ever left outside the batch that
    /// could still need a tombstone to shadow it — so dropping tombstones
    /// in the merge is always safe here.
    ///
    /// Must be called only while `writer_lock` is already held by the
    /// caller (either inherited from a write-tx guard via `flush()`, or
    /// acquired explicitly by `force_compact()`) — it does not acquire the
    /// lock itself, to avoid deadlocking when `flush()` calls it.
    ///
    /// A `ReadSnapshot` taken before this call holds `Arc<SstableHandle>`s
    /// that each independently own their own `Arc<File>`, captured at
    /// construction time and fully decoupled from `self.path`. Reading
    /// from them after the rename below is safe and correct: on POSIX, an
    /// open fd to an unlinked/renamed-away inode keeps working until
    /// closed; on Windows, `FILE_SHARE_DELETE` (set when every container
    /// file handle is opened, see `container.rs`) lets the rename proceed
    /// while such handles remain open, and NTFS defers freeing the old
    /// file's storage until the last handle to it closes.
    ///
    /// The automatic call from `flush` (`force == false`) only merges once
    /// there are at least two SSTables. `force_compact` rewrites even a lone
    /// SSTable: the point there is also to reclaim dead WAL/manifest space
    /// and upgrade legacy-format data, which one SSTable can still carry.
    pub(super) fn compact_all(&self, force: bool) -> Result<(), BknError> {
        let sstables_to_merge;
        {
            let state = self
                .state
                .read()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
            let worth_it = state.sstables.len() >= 2 || (force && !state.sstables.is_empty());
            if !worth_it {
                return Ok(());
            }
            sstables_to_merge = state.sstables.clone();
        }

        // Newest generation first, as `MergeIter` expects.
        let sources: Vec<EntryIter<'_>> = sstables_to_merge
            .iter()
            .rev()
            .map(|sst| Box::new(sst.cursor(Bound::Unbounded, Bound::Unbounded)) as EntryIter<'_>)
            .collect();
        let expected: u64 = sstables_to_merge
            .iter()
            .map(|s| s.estimated_entries())
            .sum();

        let new_generation = {
            let mut state = self
                .state
                .write()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
            let id = state.next_sstable_id;
            state.next_sstable_id = state
                .next_sstable_id
                .checked_add(1)
                .ok_or_else(|| BknError::ResourceLimit("generation exhausted".into()))?;
            id
        };

        let tmp_path = compaction_tmp_path(&self.path);
        let fresh = write_fresh_container(
            self.io.as_ref(),
            &tmp_path,
            new_generation,
            MergeIter::new(sources),
            expected as usize,
            self.write_options(),
        )?;

        let temporary_bytes = fresh.file.metadata().map_err(io_err)?.len();
        let source_bytes = self
            .container
            .lock()
            .map_err(|_| BknError::RecoveryRequired("container poisoned".into()))?
            .file
            .metadata()
            .map_err(io_err)?
            .len();
        self.compaction_temp_peak
            .fetch_max(temporary_bytes, Ordering::Relaxed);
        self.compaction_disk_peak.fetch_max(
            source_bytes.saturating_add(temporary_bytes),
            Ordering::Relaxed,
        );
        self.io.replace(&tmp_path, &self.path).map_err(io_err)?;
        self.io.sync_parent(&self.path).map_err(io_err)?;

        {
            let mut cw = self
                .container
                .lock()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
            cw.file = fresh.file.clone();
            cw.header_seq = 2;
            cw.header_slot = 1;
        }
        {
            let mut state = self
                .state
                .write()
                .map_err(|_| BknError::RecoveryRequired("engine mutex poisoned".into()))?;
            state.sstables = vec![Arc::new(fresh.handle)];
            state.wal_region_start = fresh.manifest.wal_region_start;
            state.next_sstable_id = fresh.manifest.next_sstable_id;
            state.manifest_len = fresh.manifest_len;
        }
        Ok(())
    }
}

fn projected_recovery_bytes(active: &Memtable, pending: &Pending) -> Result<u64, BknError> {
    let footprint = |k: &[u8], value_len: usize| -> Result<u64, BknError> {
        (k.len() as u64)
            .checked_add(value_len as u64)
            .and_then(|n| n.checked_add(64))
            .ok_or_else(|| BknError::ResourceLimit("memtable size overflow".into()))
    };
    let mut size = 0u64;
    for (k, v) in active {
        size = size
            .checked_add(footprint(
                k,
                match v {
                    LsmValue::Value(v) => v.len(),
                    LsmValue::Tombstone => 0,
                },
            )?)
            .ok_or_else(|| BknError::ResourceLimit("memtable size overflow".into()))?;
    }
    for (k, v) in pending {
        if let Some(old) = active.get(k) {
            size -= footprint(
                k,
                match old {
                    LsmValue::Value(v) => v.len(),
                    LsmValue::Tombstone => 0,
                },
            )?;
        }
        size = size
            .checked_add(footprint(k, v.as_ref().map_or(0, Vec::len))?)
            .ok_or_else(|| BknError::ResourceLimit("memtable size overflow".into()))?;
    }
    Ok(size)
}
#[cfg(test)]
mod accounting_tests {
    use super::*;
    #[test]
    fn prospective_accounting_matches_the_applied_transaction() {
        let mut active = Memtable::new();
        active.insert(b"a".to_vec(), LsmValue::Value(vec![7; 100]));
        active.insert(b"b".to_vec(), LsmValue::Tombstone);
        let pending = Pending::from([
            (b"a".to_vec(), None),
            (b"b".to_vec(), Some(vec![1; 10])),
            (b"c".to_vec(), None),
        ]);
        let estimated = projected_recovery_bytes(&active, &pending).unwrap();
        for (k, v) in pending {
            active.insert(k, v.map_or(LsmValue::Tombstone, LsmValue::Value));
        }
        let actual: u64 = active
            .iter()
            .map(|(k, v)| {
                64 + k.len() as u64
                    + match v {
                        LsmValue::Value(v) => v.len() as u64,
                        LsmValue::Tombstone => 0,
                    }
            })
            .sum();
        assert_eq!(estimated, actual);
    }
}
